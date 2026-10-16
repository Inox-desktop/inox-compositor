//! Smithay compositor state and protocol handlers for the M1 milestone.
//!
//! M1 advertises the `wl_output`, base `wl_compositor` and private `inox_ipc`
//! globals. Rendering of client surfaces, seats and window management arrive in
//! later milestones.
//!
//! The Wayland bindings come from `inox-protocols`: the curated core `wl_*` types
//! and the `inox-ipc` control protocol, so upstream versions are pinned in one
//! place (see the workspace `AGENTS.md`).

use std::sync::OnceLock;

use calloop::LoopSignal;
use inox_protocols::{
    ipc::inox_ipc::server::inox_ipc::{InoxIpc, ReloadResult, Request as InoxIpcRequest},
    standard::core::server::wl_surface::WlSurface,
};
use smithay::{
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        backend::{ClientData, ClientId, DisconnectReason},
    },
    utils::{Point, Transform},
    wayland::{
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        output::{OutputHandler, OutputManagerState},
    },
};
use tracing::{debug, info, warn};

use crate::{
    config::CompositorConfig,
    render::{HeadlessRenderer, RenderError},
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
    renderer: HeadlessRenderer,
    frames_rendered: u64,
    exit_after_frames: Option<u64>,
    exit_requested: bool,
    loop_signal: LoopSignal,
}

impl InoxState {
    /// Build the state, the headless output and the render target.
    ///
    /// # Errors
    ///
    /// Returns [`StateError::Renderer`] when the EGL/GLES renderer cannot be
    /// created, which happens when no usable GL driver is available.
    pub fn new(
        display_handle: &DisplayHandle,
        config: &CompositorConfig,
        loop_signal: LoopSignal,
    ) -> Result<Self, StateError> {
        let compositor_state = CompositorState::new::<Self>(display_handle);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(display_handle);
        let output = create_headless_output(config, display_handle);
        let renderer = HeadlessRenderer::new(config.output_size())?;

        let ipc_version = inox_ipc_version();
        let ipc_global = display_handle.create_global::<Self, InoxIpc, ()>(ipc_version, ());
        debug!(
            ?ipc_global,
            version = ipc_version,
            "advertised inox_ipc global"
        );

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
            renderer,
            frames_rendered: 0,
            exit_after_frames: config.exit_after_frames,
            exit_requested: false,
            loop_signal,
        })
    }

    /// Render one frame of the headless output.
    ///
    /// # Errors
    ///
    /// Returns a [`RenderError`] when the GLES rendering fails.
    pub fn render_frame(&mut self) -> Result<(), RenderError> {
        self.renderer.render_solid()?;
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
        Some(Scale::Integer(1)),
        Some(Point::from((0, 0))),
    );

    // The global lives as long as the display does, and M1 never removes outputs, so
    // the handle is only reported and then dropped.
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
#[derive(Default)]
pub(crate) struct ClientState {
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

    fn commit(&mut self, _surface: &WlSurface) {
        // M1 renders no client surfaces; surface commits become meaningful in M2.
    }
}

/// The `inox_ipc` protocol version the compositor advertises and answers with.
///
/// Read from the interface generated in `inox-protocols` so the reported version
/// can never drift from the protocol definition.
fn inox_ipc_version() -> u32 {
    <InoxIpc as Resource>::interface().version
}

/// Service the private `inox-ipc` control protocol.
///
/// M1 only has to prove the compositor genuinely consumes `inox-protocols`, so the
/// handler is deliberately small: `get_version` reports the protocol version, and
/// `reload_config` is answered with [`ReloadResult::Unsupported`] because
/// configuration reloading arrives in a later milestone. Answering explicitly lets
/// the shell tell "not implemented yet" apart from a transport failure.
impl Dispatch<InoxIpc, ()> for InoxState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        resource: &InoxIpc,
        request: InoxIpcRequest,
        _data: &(),
        _display_handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            // `destroy` is a destructor: wayland-server removes the object once the
            // request has been dispatched, so there is nothing to clean up here.
            InoxIpcRequest::Destroy => {}
            InoxIpcRequest::GetVersion => resource.version(inox_ipc_version()),
            InoxIpcRequest::ReloadConfig => resource.config_reloaded(ReloadResult::Unsupported),
            // `Request` is `#[non_exhaustive]`: a later protocol revision may add
            // requests, which this milestone ignores rather than second-guessing.
            _ => {}
        }
    }
}

impl GlobalDispatch<InoxIpc, ()> for InoxState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<InoxIpc>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let ipc = data_init.init(resource, ());

        // The protocol asks the compositor to report its version as soon as the
        // global is bound, so a client does not have to send `get_version` first.
        ipc.version(inox_ipc_version());
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
