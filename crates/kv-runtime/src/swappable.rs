//! A sound source that can be swapped while the engine is running.
//!
//! Switching packs must never stop the audio stream, so the swap cannot
//! simply drop the old pack: the real-time mixer may still be rendering
//! voices whose `PlayCommand::sample_ptr` points into its memory-mapped
//! sample data.
//!
//! [`SwappableSource`] makes the *input* side of the switch instant and
//! lock-free from the real-time thread's point of view (input threads take a
//! short mutex, the audio thread never touches this type), and defers the
//! actual release of the old pack until it is provably unreferenced.
//!
//! A pack may be released when all of these hold:
//!
//! * no producer is between [`SoundSource::play`] and
//!   [`SoundSource::queued`] - producers announce themselves for exactly
//!   that window,
//! * the command queue is empty,
//! * the real-time thread is not inside its process callback (the window in
//!   which a command sits between the queue and a voice),
//! * **either** there are no active voices at all, **or** that pack's own
//!   safety deadline has passed, by which every voice that could have
//!   started before its swap has finished at the slowest pitch the mixer
//!   will use.
//!
//! The deadline is per parked pack, never shared: two swaps in a row park
//! two packs with different deadlines, and releasing the newer one because
//! the older one's deadline came first would unmap sample data the
//! real-time thread is still reading.

use crate::player::PackPlayer;
use kv_audio_pipewire::stream::RtStats;
use kv_core::{PhysicalKey, PlayCommand, SoundSource, VariantState};
use kv_ring::SpscRing;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Extra time granted on top of the longest possible voice.
///
/// Covers one quantum of queue latency and normal scheduling jitter.
const RETIRE_SLACK: Duration = Duration::from_secs(1);

/// A pack-backed source whose current pack can be replaced at any time.
pub struct SwappableSource {
    slot: Mutex<Slot>,
    /// Producers inside `play` .. `queued`. Announced before the lock is
    /// taken so the retire check can never race past one.
    producers: AtomicUsize,
    /// [`Slot::retired`].len() mirrored for lock-free reads, so the release
    /// path can prove there is nothing parked before it takes any lock.
    parked: AtomicUsize,
    /// Live real-time state, installed by the runtime once the engine is up.
    ///
    /// Input threads have no other way to observe it, and it is what turns
    /// [`retire_if_safe`](Self::retire_if_safe) from a guess into a proof.
    release: Mutex<Option<Arc<ReleaseGate>>>,
}

/// The real-time observations a release decision needs.
struct ReleaseGate {
    stats: Arc<RtStats>,
    queue: Arc<SpscRing<PlayCommand>>,
}

#[derive(Default)]
struct Slot {
    /// The pack every new key press is played from.
    current: Option<Arc<PackPlayer>>,
    /// Players retired by a swap; kept alive until it is safe to release.
    retired: Vec<Retired>,
}

struct Retired {
    player: Arc<PackPlayer>,
    /// Earliest moment the pack could possibly still be referenced.
    safe_after: Instant,
}

/// What a [`SwappableSource::swap`] did.
#[derive(Debug, Clone)]
pub struct SwapReport {
    /// Name of the pack that was playing before the swap.
    pub from: String,
    /// Name of the pack that is playing now.
    pub to: String,
    /// Packs waiting to be released (including the one just retired).
    pub retired: usize,
    /// When the retired pack becomes free of any further checks.
    pub safe_after: Instant,
    /// How long the swap itself took.
    pub elapsed: Duration,
}

impl SwappableSource {
    /// Creates a source playing from `current`.
    pub fn new(current: Arc<PackPlayer>) -> Self {
        Self {
            slot: Mutex::new(Slot {
                current: Some(current),
                retired: Vec::new(),
            }),
            producers: AtomicUsize::new(0),
            parked: AtomicUsize::new(0),
            release: Mutex::new(None),
        }
    }

    /// The player every new key press plays from right now.
    pub fn current(&self) -> Arc<PackPlayer> {
        let slot = self.lock();
        slot.current
            .clone()
            .expect("a SwappableSource always has a current pack")
    }

    /// Replaces the pack new key presses play from.
    ///
    /// Returns immediately: the previous pack is parked and released later
    /// by [`retire`](Self::retire).
    pub fn swap(&self, next: Arc<PackPlayer>) -> SwapReport {
        let started = Instant::now();
        let to_name = next.pack().stats().name.clone();

        let mut slot = self.lock();
        let previous = slot.current.replace(next);
        let swapped_at = Instant::now();

        let (from, safe_after) = match previous {
            Some(previous) => {
                let name = previous.pack().stats().name.clone();
                let safe_after = swapped_at + safety_margin(&previous);
                slot.retired.push(Retired {
                    player: previous,
                    safe_after,
                });
                (name, safe_after)
            }
            None => ("<none>".to_string(), swapped_at),
        };
        let retired = slot.retired.len();
        self.parked.store(retired, Ordering::Release);
        drop(slot);

        SwapReport {
            from,
            to: to_name,
            retired,
            safe_after,
            elapsed: started.elapsed(),
        }
    }

