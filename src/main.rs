//! Entry point of the `inox-compositor` binary.
//!
//! Command line handling lives here; everything else is in the library so that
//! integration tests can drive the compositor without spawning a process.

use std::io::{self, Write};

use anyhow::{Context, Result};
use inox_compositor::{
    VERSION,
    config::{HELP, Invocation, parse_args},
    runtime,
};
use tracing::info;

fn main() -> Result<()> {
    let invocation =
        parse_args(std::env::args().skip(1)).context("invalid command line arguments")?;

    match invocation {
        Invocation::Help => print(HELP),
        Invocation::Version => print(&format!("inox-compositor {VERSION}\n")),
        Invocation::Run(config) => {
            inox_common::logging::init().context("failed to initialise logging")?;

            info!(version = VERSION, "starting inox-compositor");
            runtime::run(config).context("the compositor event loop failed")
        }
    }
}

/// Write user-facing text to stdout.
///
/// `println!` is reserved for tests and examples (root `AGENTS.md` §3.5), so CLI
/// output goes through an explicit write instead.
fn print(text: &str) -> Result<()> {
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(text.as_bytes())
        .context("failed to write to stdout")?;
    stdout.flush().context("failed to flush stdout")
}
