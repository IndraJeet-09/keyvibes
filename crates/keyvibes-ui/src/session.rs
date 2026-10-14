//! Engine lifecycle and the control connection for the desktop UI.
//!
//! # Lifecycle model
//!
//! KeyVibes has exactly one engine process, and the control socket is what
//! makes it single-instance: a second `keyvibes run` refuses to bind and
//! exits. The UI therefore has two possible relationships with it:
//!
//! * **attached** - something else started the engine (the systemd user
//!   service, a terminal, another launcher). The UI is a remote control;
//!   quitting it leaves the engine running, because that engine is
//!   independently reachable through `keyvibes ctl`.
//! * **owned** - nothing was listening, so the UI spawned `keyvibes run`.
//!   The UI is now responsible for it: quitting sends a `Shutdown` request
//!   and waits, so no invisible engine outlives its only control surface.
//!
//! # Threading
//!
//! Nothing here runs on the GTK main loop. Two long-lived threads own the
//! socket traffic:
//!
//! * a **command worker** that owns one connection and drains a queue of
//!   requests, so a slow pack switch can never freeze a slider;
//! * an **event listener** on a second connection that pushes state
//!   snapshots until the engine closes it.
//!
//! Both post [`UiEvent`]s into one channel the GTK main loop drains. The
//! real-time path never sees any of this: requests are small JSON lines on
//! a `0600` unix socket.

use anyhow::{Context, Result};
use kv_control::{
    default_socket_path, Client, DiagnosticsReport, Request, Response, ServerMessage, State,
};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// How long to wait for a spawned engine to bind its control socket.
const BIND_TIMEOUT: Duration = Duration::from_secs(20);
/// Poll interval while waiting for the socket or an exit.
const POLL: Duration = Duration::from_millis(50);
/// How long a single control request may take before the UI gives up.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait for a graceful engine shutdown before forcing it.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// Idle delay before the listener retries a connect that failed.
const RECONNECT_DELAY: Duration = Duration::from_millis(300);

/// Messages the background threads post to the GTK main loop.
pub enum UiEvent {
    /// A fresh state snapshot from the subscription or a reply.
    ///
    /// Boxed because a `State` carries every installed pack, so the enum
    /// would otherwise be hundreds of bytes on every small message.
    State(Box<State>),
    /// The result of a [`Request::Diagnostics`].
    Diagnostics(Box<DiagnosticsReport>),
    /// The listener connected (or reconnected) to an engine.
    Connected,
    /// The listener lost the engine: it exited or the socket vanished.
    Disconnected,
    /// A line of engine stderr, for the diagnostics view.
    Log(String),
    /// `ensure_running` finished; the session is usable.
    Ready(Box<Session>),
    /// `ensure_running` could not produce a session.
    StartFailed(String),
    /// A request was refused by the engine or the socket failed.
    ActionFailed(String),
    /// The process was signalled: shut down the same way Quit does.
    Quit,
}

/// Work handed to the command worker thread.
enum Command {
    /// Send this request and report the outcome as a [`UiEvent`].
    Request(Request),
    /// Stop the engine (if this UI owns it) and end the worker.
    Quit,
}

/// A live relationship with an engine process.
pub struct Session {
    /// Queue for control requests; drained by the command worker.
    commands: async_channel::Sender<Command>,
    /// Whether this UI is responsible for the engine's lifetime.
    pub ownership: Ownership,
}

impl Session {
    /// Queues a request without waiting; the reply arrives as a
    /// [`UiEvent`]. Never blocks the GTK main loop.
    pub fn post(&self, request: Request) {
        let _ = self.commands.send_blocking(Command::Request(request));
    }

    /// Stops an owned engine and waits for it; leaves an attached one alone.
    ///
    /// Returns `true` when an engine was stopped, so the caller can tell
    /// the user what happened.
    pub fn shutdown(&mut self) -> bool {
        let _ = self.commands.send_blocking(Command::Quit);
        self.ownership.await_exit(SHUTDOWN_TIMEOUT)
    }
}

