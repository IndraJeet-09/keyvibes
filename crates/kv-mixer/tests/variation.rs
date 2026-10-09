//! Phase 7 gate: variant rotation.
//!
//! `cargo test -p kv-mixer variant_rotation`
//!
//! Repeated presses of the same clip must never sound identical, and the
//! rotator that guarantees this must cover every preset without ever picking
//! the same one twice in a row.

use kv_mixer::{Mixer, MixerSettings, VariationConfig, VariationState, VARIATION_PRESETS};

fn cmd(samples: &[i16]) -> kv_core::PlayCommand {
    unsafe { kv_core::PlayCommand::new(samples.as_ptr(), 4, 48000, 1u64 << 32, 1.0, 1.0, false) }
}

#[test]
fn variant_rotation_never_repeats_consecutively() {
    let mut state = VariationState::new(1);
    let config = VariationConfig::default();

    let mut previous = state.next(&config).preset;
    for _ in 0..(VARIATION_PRESETS as usize * 16) {
        let next = state.next(&config).preset;
        assert_ne!(
            next, previous,
            "preset {next} selected twice in a row (prev {previous})"
        );
        previous = next;
    }
}

#[test]
fn variant_rotation_reaches_every_preset() {
    let mut state = VariationState::new(1);
    let config = VariationConfig::default();

    let mut seen = [false; VARIATION_PRESETS as usize];
    for _ in 0..(VARIATION_PRESETS as usize * 4) {
        seen[state.next(&config).preset as usize] = true;
    }
    assert!(
        seen.iter().all(|&s| s),
        "rotation must reach all {VARIATION_PRESETS} presets, got {seen:?}"
    );
}

#[test]
fn variant_rotation_advances_on_every_trigger() {
    let samples = [12000i16; 8];
    let mut mixer = Mixer::new(48000);

    let mut previous = mixer.variation_state().last_preset();
    for _ in 0..(VARIATION_PRESETS as usize * 4) {
        mixer.trigger(cmd(&samples));
        let preset = mixer.variation_state().last_preset();
        assert_ne!(
            preset, previous,
            "the mixer must rotate the preset on every trigger"
        );
        previous = preset;
    }
    assert_eq!(mixer.active_voice_count(), VARIATION_PRESETS as usize * 4);
}

#[test]
fn variant_rotation_varied_voices_differ_from_each_other() {
    let samples = [12000i16; 8];
    let mut mixer = Mixer::new(48000);

    for _ in 0..8 {
        mixer.trigger(cmd(&samples));
    }

    let first_step = mixer.voice(0).expect("voice 0 allocated").step;
    let first_gain = mixer.voice(0).expect("voice 0 allocated").left_gain;

    let differing_steps = (0..8)
        .filter(|&i| mixer.voice(i).expect("voice allocated").step != first_step)
        .count();
    let differing_gains = (0..8)
        .filter(|&i| (mixer.voice(i).expect("voice allocated").left_gain - first_gain).abs() > 1e-6)
        .count();

    assert!(
        differing_steps > 0 && differing_gains > 0,
        "variation must change both pitch and gain across triggers \
         (steps differing: {differing_steps}, gains differing: {differing_gains})"
    );
}

#[test]
fn variant_rotation_disabled_keeps_voices_identical() {
    let samples = [12000i16; 8];
    let mut mixer = Mixer::with_seed(
        48000,
        MixerSettings {
            master_gain: 1.0,
            variation: VariationConfig::none(),
        },
        7,
    );

    for _ in 0..8 {
        mixer.trigger(cmd(&samples));
    }

    let first = (
        mixer.voice(0).expect("voice 0").step,
        mixer.voice(0).unwrap().left_gain,
    );
    for i in 1..8 {
        let voice = mixer.voice(i).expect("voice allocated");
        assert_eq!(voice.step, first.0, "voice {i} stepped differently");
        assert_eq!(voice.left_gain, first.1, "voice {i} gained differently");
    }
}

#[test]
fn variant_rotation_presets_differ_from_jitter_alone() {
    // The preset is the deterministic part of the rotation: with jitter
    // removed by a zero range, presets still advance but values stay at unity.
    let mut state = VariationState::new(3);
    let config = VariationConfig::none();

    let mut presets = Vec::new();
    for _ in 0..VARIATION_PRESETS {
        let v = state.next(&config);
        presets.push(v.preset);
        assert_eq!(v.pitch_ratio(), 1.0);
        assert_eq!(v.gain(), 1.0);
    }

    let unique: std::collections::BTreeSet<_> = presets.iter().copied().collect();
    assert_eq!(unique.len(), VARIATION_PRESETS as usize);
}
