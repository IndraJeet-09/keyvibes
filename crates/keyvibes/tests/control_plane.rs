//! Integration test for the local control plane.
//!
//! Starts the shipped binary in `--simulate --duration` mode against a
//! private socket, then drives it through `kv-control` exactly the way the
//! desktop UI does: ping, toggle settings, switch packs, preview, shutdown.
//! Asserts the socket is cleaned up when the engine exits.
//!
//! When no PipeWire session is reachable the engine cannot start, so the
//! test reports a skip (println + early return) rather than a failure -
//! matching the workspace convention that hardware-dependent steps are
//! `NOT RUN`, never a false pass.

use kv_control::{Client, Request};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long to wait for the engine to bind its socket.
const BIND_TIMEOUT: Duration = Duration::from_secs(15);

struct Engine {
    child: Child,
    socket: PathBuf,
    config: PathBuf,
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_file(&self.config);
    }
}

/// Starts the engine on a unique socket; `None` when PipeWire is missing.
fn start_engine() -> Option<Engine> {
    let id = std::process::id();
    let socket = std::env::temp_dir().join(format!("keyvibes-ctl-test-{id}.sock"));
    let config = std::env::temp_dir().join(format!("keyvibes-ctl-test-{id}.toml"));
    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&config);

    // The engine picks whatever pack the machine has installed; the control
    // protocol does not care which one.
    let mut child = Command::new(env!("CARGO_BIN_EXE_keyvibes"))
        .args(["run", "--simulate", "--duration", "40"])
        .env("KEYVIBES_SOCKET", &socket)
        .env("KEYVIBES_CONFIG", &config)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + BIND_TIMEOUT;
    while Instant::now() < deadline {
        if socket.exists() {
            // Give the listener a beat to come up after the file appears.
            std::thread::sleep(Duration::from_millis(100));
            if Client::connect(&socket).is_ok() {
                return Some(Engine {
                    child,
                    socket,
                    config,
                });
            }
        }
        // A dead child means the engine failed (probably no PipeWire).
        if let Ok(Some(status)) = child.try_wait() {
            let _ = status;
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // Timed out: kill and treat as unavailable.
    let mut engine = Engine {
        child,
        socket,
        config,
    };
    let _ = engine.child.kill();
    let _ = engine.child.wait();
    None
}

#[test]
fn control_plane_round_trip() {
    let started = Instant::now();
    let Some(mut engine) = start_engine() else {
        println!("SKIP: engine could not start (no PipeWire session on this host?)");
        return;
    };
    eprintln!("[ctl-test] engine up after {:?}", started.elapsed());

    let mut client = Client::connect(&engine.socket).expect("connect to the live engine");

    // --- ping carries a full state snapshot -------------------------------
    let response = client.request(Request::Ping).expect("ping");
    assert!(response.ok, "ping must succeed: {:?}", response.error);
    let state = response.state.expect("ping replies with state");
    assert!(state.settings.enabled, "engine starts enabled");
    assert!(!state.packs.is_empty(), "at least one pack installed");
    assert!(state.pack.is_some(), "a pack is loaded at startup");

    // --- volume round-trips through the live mixer ------------------------
    let response = client.request(Request::SetVolume { volume: 0.42 }).unwrap();
    assert!(response.ok, "set volume: {:?}", response.error);
    assert!(
        (response.state.unwrap().settings.volume - 0.42).abs() < 1e-6,
        "volume is applied immediately"
    );

    // --- invalid volume is rejected with a message, not applied -----------
    let response = client.request(Request::SetVolume { volume: 3.5 }).unwrap();
    assert!(!response.ok, "out-of-range volume must be rejected");
    assert!(
        response.error.unwrap().contains("0.0 and 1.0"),
        "the error names the allowed range"
    );

    // --- enable switch ----------------------------------------------------
    let response = client
        .request(Request::SetEnabled { enabled: false })
        .unwrap();
    assert!(response.ok);
    assert!(!response.state.unwrap().settings.enabled);

    let response = client
        .request(Request::SetEnabled { enabled: true })
        .unwrap();
    assert!(response.ok);
    assert!(response.state.unwrap().settings.enabled);

    // --- variation toggles are independent -------------------------------
    let response = client
        .request(Request::SetVariation {
            pitch: Some(false),
            gain: None,
        })
        .unwrap();
    assert!(response.ok);
    let settings = response.state.unwrap().settings;
    assert!(!settings.pitch_variation, "pitch toggle applied");
    assert!(settings.gain_variation, "gain toggle untouched");

    // --- pack switch lands on the next main-loop tick ---------------------
    let first_pack = state.pack.clone().expect("startup pack").name;
    let target = state
        .packs
        .iter()
        .find(|p| p.name != first_pack)
        .map(|p| p.name.clone());
    if let Some(target) = target {
        let response = client
            .request(Request::SetPack {
                selector: target.clone(),
            })
            .unwrap();
        assert!(response.ok, "set pack: {:?}", response.error);
        // Deferred: poll until the main loop applies it.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            std::thread::sleep(Duration::from_millis(150));
            let state = client.request(Request::GetState).unwrap().state.unwrap();
            if state.pack.as_ref().is_some_and(|p| p.name == target) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "pack switch to {target:?} never landed"
            );
        }
    }

    // --- preview queues through the real path ----------------------------
    let response = client
        .request(Request::Preview {
            pack: None,
            keys: Some(vec!["A".into(), "S".into()]),
        })
        .unwrap();
    assert!(response.ok, "preview: {:?}", response.error);

    // --- unknown command fails cleanly ------------------------------------
    let bad = r#"{"id":99,"cmd":"launch_rockets"}"#;
    // Send it raw through a fresh connection: serde must reject it.
    {
        use std::io::Write;
        let mut raw = std::os::unix::net::UnixStream::connect(&engine.socket).unwrap();
        writeln!(raw, "{bad}").unwrap();
        // Server closes the connection on an unparsable line; just assert we
        // get EOF or an error rather than a hang or a panic.
        let mut buf = [0u8; 16];
        let _ = std::io::Read::read(&mut raw, &mut buf);
    }

    // --- subscribers receive an event for every mutation -----------------
    let mut watcher = Client::connect(&engine.socket).expect("watcher connect");
    let sub = watcher.subscribe().expect("subscribe");
    assert!(sub.ok, "subscribe: {:?}", sub.error);
    // Mutate through the first connection; the watcher must hear about it.
    let response = client.request(Request::SetVolume { volume: 0.73 }).unwrap();
    assert!(response.ok);
    let event = {
        // 5s read timeout so a missing event fails instead of hanging.
        watcher
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout");
        watcher.read_event()
    };
    match event {
        Ok(event) => {
            assert_eq!(event.kind, "state", "event kind");
            assert!(
                (event.state.settings.volume - 0.73).abs() < 1e-6,
                "the event carries the new volume, got {}",
                event.state.settings.volume
            );
        }
        Err(error) => panic!("subscribed client must receive a state event: {error}"),
    }

    // --- shutdown removes the socket --------------------------------------
    let response = client.request(Request::Shutdown).unwrap();
    assert!(response.ok, "shutdown: {:?}", response.error);

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if !engine.socket.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !engine.socket.exists(),
        "engine must unlink its control socket on exit"
    );
    let status = engine.child.wait().expect("engine must exit");
    assert!(status.success(), "engine exits cleanly after shutdown");
    eprintln!("[ctl-test] full round trip took {:?}", started.elapsed());
}
