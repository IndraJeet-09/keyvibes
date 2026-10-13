//! Unix-socket transport for the control protocol.
//!
//! Security model, in order of what keeps the engine honest:
//!
//! * the socket lives under `$XDG_RUNTIME_DIR` with mode `0600` and its
//!   parent directory `0700`, so no other user can even open it;
//! * every accepted connection is re-checked with `SO_PEERCRED`; a different
//!   uid is closed immediately (defence in depth against a mis-set umask or
//!   a bind-mounted runtime dir);
//! * nothing here listens on a network socket, and `KEYVIBES_SOCKET` only
//!   moves the path - it never turns the transport into TCP.
//!
//! Framing is one JSON document per line (`\n`-delimited UTF-8), so a client
//! can be a shell pipeline and a half-written line never desynchronises the
//! stream: the server drops the connection and the client retries.

use crate::protocol::{Envelope, Request, Response, ServerMessage};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Environment variable that overrides the control socket path.
pub const SOCKET_ENV: &str = "KEYVIBES_SOCKET";

/// Default directory name under the runtime dir.
const DIR_NAME: &str = "keyvibes";
/// Socket file name inside that directory.
const FILE_NAME: &str = "control.sock";

/// Resolves the control socket path.
///
/// Order: `$KEYVIBES_SOCKET`, then `$XDG_RUNTIME_DIR/keyvibes/control.sock`,
/// then `/tmp/keyvibes-$UID/control.sock`.
pub fn default_socket_path() -> PathBuf {
    if let Some(override_path) = std::env::var_os(SOCKET_ENV) {
        if !override_path.is_empty() {
            return PathBuf::from(override_path);
        }
    }
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        if !runtime.is_empty() {
            return PathBuf::from(runtime).join(DIR_NAME).join(FILE_NAME);
        }
    }
    // A fixed per-uid fallback: the directory is created 0700, so even under
    // a shared /tmp nothing else can enter it.
    PathBuf::from(format!("/tmp/keyvibes-{}", crate::uid())).join(FILE_NAME)
}

/// Returns the kernel-reported uid of the peer on `stream`, or the OS error
/// if the platform refused to answer.
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(credentials.uid)
}

/// Creates `path`'s parent directory as `0700` if it does not exist.
fn ensure_parent_dir(path: &Path) -> io::Result<()> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => return Ok(()),
    };
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    // `recursive(true)` does not chmod an existing directory; do it here so a
    // directory left too-open by an older build is corrected on next bind.
    let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    Ok(())
}

/// A listening control socket.
///
/// Binding fails with an actionable message if another live engine already
/// owns the socket; a socket left behind by a crashed engine is unlinked and
/// rebound instead of blocking startup.
pub struct Server {
    listener: UnixListener,
    path: PathBuf,
}

impl Server {
    /// Binds the control socket at `path`, enforcing `0700`/`0600` modes.
    pub fn bind(path: &Path) -> io::Result<Self> {
        ensure_parent_dir(path)?;

        if path.exists() {
            // A live engine accepts the probe connection; anything else
            // (connection refused, not a socket, host gone) means the file
            // is a leftover from a crash and is safe to replace.
            match UnixStream::connect(path) {
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!(
                            "another KeyVibes engine is already listening on {} \
                             (stop it first, or point {} elsewhere)",
                            path.display(),
                            SOCKET_ENV
                        ),
                    ));
                }
                Err(_) => {
                    let _ = std::fs::remove_file(path);
                }
            }
        }

        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            listener,
            path: path.to_path_buf(),
        })
    }

    /// The path this server is bound at.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Switches the listener between blocking and non-blocking accept.
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.listener.set_nonblocking(nonblocking)
    }

    /// Blocks until the next connection, verifying the peer's uid.
    ///
    /// A peer with a different uid is accepted and closed; the loop simply
    /// tries again, so a local attacker can neither connect nor wedge the
    /// server into a tight accept loop.
    pub fn accept(&self) -> io::Result<UnixStream> {
        loop {
            let (stream, _) = self.listener.accept()?;
            match peer_uid(&stream) {
                Ok(uid) if uid == crate::uid() => return Ok(stream),
                Ok(_) => continue,
                // If the kernel will not tell us, refuse the connection:
                // better to fail closed than serve an unverified peer.
                Err(_) => continue,
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A connected control client.
///
/// One request in, one [`ServerMessage`] out; subscribers then receive
/// [`crate::protocol::Event`]s until the connection closes.
pub struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u64,
    path: PathBuf,
}

impl Client {
    /// Connects to the engine listening at `path`.
    pub fn connect(path: &Path) -> io::Result<Self> {
        let stream = UnixStream::connect(path)?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self {
            stream,
            reader,
            next_id: 1,
            path: path.to_path_buf(),
        })
    }

    /// Connects to [`default_socket_path`].
    pub fn connect_default() -> io::Result<Self> {
        Self::connect(&default_socket_path())
    }

    /// The path this client is connected to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Sends `request` and waits for the matching response.
    pub fn request(&mut self, request: Request) -> io::Result<Response> {
        let id = self.next_id;
        self.next_id += 1;
        let envelope = Envelope { id, cmd: request };
        let mut line = serde_json::to_string(&envelope)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        self.stream.write_all(line.as_bytes())?;
        self.stream.flush()?;

        match self.read_message()? {
            ServerMessage::Response(response) => {
                if response.id != id {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("response id {} does not match request id {id}", response.id),
                    ));
                }
                Ok(response)
            }
            ServerMessage::Event(event) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unexpected event while waiting for a response: {}",
                    event.kind
                ),
            )),
        }
    }

    /// Sends `Request::Subscribe`; further reads yield state events.
    pub fn subscribe(&mut self) -> io::Result<Response> {
        self.request(Request::Subscribe)
    }

    /// Bounds how long the next read may block (`None` = block forever).
    ///
    /// The timeout is a socket-level option, so it also covers the cloned
    /// descriptor the buffered reader sits on. Returns `WouldBlock` on
    /// expiry - callers use it to fail a test or a watch loop instead of
    /// hanging.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(timeout)
    }

    /// Reads the next server message (response or event).
    pub fn read_message(&mut self) -> io::Result<ServerMessage> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "control connection closed",
            ));
        }
        serde_json::from_str(line.trim_end())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Reads the next event; errors if the next message is a response.
    pub fn read_event(&mut self) -> io::Result<crate::protocol::Event> {
        match self.read_message()? {
            ServerMessage::Event(event) => Ok(event),
            ServerMessage::Response(response) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected response id {} while subscribed", response.id),
            )),
        }
    }
}

