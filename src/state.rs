//! Smithay compositor state and protocol handlers.
//!
//! The compositor advertises the `wl_output`, `wl_compositor`, `wl_shm`,
//! `wl_seat` and `xdg_wm_base` globals. Client toplevels become managed windows:
//! they are registered in a [`Workspace`], tiled by [`LayoutConfig`] and mapped
//! into a Smithay desktop `Space` that [`InoxState::render_frame`] composites.
//!
//! Input handling is split in two: Smithay's seat owns the keymap, modifier
//! tracking and focus delivery, while [`crate::input::action_for_key`] turns a
//! key press into a compositor [`Action`]. [`InoxState::inject_key`] and the
//! pointer injection methods feed that path, so keyboard and pointer events can
//! be exercised without a libinput backend (see ADR 0006).

use std::{borrow::Cow, collections::HashMap, sync::OnceLock};

use calloop::LoopSignal;
use smithay::{
    backend::{
        input::{ButtonState, KeyState},
        renderer::utils::on_commit_buffer_handler,
    },
    desktop::{Space, Window, WindowSurfaceType},
    input::{
        Seat, SeatHandler, SeatState,
        keyboard::{FilterResult, KeyboardHandle, Keycode, XkbConfig},
        pointer::{ButtonEvent, CursorImageStatus, MotionEvent, PointerHandle},
    },
    output::{Mode, Output, PhysicalProperties, Scale as OutputScale, Subpixel},
    reexports::wayland_server::{
        Client, DisplayHandle,
        backend::{ClientData, ClientId, DisconnectReason},
        protocol::{wl_buffer::WlBuffer, wl_seat::WlSeat, wl_surface::WlSurface},
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Serial, Size, Transform},
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        output::{OutputHandler, OutputManagerState},
        seat::WaylandFocus,
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
        },
        shm::{ShmHandler, ShmState},
    },
};
use tracing::{debug, info, warn};

use crate::{
    config::CompositorConfig,
    input::{Action, action_for_key},
    layout::LayoutConfig,
    render::{HeadlessRenderer, RenderError},
    workspace::{WindowId, Workspace},
};

/// Physical pixels assumed per inch when translating the headless mode into
/// millimetres for the advertised `wl_output` geometry.
const PIXELS_PER_INCH: i32 = 96;

/// Millimetres per inch, scaled by ten to keep the conversion in integer arithmetic.
const MILLIMETRES_PER_INCH_X10: i32 = 254;