    /// How many producers are between `play` and `queued` right now.
    pub fn producers(&self) -> usize {
        self.producers.load(Ordering::SeqCst)
    }

    /// How many packs are parked waiting to be released.
    pub fn retired(&self) -> usize {
        self.lock().retired.len()
    }

    /// The moment the oldest parked pack becomes free of every check.
    ///
    /// Diagnostics only: a release decision never consults this, because the
    /// oldest deadline proves nothing about the packs parked after it.
    ///
    /// `None` when nothing is parked.
    pub fn safety_deadline(&self) -> Option<Instant> {
        self.lock()
            .retired
            .iter()
            .map(|entry| entry.safe_after)
            .min()
    }

    /// Installs the live real-time state a release decision needs.
    ///
    /// The runtime calls this once the audio engine is up. Before it, only
    /// an explicit [`retire`](Self::retire) can release anything: input
    /// threads cannot observe the stream for themselves.
    pub fn attach(&self, stats: Arc<RtStats>, queue: Arc<SpscRing<PlayCommand>>) {
        let mut release = self
            .release
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *release = Some(Arc::new(ReleaseGate { stats, queue }));
    }

    /// Releases parked packs that are provably unreferenced.
    ///
    /// The caller must already have established that no producer is inside
    /// `play` .. `queued`, the command queue is empty, and the real-time
    /// thread is not inside its process callback - see
    /// [`retire_if_safe`](Self::retire_if_safe), which does all of it.
    ///
    /// * `voices_clear` - the mixer is rendering nothing at all, so no pack
    ///   can be read any more and every parked pack goes.
    /// * otherwise - only packs whose **own** safety deadline has passed go.
    ///   A pack parked moments ago may still be sounding while one parked
    ///   earlier has finished; releasing them together would unmap sample
    ///   data the real-time thread is still reading.
    ///
    /// Returns how many packs were released.
    pub fn retire(&self, voices_clear: bool) -> usize {
        let mut slot = self.lock();
        let before = slot.retired.len();
        if voices_clear {
            slot.retired.clear();
        } else {
            let now = Instant::now();
            slot.retired.retain(|entry| now < entry.safe_after);
        }
        self.parked.store(slot.retired.len(), Ordering::Release);
        before - slot.retired.len()
    }

    /// Releases parked packs from an input thread, when it is safe.
    ///
    /// Called after every command a producer queues - the place a swap's
    /// parked pack is most likely to be forgotten. The fast path is one
    /// atomic load, so the usual case (nothing parked) costs nothing.
    ///
    /// Returns how many packs were released.
    pub fn retire_if_safe(&self) -> usize {
        if self.parked.load(Ordering::Acquire) == 0 {
            return 0;
        }
        let gate = self
            .release
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let Some(gate) = gate else {
            return 0;
        };
        let stats = gate.stats.snapshot();
        let producers_clear = self.producers.load(Ordering::SeqCst) == 0;
        let queue_clear = gate.queue.is_empty();
        // The one window in which a command has left the queue but has not
        // become a voice yet: only observable while the data thread is in
        // its callback.
        let not_mid_callback = !stats.in_callback;
        if !(producers_clear && queue_clear && not_mid_callback) {
            return 0;
        }
        self.retire(stats.active_voices == 0)
    }

    /// Names of the parked packs, oldest first (diagnostics and tests).
    pub fn retired_names(&self) -> Vec<String> {
        let slot = self.lock();
        slot.retired
            .iter()
            .map(|entry| entry.player.pack().stats().name.clone())
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(|error| error.into_inner())
    }
}

impl SoundSource for SwappableSource {
    #[inline]
    fn play(&self, key: PhysicalKey, state: &mut VariantState) -> Option<PlayCommand> {
        // Announced before the lock so a retire check that already holds the
        // lock still sees us if we are about to read `current`.
        self.producers.fetch_add(1, Ordering::SeqCst);
        let command = {
            let slot = self.lock();
            match slot.current.as_ref() {
                Some(player) => player.play(key, state),
                None => None,
            }
        };
        if command.is_none() {
            self.producers.fetch_sub(1, Ordering::SeqCst);
        }
        command
    }

    fn queued(&self, _command: PlayCommand) {
        self.producers.fetch_sub(1, Ordering::SeqCst);
        // Where a swap's parked pack is released in production: an input
        // thread is running here anyway, and it is the only control-plane
        // hook that fires once per press while a pack is parked. The fast
        // path is a single atomic load when nothing is parked.
        self.retire_if_safe();
    }
}

