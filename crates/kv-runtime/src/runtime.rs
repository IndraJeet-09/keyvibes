//! Runtime coordination for KeyVibes.

use kv_audio_pipewire::stream::PipeWireStream;
use kv_ring::SpscRing;
use kv_core::PlayCommand;
use std::sync::Arc;

/// Main runtime coordinating all subsystems.
pub struct Runtime {
    stream: Option<PipeWireStream>,
    command_queue: Arc<SpscRing<PlayCommand>>,
}

impl Runtime {
    pub fn new(output_rate: u32) -> Self {
        let queue = Arc::new(SpscRing::with_capacity(256));
        Self {
            stream: None,
            command_queue: queue,
        }
    }

    pub fn start(&mut self) -> Result<(), crate::kv_audio_pipewire::stream::AudioError> {
        let stream = PipeWireStream::new(48000, self.command_queue.clone())?;
        self.stream = Some(stream);
        Ok(())
    }
}
