//! The Inox Wayland compositor, built on Smithay.
//!
//! Part of the Inox desktop environment, layer **L2**. See the repository
//! `AGENTS.md` for contribution and architecture constraints.
//!
//! # Current milestone
//!
//! The crate implements milestone **M2** (PRD §13): a headless compositor that
//! creates an output, handles keyboard and pointer events, manages client
//! toplevels as tiled windows and renders the composited result. There is still
//! no DRM/KMS backend and no libinput backend (see ADR 0005 and ADR 0008).
//!
//! # Layout
//!
//! - [`config`] — command line parsing into plain configuration data.
//! - [`input`] — key presses mapped to compositor actions.
//! - [`layout`] — the pure master/stack tiling geometry.
//! - [`workspace`] — the pure window bookkeeping and layout order.
//! - [`render`] — the EGL/GLES render target and the only `unsafe` code in the crate.
//! - [`runtime`] — the calloop event loop: Wayland socket, frame timer, signals.
//! - [`state`] — Smithay compositor state and its protocol handlers.

pub mod config;
pub mod input;
pub mod layout;
pub mod render;
pub mod runtime;
pub mod state;
pub mod workspace;

pub use config::{CompositorConfig, ConfigError, Invocation};
pub use input::Action;
pub use layout::{LayoutConfig, MASTER_RATIO_MAX, MASTER_RATIO_MIN};
pub use render::{HeadlessRenderer, RenderError};
pub use runtime::{RunError, run};
pub use state::{ClientState, InoxState, StateError};
pub use workspace::{WindowId, Workspace};

/// The crate version as reported by Cargo.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
