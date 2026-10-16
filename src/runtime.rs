//! The compositor event loop.
//!
//! The loop is driven by calloop (see ADR 0003) and wires together four sources:
//! the Wayland listening socket, the Wayland display itself, the process signals
//! that request a clean shutdown, and the timer that drives frames.

use std::{ffi::OsString, io, sync::Arc, time::Duration};

use calloop::{
    EventLoop, Interest, Mode, PostAction,
    generic::{Generic, NoIoDrop},
    signals::{Signal, Signals},
    timer::{TimeoutAction, Timer},
};
use smithay::{
    reexports::wayland_server::{BindError, Display, DisplayHandle, backend::InitError},
    wayland::socket::ListeningSocketSource,
};
use tracing::{debug, info, warn};

use crate::{
    config::CompositorConfig,
    state::{ClientState, InoxState, StateError},
};

/// Errors that abort the compositor event loop.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The calloop event loop could not be created.
    #[error("failed to create the event loop")]
    EventLoop(#[from] calloop::Error),

    /// The Wayland display could not be created.
    #[error("failed to create the wayland display")]
    Display(#[from] InitError),

    /// The Wayland socket could not be bound.
    #[error("failed to bind the wayland socket")]
    Socket(#[from] BindError),

    /// The compositor state could not be created.
    #[error("failed to create the compositor state")]
    State(#[from] StateError),

    /// A calloop event source could not be registered.
    ///
    /// The underlying [`calloop::InsertError`] is kept as a rendered reason because it
    /// is not `Send + Sync` for every source type.
    #[error("failed to register the {name} event source: {reason}")]
    EventSource {
        /// Which event source failed to register.
        name: &'static str,
        /// The rendered registration error.
        reason: String,
    },
}

/// State shared with every calloop event source.
struct CalloopData {
    state: InoxState,
    display_handle: DisplayHandle,
}

/// Run the compositor until it is signalled to exit or its frame budget is reached.
///
/// Returns `Ok(())` after a clean shutdown, which is the M1 acceptance criterion:
/// termination signals must lead to exit status 0 rather than killing the process.
///
/// # Errors
///
/// Returns a [`RunError`] when the event loop, the Wayland display, the listening
/// socket or an event source cannot be set up, or when the compositor state cannot
/// be built.
pub fn run(config: CompositorConfig) -> Result<(), RunError> {
    let mut event_loop: EventLoop<'static, CalloopData> = EventLoop::try_new()?;
    let loop_handle = event_loop.handle();
    let loop_signal = event_loop.get_signal();

    let display: Display<InoxState> = Display::new()?;
    let display_handle = display.handle();

    let state = InoxState::new(&display_handle, &config, loop_signal)?;

    let mut data = CalloopData {
        state,
        display_handle,
    };

    // Registered before the listening socket is bound, so that a compositor which has
    // already bound its socket is guaranteed to handle termination signals.
    register_signal_source(&loop_handle)?;

    let listening_socket = match config.socket_name.as_deref() {
        Some(name) => ListeningSocketSource::with_name(name)?,
        None => ListeningSocketSource::new_auto()?,
    };
    let socket_name = listening_socket.socket_name().to_os_string();

    register_socket_source(&loop_handle, listening_socket, socket_name.clone())?;
    register_display_source(&loop_handle, display)?;
    register_frame_source(&loop_handle, frame_interval(&config))?;

    info!(socket = ?socket_name, "inox-compositor ready");

    event_loop.run(None, &mut data, |_| {})?;

    info!(
        frames = data.state.frames_rendered(),
        "inox-compositor stopped"
    );

    Ok(())
}

/// Frame interval for the timer driving rendering, derived from the output refresh rate.
fn frame_interval(config: &CompositorConfig) -> Duration {
    let refresh_hz = config.refresh_hz.max(1) as u64;

    Duration::from_nanos(1_000_000_000 / refresh_hz)
}

/// Accept client connections and register them on the display.
fn register_socket_source(
    loop_handle: &calloop::LoopHandle<'static, CalloopData>,
    listening_socket: ListeningSocketSource,
    socket_name: OsString,
) -> Result<(), RunError> {
    loop_handle
        .insert_source(
            listening_socket,
            move |stream, _, data: &mut CalloopData| match data
                .display_handle
                .insert_client(stream, Arc::new(ClientState::default()))
            {
                Ok(client) => {
                    debug!(client = ?client.id(), socket = ?socket_name, "client connected");
                }
                Err(error) => warn!(%error, "rejected a client connection"),
            },
        )
        .map(drop)
        .map_err(|source| RunError::EventSource {
            name: "wayland socket",
            reason: source.to_string(),
        })
}

/// Drive the Wayland display whenever its backend reports readability.
fn register_display_source(
    loop_handle: &calloop::LoopHandle<'static, CalloopData>,
    display: Display<InoxState>,
) -> Result<(), RunError> {
    loop_handle
        .insert_source(
            Generic::new(display, Interest::READ, Mode::Level),
            |_, source, data: &mut CalloopData| {
                if let Err(error) = service_clients(source, &mut data.state) {
                    warn!(%error, "failed to service wayland clients");
                }

                Ok(PostAction::Continue)
            },
        )
        .map(drop)
        .map_err(|source| RunError::EventSource {
            name: "wayland display",
            reason: source.to_string(),
        })
}

/// Dispatch pending client requests and flush outgoing buffers.
#[allow(unsafe_code)]
fn service_clients(
    source: &mut NoIoDrop<Display<InoxState>>,
    state: &mut InoxState,
) -> io::Result<usize> {
    // SAFETY: the `Generic` source owns the `Display` for the whole lifetime of the
    // event loop. This callback neither moves the `Display` out of the source nor
    // drops the source, which is exactly what `Generic::get_mut` requires, and the
    // mutable reference never escapes this function.
    let display = unsafe { source.get_mut() };

    let dispatched = display.dispatch_clients(state)?;
    display.flush_clients()?;

    Ok(dispatched)
}

/// Turn `SIGINT` and `SIGTERM` into a clean shutdown request.
fn register_signal_source(
    loop_handle: &calloop::LoopHandle<'static, CalloopData>,
) -> Result<(), RunError> {
    let signals = Signals::new(&[Signal::SIGINT, Signal::SIGTERM])?;

    loop_handle
        .insert_source(signals, |event, _, data: &mut CalloopData| {
            info!(signal = ?event.signal(), "shutdown requested");
            data.state.request_exit();
        })
        .map(drop)
        .map_err(|source| RunError::EventSource {
            name: "signal",
            reason: source.to_string(),
        })
}

/// Render frames at the output refresh rate until shutdown is requested.
fn register_frame_source(
    loop_handle: &calloop::LoopHandle<'static, CalloopData>,
    interval: Duration,
) -> Result<(), RunError> {
    loop_handle
        .insert_source(
            Timer::from_duration(interval),
            move |_, _, data: &mut CalloopData| {
                if let Err(error) = data.state.render_frame() {
                    warn!(%error, "failed to render a frame, shutting down");
                    data.state.request_exit();

                    return TimeoutAction::Drop;
                }

                debug!(frames = data.state.frames_rendered(), "rendered frame");

                if data.state.frame_budget_reached() {
                    info!(
                        frames = data.state.frames_rendered(),
                        "frame budget reached, shutting down"
                    );
                    data.state.request_exit();

                    return TimeoutAction::Drop;
                }

                TimeoutAction::ToDuration(interval)
            },
        )
        .map(drop)
        .map_err(|source| RunError::EventSource {
            name: "frame timer",
            reason: source.to_string(),
        })
}
