//! Integration test for the M2 acceptance criteria: the compositor handles
//! keyboard and pointer events and manages more than one client window, tiling
//! them through the desktop space.
//!
//! The test drives a real Wayland client (built with `wayland-client`) over an
//! in-process socket pair against a compositor state on the current thread. The
//! client runs on a second thread while the main thread services the display, so
//! no process boundary is needed (see ADR 0006).
//!
//! Everything lives in a single test function on purpose: a process may only ever
//! hold one EGL display (see the `SAFETY` note in `src/render.rs`), and test
//! functions inside one binary run in parallel threads.

use std::{
    os::unix::net::UnixStream,
    sync::{
        Arc,
        mpsc::{Receiver, Sender, channel},
    },
    thread,
    time::{Duration, Instant},
};

use calloop::EventLoop;
use inox_compositor::{ClientState, CompositorConfig, InoxState};
use smithay::{
    backend::input::{ButtonState, KeyState},
    input::keyboard::Keycode,
    reexports::wayland_server::Display,
    utils::{Logical, Point},
};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_compositor, wl_registry, wl_surface},
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

/// How long to service the display before declaring that the client never got
/// through. Generous enough for a busy CI machine.
const DISPATCH_TIMEOUT: Duration = Duration::from_secs(20);

/// `evdev` key codes (XKB key codes are the evdev scancode plus 8).
const KEY_LEFT_ALT: u32 = 56 + 8;
const KEY_LEFT_META: u32 = 125 + 8;
const KEY_TAB: u32 = 15 + 8;
const KEY_Q: u32 = 16 + 8;

/// The client's application state. It only needs the registry dispatch, because
/// the test never reads events after the initial roundtrip.
struct Client;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Client {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // The `GlobalList` records the globals; the test only needs to bind them.
    }
}

wayland_client::delegate_noop!(Client: ignore wl_compositor::WlCompositor);
wayland_client::delegate_noop!(Client: ignore wl_surface::WlSurface);
wayland_client::delegate_noop!(Client: ignore xdg_wm_base::XdgWmBase);
wayland_client::delegate_noop!(Client: ignore xdg_surface::XdgSurface);
wayland_client::delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);

/// Create an `xdg_toplevel` on its own `wl_surface` and commit it.
fn create_toplevel(
    compositor: &wl_compositor::WlCompositor,
    xdg: &xdg_wm_base::XdgWmBase,
    queue: &QueueHandle<Client>,
) -> (
    wl_surface::WlSurface,
    xdg_surface::XdgSurface,
    xdg_toplevel::XdgToplevel,
) {
    let surface = compositor.create_surface(queue, ());
    let xdg_surface = xdg.get_xdg_surface(&surface, queue, ());
    let toplevel = xdg_surface.get_toplevel(queue, ());
    surface.commit();

    (surface, xdg_surface, toplevel)
}

/// Service the display once: dispatch client requests and flush outgoing events.
fn service(display: &mut Display<InoxState>, state: &mut InoxState) {
    // Errors during teardown (a peer that went away) are not interesting here; the
    // state assertions below are what the test is about.
    let _ = display.dispatch_clients(state);
    let _ = display.flush_clients();
}