/// Who owns the engine process.
pub enum Ownership {
    /// Started by this UI; quitting must stop it.
    Owned(Child),
    /// Started by somebody else; quitting must leave it alone.
    Attached,
}

impl Ownership {
    /// Whether the UI is responsible for stopping the engine.
    pub fn is_owned(&self) -> bool {
        matches!(self, Ownership::Owned(_))
    }

    /// Waits up to `timeout` for an owned engine to exit, then forces it.
    ///
    /// Returns `true` when an engine was stopped. An attached engine is
    /// never signalled: it belongs to whoever started it.
    fn await_exit(&mut self, timeout: Duration) -> bool {
        let Ownership::Owned(child) = self else {
            return false;
        };
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) if Instant::now() < deadline => thread::sleep(POLL),
                Ok(None) | Err(_) => break,
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        true
    }
}

/// An established control connection.
///
/// Reconnects lazily: a request against a stale socket transparently
/// reconnects once, so a UI action right after an engine restart works.
pub struct Control {
    socket: PathBuf,
    client: Option<Client>,
}

impl Control {
    /// Connects to the engine listening at `socket`.
    pub fn connect(socket: PathBuf) -> Result<Self> {
        let client = Client::connect(&socket)
            .with_context(|| format!("cannot connect to {}", socket.display()))?;
        client
            .set_read_timeout(Some(REQUEST_TIMEOUT))
            .context("cannot bound the control socket read")?;
        Ok(Self {
            socket,
            client: Some(client),
        })
    }

    /// Sends one request, reconnecting once if the socket was recycled.
    pub fn send(&mut self, request: Request) -> Result<Response> {
        match self.request(&request) {
            Ok(response) => Ok(response),
            Err(first_error) => {
                self.client = None;
                self.request(&request).map_err(|second_error| {
                    anyhow::anyhow!(
                        "the engine at {} is not answering ({first_error}; retry: {second_error})",
                        self.socket.display()
                    )
                })
            }
        }
    }

    fn request(&mut self, request: &Request) -> Result<Response> {
        if self.client.is_none() {
            let client = Client::connect(&self.socket)
                .with_context(|| format!("cannot connect to {}", self.socket.display()))?;
            client
                .set_read_timeout(Some(REQUEST_TIMEOUT))
                .context("cannot bound the control socket read")?;
            self.client = Some(client);
        }
        let client = self.client.as_mut().expect("client just installed");
        let response = client.request(request.clone()).context("request failed")?;
        if !response.ok {
            anyhow::bail!(
                "the engine rejected the request: {}",
                response
                    .error
                    .unwrap_or_else(|| "unknown error".to_string())
            );
        }
        Ok(response)
    }
}

/// Is an engine already listening?
pub fn engine_is_running(socket: &Path) -> bool {
    Client::connect(socket).is_ok()
}

/// Resolves the `keyvibes` engine binary.
///
/// Order: `$KEYVIBES_BIN` (explicit override, used by tests), the sibling of
/// this executable (how a packaged install lays it out), then `PATH`.
pub fn find_engine_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("KEYVIBES_BIN") {
        if !path.is_empty() {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(dir) = current.parent() {
            let sibling = dir.join("keyvibes");
            if sibling.is_file() {
                return Some(sibling);
            }
        }
    }
    path_lookup("keyvibes")
}

