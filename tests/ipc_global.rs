//! Integration test for the M1 `inox-protocols` consumption: the compositor must
//! advertise the private `inox_ipc` global and answer `get_version` with the
//! protocol version.
//!
//! Like `signal_shutdown.rs` this drives a real `inox-compositor` process, so the
//! test never holds an EGL display itself. The client half is a plain
//! `wayland-client` connection using the generated `inox-protocols` bindings, and
//! the Wayland socket lives in a private `XDG_RUNTIME_DIR`.
//!
//! The client interaction runs on a worker thread so that the blocking wayland
//! roundtrips are bounded by a timeout: a broken compositor then fails the test
//! instead of hanging CI.

use std::{
    fs,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc,
    thread::{self, sleep},
    time::{Duration, Instant},
};

use inox_protocols::ipc::inox_ipc::client::inox_ipc::{Event as IpcEvent, InoxIpc};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::wl_registry,
};

/// The compositor binary built for this test run.
const COMPOSITOR: &str = env!("CARGO_BIN_EXE_inox-compositor");

/// How long the compositor may take to bind its socket before the test gives up.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the compositor may take to exit after being asked to.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the client interaction may take before the test gives up.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(30);

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
fn spawn(socket_name: &str) -> (Child, RuntimeDir, PathBuf) {
    let runtime_dir = RuntimeDir::new(socket_name);

    let child = Command::new(COMPOSITOR)
        .args(["--headless", "--socket", socket_name])
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

/// What the client observed while talking to the compositor.
#[derive(Debug, Default)]
struct ClientOutcome {
    /// Whether the registry advertised the `inox_ipc` global.
    global_advertised: bool,
    /// The versions reported by `version` events, in arrival order.
    versions: Vec<u32>,
}

/// The client-side state the wayland dispatch handlers mutate.
#[derive(Debug, Default)]
struct IpcClientState {
    versions: Vec<u32>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for IpcClientState {
    fn event(
        _state: &mut Self,
        _registry: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
        // `registry_queue_init` already captured the initial global list.
    }
}

impl Dispatch<InoxIpc, ()> for IpcClientState {
    fn event(
        state: &mut Self,
        _ipc: &InoxIpc,
        event: IpcEvent,
        _data: &(),
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
    ) {
        if let IpcEvent::Version { version } = event {
            state.versions.push(version);
        }
    }
}

/// Connect, bind `inox_ipc` and exercise `get_version`.
fn talk_to_compositor(socket_path: &Path) -> anyhow::Result<ClientOutcome> {
    let stream = UnixStream::connect(socket_path)?;
    let connection = Connection::from_socket(stream)?;

    let mut state = IpcClientState::default();
    let (globals, mut queue) = registry_queue_init::<IpcClientState>(&connection)?;

    let global_advertised = globals
        .contents()
        .with_list(|list| list.iter().any(|global| global.interface == "inox_ipc"));

    let ipc: InoxIpc = globals.bind(&queue.handle(), 1..=1, ())?;

    // Binding the global makes the compositor send `version` right away.
    queue.roundtrip(&mut state)?;
    // `get_version` asks for another `version` event.
    ipc.get_version();
    queue.roundtrip(&mut state)?;

    Ok(ClientOutcome {
        global_advertised,
        versions: state.versions,
    })
}

#[test]
fn inox_ipc_global_is_advertised_and_answers_get_version() {
    let (mut child, _runtime_dir, socket) = spawn("inox-ipc");
    wait_for_socket(&socket);

    let (sender, receiver) = mpsc::channel();
    let client = thread::spawn(move || {
        let _ = sender.send(talk_to_compositor(&socket));
    });

    let outcome = match receiver.recv_timeout(CLIENT_TIMEOUT) {
        Ok(result) => result.expect("the inox-ipc client interaction failed"),
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();

            panic!("the inox-ipc client did not finish within {CLIENT_TIMEOUT:?}");
        }
    };

    // The compositor must stay healthy after serving a control client.
    send_sigterm(&child);
    let status = wait_with_timeout(&mut child);

    assert_eq!(
        status.code(),
        Some(0),
        "expected a clean exit after serving inox_ipc, got {status}"
    );

    client.join().expect("the inox-ipc client thread panicked");

    assert!(
        outcome.global_advertised,
        "the compositor did not advertise the inox_ipc global"
    );
    assert_eq!(
        outcome.versions,
        [1, 1],
        "expected a version event on bind and one for get_version"
    );
}