/// Errors produced while building the compositor state.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// The headless renderer could not be created.
    #[error("failed to create the headless renderer")]
    Renderer(#[from] RenderError),

    /// The seat keyboard could not be created, which happens when no XKB keymap
    /// is available (for example when the `xkb-data` package is missing).
    #[error("failed to create the seat keyboard")]
    Keyboard(#[from] smithay::input::keyboard::Error),
}

/// The compositor state, shared with every Smithay protocol handler.
pub struct InoxState {
    compositor_state: CompositorState,

    /// Owns the `zxdg_output_manager_v1` global advertised next to `wl_output`.
    ///
    /// Smithay's output dispatch is generic over the compositor state, so this value
    /// is never read: the compositor keeps it to own the global it created.
    #[allow(dead_code)]
    output_manager_state: OutputManagerState,

    output: Output,

    seat_state: SeatState<Self>,
    seat: Seat<Self>,
    keyboard: KeyboardHandle<Self>,
    pointer: PointerHandle<Self>,

    xdg_shell_state: XdgShellState,
    shm_state: ShmState,

    /// The desktop space that owns the managed windows and their positions.
    space: Space<Window>,
    /// The tiling order and focus of the managed windows.
    workspace: Workspace,
    /// Maps workspace identifiers to the Smithay windows they stand for.
    windows: HashMap<WindowId, Window>,

    /// Tiling parameters, applied whenever the window set changes.
    layout: LayoutConfig,
    /// Rectangle the windows are tiled into, derived from the output mode.
    work_area: Rectangle<i32, Logical>,
    /// Last location reported for the pointer, in workspace coordinates.
    pointer_location: Point<f64, Logical>,

    renderer: HeadlessRenderer,
    frames_rendered: u64,
    exit_after_frames: Option<u64>,
    exit_requested: bool,
    loop_signal: LoopSignal,
}

impl InoxState {
    /// Build the state, the headless output, the seat and the render target.
    ///
    /// # Errors
    ///
    /// Returns [`StateError::Renderer`] when the EGL/GLES renderer cannot be
    /// created, which happens when no usable GL driver is available, and
    /// [`StateError::Keyboard`] when the XKB keymap cannot be compiled.
    pub fn new(
        display_handle: &DisplayHandle,
        config: &CompositorConfig,
        loop_signal: LoopSignal,
    ) -> Result<Self, StateError> {
        let compositor_state = CompositorState::new::<Self>(display_handle);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(display_handle);
        let output = create_headless_output(config, display_handle);
        let renderer = HeadlessRenderer::new(config.output_size())?;

        let xdg_shell_state = XdgShellState::new::<Self>(display_handle);
        let shm_state = ShmState::new::<Self>(display_handle, []);

        let mut seat_state = SeatState::<Self>::new();
        let mut seat = seat_state.new_wl_seat(display_handle, "seat-0");
        let keyboard = seat.add_keyboard(keyboard_config(), 200, 25)?;
        let pointer = seat.add_pointer();

        let mut space = Space::default();
        space.map_output(&output, (0, 0));

        let work_area = Rectangle::from_size(Size::from(config.output_size()));

        info!(
            output = %output.name(),
            width = config.width,
            height = config.height,
            refresh_hz = config.refresh_hz,
            "created headless output"
        );

        Ok(Self {
            compositor_state,
            output_manager_state,
            output,
            seat_state,
            seat,
            keyboard,
            pointer,
            xdg_shell_state,
            shm_state,
            space,
            workspace: Workspace::new(),
            windows: HashMap::new(),
            layout: LayoutConfig::default(),
            work_area,
            pointer_location: Point::from((0.0, 0.0)),
            renderer,
            frames_rendered: 0,
            exit_after_frames: config.exit_after_frames,
            exit_requested: false,
            loop_signal,
        })
    }

    /// Render one frame of the headless output.
    ///
    /// The desktop space is refreshed so that closed windows are dropped, the
    /// render elements for the whole work area are produced and drawn over the
    /// background colour.
    ///
    /// # Errors
    ///
    /// Returns a [`RenderError`] when the GLES rendering fails.
    pub fn render_frame(&mut self) -> Result<(), RenderError> {
        self.space.refresh();

        let elements = self.space.render_elements_for_region(
            self.renderer.renderer_mut(),
            &self.work_area,
            1.0f64,
            1.0,
        );
        self.renderer.render_elements(&elements)?;

        self.frames_rendered = self.frames_rendered.saturating_add(1);
        Ok(())
    }

    /// Number of frames rendered so far.
    pub fn frames_rendered(&self) -> u64 {
        self.frames_rendered
    }

    /// Whether the configured frame budget has been rendered.
    ///
    /// Always `false` when no budget was configured.
    pub fn frame_budget_reached(&self) -> bool {
        budget_reached(self.exit_after_frames, self.frames_rendered)
    }

    /// Read back the centre pixel of the render target, as RGBA bytes.
    ///
    /// Exposed so that headless integration tests can verify that a frame really
    /// reached the render target.
    ///
    /// # Errors
    ///
    /// Returns a [`RenderError`] when the readback fails.
    pub fn read_frame_center_pixel(&mut self) -> Result<[u8; 4], RenderError> {
        self.renderer.read_center_pixel()
    }

    /// The headless output advertised to clients.
    pub fn output(&self) -> &Output {
        &self.output
    }

    /// The seat advertised to clients, carrying the keyboard and pointer capabilities.
    pub fn seat(&self) -> &Seat<Self> {
        &self.seat
    }

    /// Whether a clean shutdown has been requested.
    pub fn exit_requested(&self) -> bool {
        self.exit_requested
    }

    /// Ask the event loop to stop after the current iteration.
    ///
    /// Repeated requests are ignored so that a second signal cannot restart the
    /// shutdown sequence.
    pub fn request_exit(&mut self) {
        if !self.exit_requested {
            self.exit_requested = true;
            self.loop_signal.stop();
        }
    }

    /// The managed windows in tiling order, master first.
    pub fn managed_windows(&self) -> &[WindowId] {
        self.workspace.windows()
    }

    /// Number of managed windows.
    pub fn window_count(&self) -> usize {
        self.workspace.len()
    }

    /// The focused window, if any.
    pub fn focused_window(&self) -> Option<WindowId> {
        self.workspace.focused()
    }

    /// The tiling parameters in use.
    pub fn layout(&self) -> LayoutConfig {
        self.layout
    }

    /// The surface currently holding the keyboard focus, if any.
    pub fn keyboard_focus(&self) -> Option<WlSurface> {
        self.keyboard.current_focus()
    }

    /// The last location reported for the pointer, in workspace coordinates.
    pub fn pointer_location(&self) -> Point<f64, Logical> {
        self.pointer_location
    }

    /// The location a managed window is mapped at inside the workspace, if any.
    pub fn window_location(&self, id: WindowId) -> Option<Point<i32, Logical>> {
        let window = self.windows.get(&id)?;
        self.space.element_location(window)
    }

    /// Feed a key press or release into the seat.
    ///
    /// Returns the compositor [`Action`] the key resolved to, or `None` when the
    /// event was not bound and was forwarded to the focused client.
    pub fn inject_key(&mut self, keycode: Keycode, state: KeyState) -> Option<Action> {
        let keyboard = self.keyboard.clone();
        let serial = SERIAL_COUNTER.next_serial();

        let action = keyboard.input(self, keycode, state, serial, 0, |_, modifiers, keysym| {
            match action_for_key(keysym.modified_sym(), modifiers) {
                Some(action) => FilterResult::Intercept(action),
                None => FilterResult::Forward,
            }
        });

        if let Some(action) = action {
            self.perform(action);
        }

        action
    }

    /// Move the pointer to `location` and deliver the resulting enter/motion events.
    pub fn inject_pointer_motion(&mut self, location: Point<f64, Logical>) {
        self.pointer_location = location;
        let focus = self.surface_under(location);

        let pointer = self.pointer.clone();
        let serial = SERIAL_COUNTER.next_serial();
        let event = MotionEvent {
            location,
            serial,
            time: 0,
        };

        pointer.motion(self, focus, &event);
        pointer.frame(self);
    }

    /// Deliver a pointer button press or release at the current location.
    pub fn inject_pointer_button(&mut self, button: u32, state: ButtonState) {
        let pointer = self.pointer.clone();
        let serial = SERIAL_COUNTER.next_serial();
        let event = ButtonEvent {
            serial,
            time: 0,
            button,
            state,
        };

        pointer.button(self, &event);
        pointer.frame(self);
    }

    /// Apply a compositor action triggered by a key binding.
    fn perform(&mut self, action: Action) {
        match action {
            Action::FocusNext => {
                self.workspace.focus_next();
                self.sync_focus();
            }
            Action::FocusPrev => {
                self.workspace.focus_prev();
                self.sync_focus();
            }
            Action::Quit => self.request_exit(),
        }
    }

    /// Register a freshly created toplevel as a managed window.
    fn add_window(&mut self, surface: ToplevelSurface) {
        let id = self.workspace.add();
        self.windows.insert(id, Window::new_wayland_window(surface));

        info!(
            id = id.get(),
            windows = self.workspace.len(),
            "mapped toplevel"
        );

        self.arrange_windows();
        self.sync_focus();
    }

    /// Forget the window built from a destroyed toplevel.
    fn remove_window(&mut self, surface: &ToplevelSurface) {
        let Some(id) = self.window_id_for_surface(surface.wl_surface()) else {
            return;
        };

        if let Some(window) = self.windows.remove(&id) {
            self.space.unmap_elem(&window);
        }
        self.workspace.remove(id);

        info!(
            id = id.get(),
            windows = self.workspace.len(),
            "unmapped toplevel"
        );

        self.arrange_windows();
        self.sync_focus();
    }

    /// Identifier of the managed window built from `surface`, if any.
    fn window_id_for_surface(&self, surface: &WlSurface) -> Option<WindowId> {
        self.windows
            .iter()
            .find(|(_, window)| window_toplevel_surface(window) == Some(surface))
            .map(|(id, _)| *id)
    }

    /// Suggest each managed window its tiled geometry and map it into the space.
    fn arrange_windows(&mut self) {
        let tiles = self.workspace.arrange(self.work_area, self.layout);
        let mut mapped: Vec<Window> = Vec::with_capacity(tiles.len());

        for (id, tile) in &tiles {
            let Some(window) = self.windows.get(id).cloned() else {
                continue;
            };

            if let Some(toplevel) = window.toplevel() {
                let size = clamp_size(tile.size);
                toplevel.with_pending_state(|state| {
                    state.size = Some(size);
                });
                toplevel.send_pending_configure();
            }

            self.space.map_element(window.clone(), tile.loc, false);
            mapped.push(window);
        }

        let stale: Vec<Window> = self
            .space
            .elements()
            .filter(|window| !mapped.contains(window))
            .cloned()
            .collect();
        for window in stale {
            self.space.unmap_elem(&window);
        }

        self.space.refresh();
    }

    /// Point the keyboard focus at the focused window and activate it in the space.
    fn sync_focus(&mut self) {
        let focused = self
            .workspace
            .focused()
            .and_then(|id| self.windows.get(&id))
            .and_then(|window| window.wl_surface())
            .map(Cow::into_owned);

        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, focused, serial);

        if let Some(id) = self.workspace.focused() {
            if let Some(window) = self.windows.get(&id).cloned() {
                self.space.raise_element(&window, true);
            }
        }
    }

    /// The surface under a pointer location, together with its absolute position.
    fn surface_under(
        &self,
        location: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let (window, window_location) = self.space.element_under(location)?;
        let relative = location - window_location.to_f64();
        let (surface, surface_location) = window.surface_under(relative, WindowSurfaceType::ALL)?;
        let absolute = (window_location + surface_location).to_f64();

        Some((surface, absolute))
    }
}

