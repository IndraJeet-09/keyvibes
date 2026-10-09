//! Scripted key-event source for hardware-free diagnostics.
//!
//! This is the fallback path used when no readable keyboard is available
//! (for example when the user is not in the `input` group). It drives the
//! **same** production pipeline as a real device:
//!
//! ```text
//! PhysicalKey -> SoundSource::play -> SPSC queue -> mixer -> PipeWire
//! ```
//!
//! It never touches `/dev/input` and never claims to represent real hardware.

use kv_core::{PhysicalKey, PlayCommand, SoundSource, StreamWake, VariantState};
use kv_input_linux::diagnostics::InputStats;
use kv_ring::SpscRing;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// One step of a simulated typing script.
#[derive(Debug, Clone)]
pub enum SimStep {
    /// Press (and immediately release) a key.
    Press(PhysicalKey),
    /// Press several keys in the same instant - a chord.
    ///
    /// Every key of the chord is queued before the next wait, so the audio
    /// thread drains them in the same quantum and the mixer really does hold
    /// them all at once.
    Chord(Vec<PhysicalKey>),
    /// Pause between steps.
    Wait(Duration),
}

/// Options for a simulated input session.
#[derive(Debug, Clone)]
pub struct SimInputOptions {
    /// Ordered steps to execute.
    pub script: Vec<SimStep>,
    /// How many times to repeat the script; `0` repeats until stopped.
    pub repeats: u32,
}

impl SimInputOptions {
    /// Builds a script that presses `keys` in order with `gap` between them.
    pub fn from_keys(keys: &[PhysicalKey], gap: Duration, repeats: u32) -> Self {
        let mut script = Vec::with_capacity(keys.len() * 2);
        for key in keys {
            script.push(SimStep::Press(*key));
            script.push(SimStep::Wait(gap));
        }
        Self { script, repeats }
    }

    /// Builds a sustained load script mixing single presses and chords.
    ///
    /// `keys_per_second` is the *average number of individual key events*,
    /// not the number of script steps: a chord of `n` consumes `n` events'
    /// worth of time, so the offered rate stays constant whatever the
    /// chord size.
    ///
    /// The script is deterministic for a given `seed`, which keeps stress
    /// runs reproducible. `repeats` is `0` (loop until stopped).
    pub fn stress(keys: &[PhysicalKey], keys_per_second: f64, seed: u64) -> Self {
        const STEPS: usize = 512;
        let keys = if keys.is_empty() {
            &[PhysicalKey::Space][..]
        } else {
            keys
        };
        let rate = keys_per_second.max(1.0);
        let mut rng = seed | 1;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };

        let mut script = Vec::with_capacity(STEPS * 2);
        for _ in 0..STEPS {
            let roll = next() % 100;
            // 1 in 4 steps is a chord; chord size 2..=8.
            let event: SimStep = if roll < 25 {
                let size = 2 + (next() % 7) as usize;
                let chord: Vec<PhysicalKey> = (0..size)
                    .map(|_| keys[(next() % keys.len() as u64) as usize])
                    .collect();
                let count = chord.len() as f64;
                script.push(SimStep::Chord(chord));
                script.push(SimStep::Wait(Duration::from_secs_f64(count / rate)));
                continue;
            } else {
                SimStep::Press(keys[(next() % keys.len() as u64) as usize])
            };
            script.push(event);
            script.push(SimStep::Wait(Duration::from_secs_f64(1.0 / rate)));
        }

        Self { script, repeats: 0 }
    }
}

/// Handle to a running simulated input source.
pub struct SimulatedInput {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl SimulatedInput {
    /// Starts a simulated device producing `script`, looping `repeats` times.
    ///
    /// `repeats == 0` loops until [`stop`](Self::stop) is called.
    pub fn start<S>(
        queue: Arc<SpscRing<PlayCommand>>,
        stats: Arc<InputStats>,
        source: Arc<S>,
        script: Vec<SimStep>,
        repeats: u32,
        wake: Option<Arc<dyn StreamWake>>,
    ) -> Self
    where
        S: SoundSource + Send + Sync + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();