/// Wait until the compositor manages at least `count` windows.
fn wait_for_windows(display: &mut Display<InoxState>, state: &mut InoxState, count: usize) {
    let deadline = Instant::now() + DISPATCH_TIMEOUT;

    while state.window_count() < count {
        service(display, state);

        if state.window_count() >= count {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out after {DISPATCH_TIMEOUT:?} with {} windows managed",
            state.window_count()
        );

        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn manages_client_windows_and_input() {
    let event_loop: EventLoop<'static, ()> =
        EventLoop::try_new().expect("failed to create the event loop");
    let mut display: Display<InoxState> =
        Display::new().expect("failed to create the wayland display");
    let mut display_handle = display.handle();

    let mut state = InoxState::new(
        &display_handle,
        &CompositorConfig::default(),
        event_loop.get_signal(),
    )
    .expect("failed to create the compositor state");

    let (server_stream, client_stream) =
        UnixStream::pair().expect("failed to create the socket pair");
    display_handle
        .insert_client(server_stream, Arc::new(ClientState::default()))
        .expect("failed to register the client");

    let (client, stop_tx): (thread::JoinHandle<()>, Sender<()>) =
        spawn_client(client_stream, &mut display, &mut state);

    // --- window management -------------------------------------------------

    assert_eq!(state.window_count(), 2, "both toplevels should be managed");

    let windows = state.managed_windows().to_vec();
    assert_eq!(windows.len(), 2);

    // The most recently mapped window is the layout master.
    let master = windows[0];
    let stack = windows[1];

    // Default tiling at 1280x720 with a 60% master column and an 8px gap:
    // the master column starts at x = 8 with width 758, the stack column starts
    // at x = 774 with width 498.
    assert_eq!(
        state.window_location(master),
        Some(Point::from((8, 8))),
        "the master window should sit at the top-left of the work area"
    );
    assert_eq!(
        state.window_location(stack),
        Some(Point::from((774, 8))),
        "the stack window should sit right of the master column"
    );

    // --- input events ------------------------------------------------------

    // Pointer motion is tracked and delivered without a client focus.
    let pointer = Point::<f64, Logical>::from((100.0, 120.0));
    state.inject_pointer_motion(pointer);
    assert_eq!(state.pointer_location(), pointer);

    state.inject_pointer_button(0x110, ButtonState::Pressed);
    state.inject_pointer_button(0x110, ButtonState::Released);

    // Alt+Tab cycles the keyboard focus forward. The focused window starts as the
    // master (the newest window); one step lands on the stack window.
    assert_eq!(state.focused_window(), Some(master));
    assert_eq!(
        state.inject_key(Keycode::new(KEY_LEFT_ALT), KeyState::Pressed),
        None
    );
    assert_eq!(
        state.inject_key(Keycode::new(KEY_TAB), KeyState::Pressed),
        Some(inox_compositor::Action::FocusNext)
    );
    assert_eq!(state.focused_window(), Some(stack));

    state.inject_key(Keycode::new(KEY_TAB), KeyState::Released);
    state.inject_key(Keycode::new(KEY_LEFT_ALT), KeyState::Released);

    // Super+Q asks the compositor to shut down cleanly.
    assert_eq!(
        state.inject_key(Keycode::new(KEY_LEFT_META), KeyState::Pressed),
        None
    );
    assert_eq!(
        state.inject_key(Keycode::new(KEY_Q), KeyState::Pressed),
        Some(inox_compositor::Action::Quit)
    );
    assert!(state.exit_requested());

    state.inject_key(Keycode::new(KEY_Q), KeyState::Released);
    state.inject_key(Keycode::new(KEY_LEFT_META), KeyState::Released);

    // Release the client so it drops its protocol objects, then let it finish.
    let _ = stop_tx.send(());
    client.join().expect("the client thread panicked");
}

/// Start the client thread and block the current thread until two windows exist.
///
/// The client finishes its registry roundtrip and toplevel setup, then waits to be
/// released; keeping it alive holds the protocol objects (and their destroy
/// requests) back until the assertions have run.
fn spawn_client(
    client_stream: UnixStream,
    display: &mut Display<InoxState>,
    state: &mut InoxState,
) -> (thread::JoinHandle<()>, Sender<()>) {
    let (stop_tx, stop_rx): (Sender<()>, Receiver<()>) = channel();

    let handle = thread::spawn(move || {
        let connection = Connection::from_socket(client_stream).expect("failed to connect");

        let (globals, event_queue) =
            registry_queue_init::<Client>(&connection).expect("failed to initialise the registry");
        let queue = event_queue.handle();

        let compositor: wl_compositor::WlCompositor = globals
            .bind(&queue, 1..=4, ())
            .expect("the compositor has no wl_compositor global");
        let xdg: xdg_wm_base::XdgWmBase = globals
            .bind(&queue, 1..=6, ())
            .expect("the compositor has no xdg_wm_base global");

        let first = create_toplevel(&compositor, &xdg, &queue);
        let second = create_toplevel(&compositor, &xdg, &queue);
        connection.flush().expect("failed to flush the client");

        // Stay alive, and keep every proxy alive, until the main thread is done.
        let _ = stop_rx.recv();

        drop((first, second, xdg, compositor, event_queue, connection));
    });

    wait_for_windows(display, state, 2);

    // Process any requests that arrived after the window count was reached so the
    // layout is fully applied.
    for _ in 0..5 {
        service(display, state);
    }

    (handle, stop_tx)
}
