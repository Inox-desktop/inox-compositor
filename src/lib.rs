//! The Inox Wayland compositor, built on Smithay.
//!
//! Part of the Inox desktop environment, layer **L2**. See the repository
//! `AGENTS.md` for contribution and architecture constraints.
//!
//! # Current milestone
//!
//! The crate implements milestone **M1** (PRD §13): a headless compositor that
//! creates an output, renders a solid frame and exits cleanly. There is no
//! DRM/KMS, no input handling and no window management yet; those arrive in later
//! milestones.
//!
//! # Layout
//!
//! - [`config`] — command line parsing into plain configuration data.
//! - [`render`] — the EGL/GLES render target and the only `unsafe` code in the crate.
//! - [`runtime`] — the calloop event loop: Wayland socket, frame timer, signals.
//! - [`state`] — Smithay compositor state and its protocol handlers.

pub mod config;
pub mod render;
pub mod runtime;
pub mod state;

pub use config::{CompositorConfig, ConfigError, Invocation};
pub use render::{HeadlessRenderer, RenderError};
pub use runtime::{RunError, run};
pub use state::{InoxState, StateError};

/// The crate version as reported by Cargo.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
