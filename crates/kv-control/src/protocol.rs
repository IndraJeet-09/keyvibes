//! Wire types for the local control protocol.
//!
//! Everything is plain JSON so a UI written in any language can talk to the
//! engine. Two rules keep the types stable:
//!
//! * fields are additive - new optional fields may appear, existing ones
//!   never change meaning;
//! * enums are internally tagged with a lowercase `cmd`/`kind` field, so a
//!   newer client sending a command an older engine does not know gets a
//!   clean "unknown command" error rather than a parse failure.

use serde::{Deserialize, Serialize};

/// One request from a client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Liveness probe; the reply carries a state snapshot.
    Ping,
    /// Full state snapshot.
    GetState,
    /// Start streaming [`Event`]s on this connection after the reply.
    Subscribe,
    /// Master switch. `false` silences all sounds without unloading the pack.
    SetEnabled {
        /// New switch position.
        enabled: bool,
    },
    /// Perceptual volume in `[0, 1]`.
    SetVolume {
        /// New volume.
        volume: f32,
    },
    /// Pitch / gain variation toggles; `None` leaves that toggle unchanged.
    SetVariation {
        /// Pitch deviation toggle.
        pitch: Option<bool>,
        /// Gain deviation toggle.
        gain: Option<bool>,
    },
    /// Key-position stereo spatialization.
    SetSpatial {
        /// New switch position.
        enabled: bool,
    },
    /// Version-2 release (key-up) sounds.
    SetReleaseSounds {
        /// New switch position.
        enabled: bool,
    },
    /// Load another pack by name or path.
    SetPack {
        /// Pack name from `pack list`, or a filesystem path.
        selector: String,
    },
    /// Play a few keys from a pack through the real output path.
    Preview {
        /// Pack name or path; `None` previews the active pack.
        pack: Option<String>,
        /// Key names as `PhysicalKey` debug names (e.g. `"KeyA"`); `None`
        /// plays a short built-in demo sequence.
        keys: Option<Vec<String>>,
    },
    /// Run the read-only environment checks (input access, PipeWire,
    /// packs, config) and return them alongside the state snapshot.
    ///
    /// Every check is side-effect free: it never opens the engine's audio
    /// stream, never grabs a device and never writes a file, so a UI can
    /// call it as often as it likes while the engine is playing.
    Diagnostics,
    /// Ask the engine to exit its main loop.
    Shutdown,
}

/// One response to a [`Request`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// The `id` the client put in the [`Envelope`], echoed back.
    pub id: u64,
    /// Whether the command was applied.
    pub ok: bool,
    /// Human-readable failure reason when `ok` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Fresh state snapshot on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<State>,
    /// Environment checks, only present on a successful
    /// [`Request::Diagnostics`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<DiagnosticsReport>,
}

/// A request wrapped with the client's correlation id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// Client-chosen correlation id, echoed in the [`Response`].
    pub id: u64,
    /// The command itself.
    #[serde(flatten)]
    pub cmd: Request,
}

/// A server-originated state push sent to subscribers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Always `"state"`, so clients can skip unknown kinds.
    pub kind: String,
    /// Snapshot taken when the event was published.
    pub state: State,
}

impl Event {
    /// Builds a state event.
    pub fn state(state: State) -> Self {
        Self {
            kind: "state".to_string(),
            state,
        }
    }
}

/// Anything the engine can send on a connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ServerMessage {
    /// Reply to a request.
    Response(Response),
    /// Push to a subscriber.
    Event(Event),
}

/// Full engine state, small enough to send on every reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// Process-level facts.
    pub engine: EngineInfo,
    /// The settings the engine is currently applying.
    pub settings: LiveSettings,
    /// The loaded pack, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack: Option<PackInfo>,
    /// Every pack discovered on disk.
    pub packs: Vec<PackInfo>,
    /// Audio output status.
    pub audio: AudioInfo,
    /// Keyboard input status.
    pub input: InputInfo,
    /// Non-fatal problems worth showing the user.
    pub issues: Vec<String>,
}

/// Process-level engine facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineInfo {
    /// OS process id, so a UI can match the socket to a process.
    pub pid: u32,
    /// `keyvibes --version` string.
    pub version: String,
    /// Whether input is simulated (diagnostic mode).
    pub simulate: bool,
    /// Seconds since the engine started serving.
    pub uptime_secs: u64,
}

/// The settings currently in effect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveSettings {
    /// Master switch.
    pub enabled: bool,
    /// Perceptual volume in `[0, 1]`.
    pub volume: f32,
    /// Pitch variation toggle.
    pub pitch_variation: bool,
    /// Gain variation toggle.
    pub gain_variation: bool,
    /// Key-position spatialization.
    pub spatial_audio: bool,
    /// Release (key-up) sounds.
    pub release_sounds: bool,
}