/// Time after which no voice started before a swap can still be sounding.
///
/// The mixer advances a voice at `clip / source_rate / pitch_ratio` seconds
/// of output per clip; pitch variation is bounded, and the slowest legal
/// playback is taken as half speed to stay conservative even if a future
/// pack requests a wider range. One extra second covers queue latency and
/// scheduling jitter.
fn safety_margin(player: &PackPlayer) -> Duration {
    let pack = player.pack();
    let stats = pack.stats();
    let rate = stats.sample_rate.max(1);

    let mut max_frames = 0u32;
    for clip in pack.clips() {
        max_frames = max_frames.max(clip.sample_frames);
    }

    let seconds = (max_frames as f64 / f64::from(rate)) * 2.0;
    Duration::from_secs_f64(seconds) + RETIRE_SLACK
}

#[cfg(test)]
mod tests {
    use super::*;
    use kv_core::Settings;
    use kv_pack::{ClipData, PackBuilder};

    fn build_player(name: &str) -> Arc<PackPlayer> {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let mut builder = PackBuilder::new();
        builder.name = name.to_string();
        builder
            .add_clip(
                PhysicalKey::A,
                ClipData::from_samples(vec![100i16; 64], 48000).unwrap(),
            )
            .unwrap();

        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "kvpack-swappable-{}-{id}.kvpack",
            std::process::id()
        ));
        builder.write(&path).unwrap();
        let pack = Arc::new(kv_pack::KvPack::open(&path).unwrap());
        let _ = std::fs::remove_file(&path);

        let mut player = PackPlayer::new(pack, 48000);
        player.spatial(Settings::default().spatial_audio_enabled);
        Arc::new(player)
    }

    #[test]
    fn swap_changes_the_pack_and_parks_the_old_one() {
        let source = SwappableSource::new(build_player("One"));
        assert_eq!(source.current().pack().stats().name, "One");

        let report = source.swap(build_player("Two"));
        assert_eq!(report.from, "One");
        assert_eq!(report.to, "Two");
        assert_eq!(source.current().pack().stats().name, "Two");
        assert_eq!(source.retired(), 1);
        assert_eq!(source.retired_names(), vec!["One".to_string()]);
    }

    #[test]
    fn retire_only_drops_parks_when_allowed() {
        let source = SwappableSource::new(build_player("One"));
        let _ = source.swap(build_player("Two"));

        assert_eq!(source.retire(false), 0, "never release unsafely");
        assert_eq!(source.retired(), 1);
        assert_eq!(source.retire(true), 1);
        assert_eq!(source.retired(), 0);
    }

    #[test]
    fn play_announces_the_producer_until_queued() {
        let source = Arc::new(SwappableSource::new(build_player("One")));
        let mut state = VariantState::default();

        assert_eq!(source.producers(), 0);
        let command = source.play(PhysicalKey::A, &mut state).expect("has A");
        assert_eq!(source.producers(), 1, "held across play -> queued");
        source.queued(command);
        assert_eq!(source.producers(), 0);
    }

    #[test]
    fn a_key_without_a_sound_releases_immediately() {
        let source = SwappableSource::new(build_player("One"));
        let mut state = VariantState::default();
        assert!(source.play(PhysicalKey::Q, &mut state).is_none());
        assert_eq!(source.producers(), 0, "no queued() follows a None");
    }

    #[test]
    fn safety_deadline_is_at_least_one_second_out() {
        let source = SwappableSource::new(build_player("One"));
        let _ = source.swap(build_player("Two"));
        let deadline = source.safety_deadline().expect("one pack is parked");
        assert!(deadline >= Instant::now() + Duration::from_secs(1));
    }

    #[test]
    fn repeated_swaps_park_every_previous_pack() {
        let source = SwappableSource::new(build_player("One"));
        let _ = source.swap(build_player("Two"));
        let _ = source.swap(build_player("Three"));
        assert_eq!(source.retired(), 2);
        assert_eq!(
            source.retired_names(),
            vec!["One".to_string(), "Two".to_string()]
        );
    }

    #[test]
    fn retire_does_not_drop_newer_park_when_older_deadline_passes() {
        let source = SwappableSource::new(build_player("One"));
        let _ = source.swap(build_player("Two"));
        // Artificially expire the first parked pack's deadline to simulate time passing.
        {
            let mut slot = source.lock();
            slot.retired[0].safe_after = Instant::now() - Duration::from_millis(10);
        }

        // Now swap to Three. Two gets a brand-new safe_after (~1s in future).
        let _ = source.swap(build_player("Three"));
        assert_eq!(source.retired(), 2);

        // Calling retire(false) (voices not clear) must ONLY drop "One", keeping "Two".
        let released = source.retire(false);
        assert_eq!(released, 1, "only the expired pack was released");
        assert_eq!(source.retired(), 1, "newer pack must still be parked");
        assert_eq!(source.retired_names(), vec!["Two".to_string()]);
    }
}