        let handle = thread::Builder::new()
            .name("keyvibes-sim-input".to_string())
            .spawn(move || {
                let mut variant_states: [VariantState; PhysicalKey::COUNT] =
                    std::array::from_fn(|_| VariantState::default());
                stats.increment_device_added();

                let mut pass = 0u32;
                while !thread_stop.load(Ordering::Relaxed) {
                    for step in &script {
                        if thread_stop.load(Ordering::Relaxed) {
                            return;
                        }
                        match step.clone() {
                            SimStep::Press(key) => {
                                Self::emit(
                                    &queue,
                                    &stats,
                                    source.as_ref(),
                                    key,
                                    &mut variant_states,
                                    wake.as_deref(),
                                );
                                stats.increment_release();
                            }
                            SimStep::Chord(keys) => {
                                for key in keys {
                                    Self::emit(
                                        &queue,
                                        &stats,
                                        source.as_ref(),
                                        key,
                                        &mut variant_states,
                                        wake.as_deref(),
                                    );
                                    stats.increment_release();
                                }
                            }
                            SimStep::Wait(duration) => {
                                // Wake early when asked to stop.
                                let mut remaining = duration;
                                while remaining > Duration::ZERO {
                                    if thread_stop.load(Ordering::Relaxed) {
                                        return;
                                    }
                                    let chunk = remaining.min(Duration::from_millis(10));
                                    thread::sleep(chunk);
                                    remaining = remaining.saturating_sub(chunk);
                                }
                            }
                        }
                    }
                    pass += 1;
                    if repeats != 0 && pass >= repeats {
                        return;
                    }
                }
                stats.increment_device_removed();
            })
            .expect("failed to spawn simulated input thread");

        Self {
            stop,
            handle: Some(handle),
        }
    }

    /// Turns one key press into a queued command and records the latency.
    fn emit<S: SoundSource + ?Sized>(
        queue: &Arc<SpscRing<PlayCommand>>,
        stats: &Arc<InputStats>,
        source: &S,
        key: PhysicalKey,
        variant_states: &mut [VariantState; PhysicalKey::COUNT],
        wake: Option<&dyn StreamWake>,
    ) {
        stats.increment_press();
        let started_ns = kv_core::monotonic_ns();
        let state = &mut variant_states[key.as_u16() as usize];
        if let Some(cmd) = source.play(key, state) {
            if let Some(wake) = wake {
                wake.wake();
            }
            let outcome = queue.push(cmd.stamped(kv_core::monotonic_ns()));
            source.queued(cmd);
            if outcome.is_err() {
                stats.increment_command_dropped();
            } else {
                stats.increment_command_generated();
                stats.record_command_latency(kv_core::monotonic_ns() - started_ns);
            }
        }
    }

    /// Signals the script to stop.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for SimulatedInput {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kv_core::{PhysicalKey, PlayCommand, SoundSource, VariantState};

    struct CountingSource {
        hits: Arc<std::sync::atomic::AtomicU64>,
    }

    impl SoundSource for CountingSource {
        fn play(&self, _key: PhysicalKey, _state: &mut VariantState) -> Option<PlayCommand> {
            self.hits.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    #[test]
    fn test_simulated_input_runs_script() {
        let queue: Arc<SpscRing<PlayCommand>> = Arc::new(SpscRing::with_capacity(64));
        let stats = Arc::new(InputStats::new());
        let hits = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let source = Arc::new(CountingSource { hits: hits.clone() });

        let sim = SimulatedInput::start(
            queue,
            stats.clone(),
            source,
            vec![
                SimStep::Press(PhysicalKey::A),
                SimStep::Wait(Duration::from_millis(1)),
            ],
            3,
            None,
        );
        let _ = sim;

        // Script is finite (3 repeats) so the thread exits on its own.
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(hits.load(Ordering::Relaxed), 3);
        assert_eq!(stats.snapshot().key_presses, 3);
        assert_eq!(stats.snapshot().key_releases, 3);
    }

    #[test]
    fn test_simulated_input_stops_on_drop() {
        let queue: Arc<SpscRing<PlayCommand>> = Arc::new(SpscRing::with_capacity(64));
        let stats = Arc::new(InputStats::new());
        let source = Arc::new(CountingSource {
            hits: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        });

        let sim = SimulatedInput::start(
            queue,
            stats,
            source,
            vec![
                SimStep::Press(PhysicalKey::A),
                SimStep::Wait(Duration::from_millis(50)),
            ],
            0,
            None,
        );
        drop(sim);
    }
}