/// The toplevel surface backing `window`, if it is a Wayland window.
fn window_toplevel_surface(window: &Window) -> Option<&WlSurface> {
    window.toplevel().map(ToplevelSurface::wl_surface)
}

/// Coerce a tiled size into one the protocol accepts, which requires it to be positive.
fn clamp_size(size: Size<i32, Logical>) -> Size<i32, Logical> {
    Size::from((size.w.max(1), size.h.max(1)))
}

/// The XKB configuration used for the seat keyboard.
///
/// The layout is pinned rather than taken from the environment so that key codes
/// resolve identically everywhere, which keeps the key bindings deterministic.
fn keyboard_config() -> XkbConfig<'static> {
    XkbConfig {
        rules: "evdev",
        model: "pc105",
        layout: "us",
        variant: "",
        options: None,
    }
}

/// Create the headless output and advertise it to clients.
fn create_headless_output(config: &CompositorConfig, display_handle: &DisplayHandle) -> Output {
    let (width, height) = config.output_size();

    let output = Output::new(
        config.output_name.clone(),
        PhysicalProperties {
            size: (millimetres(width), millimetres(height)).into(),
            subpixel: Subpixel::Unknown,
            make: "Inox".to_owned(),
            model: "Headless".to_owned(),
        },
    );

    let mode = Mode {
        size: (width, height).into(),
        refresh: config.refresh_mhz(),
    };
    output.set_preferred(mode);
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        Some(OutputScale::Integer(1)),
        Some(Point::from((0, 0))),
    );

    // The global lives as long as the display does, and the compositor never removes
    // outputs, so the handle is only reported and then dropped.
    let global = output.create_global::<InoxState>(display_handle);
    debug!(?global, output = %output.name(), "advertised wl_output global");

    output
}