/// Writes one server message as a single line.
pub fn write_message(stream: &mut UnixStream, message: &ServerMessage) -> io::Result<()> {
    let mut line = serde_json::to_string(message)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()
}

/// Reads one line-framed request from `reader`.
pub fn read_request(reader: &mut BufReader<UnixStream>) -> io::Result<Option<Envelope>> {
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        return Ok(None);
    }
    let envelope: Envelope = serde_json::from_str(line.trim_end())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some(envelope))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn temp_socket(name: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("kv-control-test-{}-{}", std::process::id(), name));
        dir.push("control.sock");
        dir
    }

    #[test]
    fn bind_mode_is_0600() {
        let path = temp_socket("mode");
        let _ = std::fs::remove_file(&path);
        let server = Server::bind(&path).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o600);
        let dir = path.parent().unwrap();
        assert_eq!(std::fs::metadata(dir).unwrap().mode() & 0o777, 0o700);
        drop(server);
        assert!(!path.exists(), "Drop removes the socket");
    }

    #[test]
    fn live_socket_is_not_unlinked() {
        let path = temp_socket("live");
        let _ = std::fs::remove_file(&path);
        let server = Server::bind(&path).unwrap();
        assert!(path.exists(), "a live socket must survive a second bind");
        let err = match Server::bind(&path) {
            Ok(_) => panic!("second bind of a live socket must fail"),
            Err(error) => error,
        };
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
        drop(server);
    }

    #[test]
    fn stale_socket_is_replaced() {
        let path = temp_socket("stale");
        let _ = std::fs::remove_file(&path);
        // A leftover file from a crashed engine: nothing is listening, so
        // the probe fails and the bind must replace it.
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"stale").unwrap();
        let server = Server::bind(&path).unwrap();
        assert_eq!(server.path(), path.as_path());
        drop(server);
    }

    #[test]
    fn request_response_round_trip() {
        let path = temp_socket("rt");
        let _ = std::fs::remove_file(&path);
        let server = Server::bind(&path).unwrap();

        let client_path = path.to_path_buf();
        let handle = std::thread::spawn(move || {
            let mut client = Client::connect(&client_path).unwrap();
            let response = client.request(Request::Ping).unwrap();
            assert!(response.ok);
            assert_eq!(response.error, None);
        });

        let stream = server.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut stream = stream;
        let envelope = read_request(&mut reader).unwrap().unwrap();
        assert_eq!(envelope.cmd, Request::Ping);
        let response = Response {
            id: envelope.id,
            ok: true,
            error: None,
            state: None,
        };
        write_message(&mut stream, &ServerMessage::Response(response)).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn env_override_wins() {
        std::env::set_var(SOCKET_ENV, "/tmp/kv-override.sock");
        assert_eq!(
            default_socket_path(),
            PathBuf::from("/tmp/kv-override.sock")
        );
        std::env::remove_var(SOCKET_ENV);
    }
}
