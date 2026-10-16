//! Integration test for the M1 acceptance criteria: the compositor creates a headless
//! output and renders a frame into it.
//!
//! Everything runs in a single test function on purpose. A process may only ever hold
//! one EGL display (see the `SAFETY` note in `src/render.rs`), and tests inside one test
//! binary run in parallel threads — so a second renderer elsewhere in this file would
//! race with this one.

use calloop::EventLoop;
use inox_compositor::{CompositorConfig, InoxState, render::CLEAR_COLOR_RGBA8};
use smithay::reexports::wayland_server::Display;

/// Tolerance of one 8-bit step, covering float-to-unorm rounding differences between GL
/// drivers (llvmpipe and radeonsi disagree by one step on the same colour).
const TOLERANCE: u8 = 1;

#[test]
fn headless_output_creates_and_renders_a_frame() {
    let event_loop: EventLoop<'static, ()> =
        EventLoop::try_new().expect("failed to create the event loop");
    let display: Display<InoxState> = Display::new().expect("failed to create the wayland display");

    let config = CompositorConfig {
        exit_after_frames: Some(2),
        ..CompositorConfig::default()
    };

    let mut state = InoxState::new(&display.handle(), &config, event_loop.get_signal())
        .expect("failed to create the compositor state");

    // The output is advertised with the configured mode.
    let output = state.output();

    assert_eq!(output.name(), config.output_name);
    assert_eq!(
        output.current_mode().map(|mode| (mode.size.w, mode.size.h)),
        Some((1280, 720)),
        "the headless output should be at its default mode"
    );
    assert_eq!(
        output.current_mode().map(|mode| mode.refresh),
        Some(60_000),
        "the output mode should carry the refresh rate in millihertz"
    );

    // The first frame reaches the render target...
    state.render_frame().expect("failed to render a frame");

    assert_eq!(state.frames_rendered(), 1);
    assert!(
        !state.frame_budget_reached(),
        "one frame should not satisfy a two-frame budget"
    );
    assert!(!state.exit_requested());

    let pixel = state
        .read_frame_center_pixel()
        .expect("failed to read the frame back");

    assert!(
        pixel
            .iter()
            .zip(CLEAR_COLOR_RGBA8.iter())
            .all(|(actual, expected)| actual.abs_diff(*expected) <= TOLERANCE),
        "expected the background colour {CLEAR_COLOR_RGBA8:?} (tolerance {TOLERANCE}), read back {pixel:?}"
    );

    // ...and the second frame exhausts the budget, which asks the loop to stop.
    state.render_frame().expect("failed to render a frame");

    assert_eq!(state.frames_rendered(), 2);
    assert!(state.frame_budget_reached());

    state.request_exit();

    assert!(state.exit_requested());
}