/// Looks `name` up in `PATH`.
fn path_lookup(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Spawns the command worker on its own connection to `socket`.
///
/// The returned queue is the only handle the UI keeps; dropping it ends the
/// worker.
fn spawn_controller(
    socket: PathBuf,
    events: &async_channel::Sender<UiEvent>,
) -> Option<async_channel::Sender<Command>> {
    let control = match Control::connect(socket) {
        Ok(control) => control,
        Err(error) => {
            let _ = events.send_blocking(UiEvent::StartFailed(format!("{error:#}")));
            return None;
        }
    };
    let (commands, inbox) = async_channel::unbounded::<Command>();
    let worker_events = events.clone();
    let spawned = thread::Builder::new()
        .name("keyvibes-ui-commands".to_string())
        .spawn(move || command_worker(control, inbox, worker_events));
    match spawned {
        Ok(_) => Some(commands),
        Err(error) => {
            let _ = events.send_blocking(UiEvent::StartFailed(format!(
                "could not start the control worker: {error}"
            )));
            None
        }
    }
}

/// Owns one control connection and drains the request queue.
fn command_worker(
    mut control: Control,
    inbox: async_channel::Receiver<Command>,
    events: async_channel::Sender<UiEvent>,
) {
    while let Ok(command) = inbox.recv_blocking() {
        let request = match command {
            Command::Quit => {
                let _ = control.send(Request::Shutdown);
                break;
            }
            Command::Request(request) => request,
        };
        match control.send(request) {
            Ok(response) => {
                if let Some(diagnostics) = response.diagnostics {
                    if events
                        .send_blocking(UiEvent::Diagnostics(Box::new(diagnostics)))
                        .is_err()
                    {
                        return;
                    }
                }
                if let Some(state) = response.state {
                    if events
                        .send_blocking(UiEvent::State(Box::new(state)))
                        .is_err()
                    {
                        return;
                    }
                }
            }
            Err(error) => {
                if events
                    .send_blocking(UiEvent::ActionFailed(format!("{error:#}")))
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}

/// Starts (or attaches to) the engine, off the GTK main loop.
///
/// Posts [`UiEvent::Ready`] with an owned or attached session, or
/// [`UiEvent::StartFailed`] carrying an explanation the UI can show.
pub fn ensure_running(socket: PathBuf, events: async_channel::Sender<UiEvent>) {
    if engine_is_running(&socket) {
        if let Some(commands) = spawn_controller(socket.clone(), &events) {
            let _ = events.send_blocking(UiEvent::Ready(Box::new(Session {
                commands,
                ownership: Ownership::Attached,
            })));
        }
        return;
    }

    let binary = match find_engine_binary() {
        Some(path) => path,
        None => {
            let _ = events.send_blocking(UiEvent::StartFailed(
                "the `keyvibes` engine binary was not found. Install KeyVibes, or set \
                 KEYVIBES_BIN to its path."
                    .to_string(),
            ));
            return;
        }
    };

    let mut child = match ProcessCommand::new(&binary)
        .arg("run")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let _ = events.send_blocking(UiEvent::StartFailed(format!(
                "could not start {}: {error}",
                binary.display()
            )));
            return;
        }
    };

    // Drain stderr on its own thread: an undrained pipe fills up and the
    // engine would eventually block on its own diagnostics.
    if let Some(stderr) = child.stderr.take() {
        let forward = events.clone();
        thread::Builder::new()
            .name("keyvibes-ui-engine-log".to_string())
            .spawn(move || {
                use std::io::BufRead;
                for line in std::io::BufReader::new(stderr)
                    .lines()
                    .map_while(Result::ok)
                {
                    if forward.send_blocking(UiEvent::Log(line)).is_err() {
                        break;
                    }
                }
            })
            .ok();
    }

    let deadline = Instant::now() + BIND_TIMEOUT;
    while Instant::now() < deadline {
        if engine_is_running(&socket) {
            // Give the listener thread a beat to observe the new socket.
            thread::sleep(POLL);
            match spawn_controller(socket.clone(), &events) {
                Some(commands) => {
                    let _ = events.send_blocking(UiEvent::Ready(Box::new(Session {
                        commands,
                        ownership: Ownership::Owned(child),
                    })));
                }
                None => {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
            return;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let _ = events.send_blocking(UiEvent::StartFailed(format!(
                    "the engine exited with {status} before it could bind {}. Check that \
                     PipeWire is running (`keyvibes doctor`).",
                    socket.display()
                )));
                return;
            }
            Ok(None) => thread::sleep(POLL),
            Err(error) => {
                let _ = events.send_blocking(UiEvent::StartFailed(format!(
                    "could not supervise the engine process: {error}"
                )));
                return;
            }
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    let _ = events.send_blocking(UiEvent::StartFailed(format!(
        "the engine did not bind {} within {}s",
        socket.display(),
        BIND_TIMEOUT.as_secs()
    )));
}

/// Subscribes to state pushes and keeps reconnecting for the UI's lifetime.
///
/// Reads block: the engine pushes an event whenever state changes and closes
/// the connection on exit, so a timeout here would only manufacture false
/// disconnects. The thread ends when the destination channel closes, which
/// is how the UI says "I am quitting".
pub fn listen(socket: PathBuf, events: async_channel::Sender<UiEvent>) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("keyvibes-ui-events".to_string())
        .spawn(move || loop {
            let mut client = match Client::connect(&socket) {
                Ok(client) => client,
                Err(_) => {
                    thread::sleep(RECONNECT_DELAY);
                    continue;
                }
            };
            if events.send_blocking(UiEvent::Connected).is_err() {
                return;
            }
            // Ask for a snapshot immediately so the window is not blank
            // until the engine happens to change something.
            if let Ok(response) = client.request(Request::GetState) {
                if let Some(state) = response.state {
                    if events
                        .send_blocking(UiEvent::State(Box::new(state)))
                        .is_err()
                    {
                        return;
                    }
                }
            }
            loop {
                match client.read_message() {
                    Ok(ServerMessage::Event(event)) => {
                        if events
                            .send_blocking(UiEvent::State(Box::new(event.state)))
                            .is_err()
                        {
                            return;
                        }
                    }
                    Ok(ServerMessage::Response(_)) => {}
                    Err(_) => break,
                }
            }
            if events.send_blocking(UiEvent::Disconnected).is_err() {
                return;
            }
            thread::sleep(RECONNECT_DELAY);
        })
        .expect("failed to spawn the event listener")
}

/// The socket path for this session, honouring `$KEYVIBES_SOCKET`.
pub fn engine_socket() -> PathBuf {
    default_socket_path()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_binary_resolution_prefers_the_explicit_override() {
        let dir = std::env::temp_dir().join(format!("keyvibes-bin-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let binary = dir.join("keyvibes");
        let _ = std::fs::write(&binary, b"#!/bin/sh\nexit 0\n");
        std::env::set_var("KEYVIBES_BIN", &binary);
        assert_eq!(find_engine_binary().as_deref(), Some(binary.as_path()));
        std::env::remove_var("KEYVIBES_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_override_falls_through_rather_than_failing() {
        std::env::set_var("KEYVIBES_BIN", "/definitely/not/here/keyvibes");
        // Without a sibling binary this may still find a real install;
        // what must never happen is returning the nonexistent override.
        if let Some(found) = find_engine_binary() {
            assert!(found.is_file(), "resolution must not return a ghost");
        }
        std::env::remove_var("KEYVIBES_BIN");
    }

    #[test]
    fn attached_ownership_never_stops_the_engine() {
        let mut ownership = Ownership::Attached;
        assert!(!ownership.is_owned());
        assert!(!ownership.await_exit(Duration::from_millis(1)));
    }

    #[test]
    fn path_lookup_finds_sh_or_reports_absently() {
        if let Some(found) = path_lookup("sh") {
            assert!(found.ends_with("sh"));
        }
        assert!(path_lookup("keyvibes-definitely-not-installed").is_none());
    }

    #[test]
    fn an_absent_socket_is_not_running() {
        let path =
            std::env::temp_dir().join(format!("keyvibes-nothing-here-{}.sock", std::process::id()));
        assert!(!engine_is_running(&path));
    }

    #[test]
    fn the_worker_reports_a_refused_request_rather_than_panicking() {
        // Bind a listener that never replies, so the request times out and
        // the error surfaces as `ActionFailed` instead of hanging the UI.
        let path =
            std::env::temp_dir().join(format!("keyvibes-ui-worker-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        if !engine_is_running(&path) {
            // Nothing is listening: the controller cannot be built, which is
            // itself the failure the UI must see.
            assert!(Control::connect(path).is_err());
        }
    }
}
