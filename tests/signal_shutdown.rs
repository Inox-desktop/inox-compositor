//! Integration tests for the M1 acceptance criterion that the compositor exits with
//! status 0 instead of being killed: both the frame budget and a termination signal
//! must lead to a clean shutdown.
//!
//! Each test drives a real `inox-compositor` process, so these tests never hold an EGL
//! display themselves. The Wayland socket is bound inside `XDG_RUNTIME_DIR`, which is
//! unset on CI runners, so every process gets a private directory of its own.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

/// The compositor binary built for this test run.
const COMPOSITOR: &str = env!("CARGO_BIN_EXE_inox-compositor");

/// How long the compositor may take to bind its socket before the test gives up.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the compositor may take to exit after being asked to.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

/// A private `XDG_RUNTIME_DIR` for one compositor process.
struct RuntimeDir(PathBuf);

impl RuntimeDir {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("inox-compositor-{label}-{}", std::process::id()));

        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("failed to create the test runtime directory");

        Self(path)
    }

    fn socket(&self, socket_name: &str) -> PathBuf {
        self.0.join(socket_name)
    }
}

impl Drop for RuntimeDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Start a compositor with a private runtime directory and a known socket name.
fn spawn(socket_name: &str, extra_args: &[&str]) -> (Child, RuntimeDir, PathBuf) {
    let runtime_dir = RuntimeDir::new(socket_name);

    let child = Command::new(COMPOSITOR)
        .args(["--headless", "--socket", socket_name])
        .args(extra_args)
        .env("XDG_RUNTIME_DIR", runtime_dir.0.as_os_str())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn inox-compositor");

    let socket = runtime_dir.socket(socket_name);

    (child, runtime_dir, socket)
}

/// Block until the compositor has bound its Wayland socket.
fn wait_for_socket(path: &Path) {
    let deadline = Instant::now() + STARTUP_TIMEOUT;

    while Instant::now() < deadline {
        if path.exists() {
            return;
        }

        sleep(Duration::from_millis(20));
    }

    panic!(
        "inox-compositor did not bind {} within {STARTUP_TIMEOUT:?}",
        path.display()
    );
}

/// Wait for the compositor to exit, killing it if it takes too long.
fn wait_with_timeout(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;

    loop {
        if let Some(status) = child.try_wait().expect("failed to poll inox-compositor") {
            return status;
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();

            panic!("inox-compositor did not exit within {SHUTDOWN_TIMEOUT:?}");
        }

        sleep(Duration::from_millis(20));
    }
}

/// Send `SIGTERM` to the compositor.
fn send_sigterm(child: &Child) {
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("failed to run kill");

    assert!(status.success(), "kill -TERM exited with {status}");
}

#[test]
fn termination_signal_exits_with_status_zero() {
    let (mut child, _runtime_dir, socket) = spawn("inox-sigterm", &[]);

    wait_for_socket(&socket);
    send_sigterm(&child);

    let status = wait_with_timeout(&mut child);

    assert_eq!(
        status.code(),
        Some(0),
        "expected a clean exit after SIGTERM, got {status}"
    );
}

#[test]
fn frame_budget_exits_with_status_zero() {
    let (mut child, _runtime_dir, _socket) = spawn("inox-frames", &["--exit-after-frames", "2"]);

    let status = wait_with_timeout(&mut child);

    assert_eq!(
        status.code(),
        Some(0),
        "expected a clean exit after rendering the frame budget, got {status}"
    );
}