/// Convert a pixel dimension to millimetres at [`PIXELS_PER_INCH`].
fn millimetres(pixels: i32) -> i32 {
    pixels.saturating_mul(MILLIMETRES_PER_INCH_X10) / (PIXELS_PER_INCH * 10)
}

/// Whether `rendered` frames satisfy the configured `budget`.
///
/// A `None` budget means "run until signalled", so it is never reached.
fn budget_reached(budget: Option<u64>, rendered: u64) -> bool {
    matches!(budget, Some(budget) if rendered >= budget)
}

/// Per-client data attached to every Wayland client.
///
/// The compositor requires every client to be registered through
/// [`DisplayHandle::insert_client`] with this state; see
/// [`CompositorHandler::client_compositor_state`].
#[derive(Default)]
pub struct ClientState {
    compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, client_id: ClientId) {
        info!(?client_id, "wayland client connected");
    }

    fn disconnected(&self, client_id: ClientId, reason: DisconnectReason) {
        info!(?client_id, ?reason, "wayland client disconnected");
    }
}

impl OutputHandler for InoxState {}

impl CompositorHandler for InoxState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        match client.get_data::<ClientState>() {
            Some(client_state) => &client_state.compositor_state,
            None => fallback_client_state(),
        }
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);

        let Some(id) = self.window_id_for_surface(surface) else {
            return;
        };

        if let Some(window) = self.windows.get(&id) {
            // Refresh the bounding box the space uses for hit-testing and tiling.
            window.on_commit();
        }
    }
}