/// One installed pack, as reported to clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackInfo {
    /// Display name (`pack list` first column).
    pub name: String,
    /// Genre-ish category from the pack metadata, or `"uncategorized"`.
    pub category: String,
    /// Absolute path of the `.kvpack` file.
    pub path: String,
    /// Keys with press clips.
    pub keys: u32,
    /// Keys with release clips (0 for version-1 packs).
    pub release_keys: u32,
    /// Total clips (press + release).
    pub clips: u32,
    /// Sample rate the clips were authored at.
    pub sample_rate: u32,
    /// Covered keys out of `kv_core::PhysicalKey::COUNT`'s 104.
    pub coverage: u32,
    /// Whether this pack has any release clips.
    pub has_release: bool,
    /// Pack author, when the manifest declares one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Licence of the pack's audio, as written in the manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Where the audio came from (upstream project, synthesizer recipe, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// One-line description from the manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `true` for the packs KeyVibes synthesizes itself, so a UI can label
    /// them honestly and never present them as field recordings.
    #[serde(default)]
    pub synthesized: bool,
}

/// Audio output status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioInfo {
    /// Lifecycle phase: `starting`, `running`, `reconnecting`, `failed`, `stopped`.
    pub phase: String,
    /// Render sample rate.
    pub output_rate: u32,
    /// Configured output device, or the default sink.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_device: Option<String>,
    /// Underrun count observed by the stream.
    pub xruns: u64,
    /// Frames rendered since start.
    pub frames_rendered: u64,
    /// Recent peak amplitude in `[0, 1]`.
    pub peak: f32,
    /// Last stream error, if the stream has failed at least once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// Keyboard input status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputInfo {
    /// Keyboards currently being read.
    pub keyboards: usize,
    /// Press events since start.
    pub presses: u64,
    /// Release events since start.
    pub releases: u64,
    /// Presses whose pack had no clip (silent, never substituted).
    pub missing_sounds: u64,
    /// Play commands queued since start.
    pub commands_generated: u64,
    /// Whether the hot-plug monitor is running, so a keyboard plugged in
    /// later is picked up without restarting the engine.
    #[serde(default)]
    pub hotplug: bool,
}

/// Result of a successful [`Request::Preview`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreviewResult {
    /// Pack that was previewed.
    pub pack: String,
    /// Keys that had a clip and were queued.
    pub keys: usize,
    /// Keys with no clip in that pack (stayed silent).
    pub missing: usize,
}

/// How badly a [`DiagnosticItem`] needs the user's attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStatus {
    /// The check passed; nothing to do.
    Ok,
    /// The check degraded but the engine still works.
    Warning,
    /// The check failed; the engine cannot do its job until it is fixed.
    Error,
}

/// One environment check, written for a person rather than a log file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticItem {
    /// Stable identifier (`input.permission`, `audio.pipewire`, ...), so a
    /// UI can match a recheck against its previous result.
    pub id: String,
    /// Short human label ("Keyboard input access").
    pub label: String,
    /// Outcome.
    pub status: DiagnosticStatus,
    /// What was actually observed - never a guess.
    pub detail: String,
    /// Plain-language explanation of what to do about it, if anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// An exact, copyable shell command that performs the fix, when one
    /// exists. KeyVibes never runs it itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Whether pressing "recheck" could plausibly change the result.
    pub recheckable: bool,
}

/// The full result of [`Request::Diagnostics`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticsReport {
    /// Checks in display order.
    pub items: Vec<DiagnosticItem>,
    /// `true` when every item is [`DiagnosticStatus::Ok`].
    pub healthy: bool,
    /// Number of items at each severity, for a one-line summary.
    pub errors: u32,
    /// See [`Self::errors`].
    pub warnings: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_round_trips() {
        let envelope = Envelope {
            id: 7,
            cmd: Request::SetPack {
                selector: "Alpaca".to_string(),
            },
        };
        let json = serde_json::to_string(&envelope).unwrap();
        assert!(json.contains("\"cmd\":\"set_pack\""));
        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back, envelope);
    }

    #[test]
    fn unknown_cmd_errors_cleanly() {
        let json = r#"{"id":1,"cmd":"launch_rockets"}"#;
        let parsed = serde_json::from_str::<Envelope>(json);
        assert!(parsed.is_err());
    }

    #[test]
    fn response_skips_absent_fields() {
        let response = Response {
            id: 1,
            ok: false,
            error: Some("no such pack".to_string()),
            state: None,
            diagnostics: None,
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(!json.contains("state"));
        assert!(!json.contains("null"));
    }

    #[test]
    fn event_is_tagged() {
        let state = State {
            engine: EngineInfo {
                pid: 1,
                version: "0.1.0".to_string(),
                simulate: true,
                uptime_secs: 0,
            },
            settings: LiveSettings {
                enabled: true,
                volume: 1.0,
                pitch_variation: true,
                gain_variation: true,
                spatial_audio: true,
                release_sounds: false,
            },
            pack: None,
            packs: vec![],
            audio: AudioInfo {
                phase: "running".to_string(),
                output_rate: 48_000,
                output_device: None,
                xruns: 0,
                frames_rendered: 0,
                peak: 0.0,
                last_error: None,
            },
            input: InputInfo {
                keyboards: 1,
                presses: 0,
                releases: 0,
                missing_sounds: 0,
                commands_generated: 0,
                hotplug: true,
            },
            issues: vec![],
        };
        let json = serde_json::to_string(&ServerMessage::Event(Event::state(state))).unwrap();
        assert!(json.contains("\"kind\":\"state\""));
        let back: ServerMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ServerMessage::Event(_)));
    }
}
