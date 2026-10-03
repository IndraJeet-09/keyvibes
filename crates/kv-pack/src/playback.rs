//! Playback command for mixer integration.
//!
//! KVPack produces [`kv_core::PlayCommand`] values: the single command type
//! shared by the input pipeline, the mixer's SPSC queue, and the audio
//! backend. The pack-specific work is building the command from a key press
//! (see [`crate::loader::KvPack::play_command`]).

pub use kv_core::PlayCommand;