impl SeatHandler for InoxState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, focused: Option<&WlSurface>) {
        debug!(focused = focused.is_some(), "keyboard focus changed");
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {
        // M2 has no cursor theme; the pointer is tracked but not drawn.
    }
}

impl ShmHandler for InoxState {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl BufferHandler for InoxState {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {
        // Buffers are owned by the renderer's import cache and released there.
    }
}

impl XdgShellHandler for InoxState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        self.add_window(surface);
    }

    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {
        // M2 does not track popups; they are rendered through their parent window.
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {
        // Popup pointer grabs arrive with the popup milestone.
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        _positioner: PositionerState,
        token: u32,
    ) {
        // Acknowledge the reposition so the client stops waiting; the geometry is
        // unchanged because M2 does not place popups itself.
        surface.send_repositioned(token);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        self.remove_window(&surface);
    }
}

/// Fallback used when a client carries no [`ClientState`].
///
/// Every client is registered with a [`ClientState`] through
/// [`DisplayHandle::insert_client`], so this is unreachable in practice. Returning
/// a shared default keeps the compositor alive instead of panicking on an
/// unexpected client.
fn fallback_client_state() -> &'static CompositorClientState {
    static FALLBACK: OnceLock<CompositorClientState> = OnceLock::new();

    FALLBACK.get_or_init(|| {
        warn!("client without the Inox client state, using a shared default");
        CompositorClientState::default()
    })
}

smithay::delegate_compositor!(InoxState);
smithay::delegate_output!(InoxState);
smithay::delegate_seat!(InoxState);
smithay::delegate_xdg_shell!(InoxState);
smithay::delegate_shm!(InoxState);

#[cfg(test)]
mod tests {
    use super::budget_reached;

    #[test]
    fn without_a_budget_the_compositor_never_stops_by_itself() {
        assert!(!budget_reached(None, 0));
        assert!(!budget_reached(None, u64::MAX));
    }

    #[test]
    fn the_budget_is_reached_at_the_configured_frame_count() {
        assert!(!budget_reached(Some(3), 0));
        assert!(!budget_reached(Some(3), 2));
        assert!(budget_reached(Some(3), 3));
        assert!(budget_reached(Some(3), 4));
    }

    #[test]
    fn a_budget_of_one_frame_is_reached_after_the_first_frame() {
        assert!(!budget_reached(Some(1), 0));
        assert!(budget_reached(Some(1), 1));
    }
}
