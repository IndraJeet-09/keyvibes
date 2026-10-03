//! Phase 1: Mixer testing with synthetic sources.
//!
//! This test module validates the mixer without requiring keyboard input
//! or PipeWire. We generate synthetic waveforms and verify the mixer
//! produces correct output.

use kv_core::PlayCommand;
use kv_mixer::Mixer;
use kv_ring::SpscRing;

/// Generates a simple sine wave at the given frequency.
fn generate_sine_wave(freq: f32, sample_rate: u32, duration_secs: f32) -> Vec<i16> {
    let sample_count = (sample_rate as f32 * duration_secs) as usize;
    let mut samples = Vec::with_capacity(sample_count + 6); // +6 for guard samples

    // 2 guard samples at start
    samples.push(0);
    samples.push(0);

    // Generate sine wave
    for i in 0..sample_count {
        let t = i as f32 / sample_rate as f32;
        let sample = (2.0 * std::f32::consts::PI * freq * t).sin() * 16000.0;
        samples.push(sample as i16);
    }

    // 3 guard samples at end
    samples.push(0);
    samples.push(0);
    samples.push(0);

    samples
}

/// Generates a short click/impulse.
fn generate_click() -> Vec<i16> {
    vec![
        0, 0,           // guard samples
        16000, 8000, -8000, -4000, 2000, -1000, 500, -250, 0,
        0, 0, 0,        // guard samples
    ]
}

/// Generates silence.
fn generate_silence(sample_count: usize) -> Vec<i16> {
    vec![0; sample_count + 5]
}

#[test]
fn test_single_voice_sine() {
    // Generate a 440Hz sine wave (A4)
    let samples = generate_sine_wave(440.0, 48000, 0.1);
    let mut mixer = Mixer::new(48000);

    // Create a play command
    let cmd = unsafe {
        PlayCommand::new(
            samples[2..].as_ptr(), // Skip guard samples
            (samples.len() - 5) as u32,
            48000,
            1u64 << 32, // 1.0 playback rate
            1.0,        // left gain
            1.0,        // right gain
            false,
        )
    };

    mixer.trigger(cmd);

    // Render a few frames
    let mut output = [0.0f32; 256];
    unsafe {
        mixer.render_block(&mut output);
    }

    // Should have non-zero output
    let has_signal = output.iter().any(|&s| s.abs() > 0.01);
    assert!(has_signal, "Expected non-zero output from sine wave");

    // All samples should be finite
    assert!(output.iter().all(|&s| s.is_finite()));
}

#[test]
fn test_single_voice_click() {
    let samples = generate_click();
    let mut mixer = Mixer::new(48000);

    let cmd = unsafe {
        PlayCommand::new(
            samples[2..].as_ptr(),
            (samples.len() - 5) as u32,
            48000,
            1u64 << 32,
            1.0,
            1.0,
            false,
        )
    };

    mixer.trigger(cmd);

    let mut output = [0.0f32; 64];
    unsafe {
        mixer.render_block(&mut output);
    }

    // Should have a transient
    let peak = output.iter().map(|&s| s.abs()).fold(0.0f32, f32::max);
    assert!(peak > 0.1, "Expected strong transient from click, got peak={}", peak);
}

#[test]
fn test_eight_overlapping_voices() {
    let samples = generate_click();
    let mut mixer = Mixer::new(48000);

    // Trigger 8 voices
    for _ in 0..8 {
        let cmd = unsafe {
            PlayCommand::new(
                samples[2..].as_ptr(),
                (samples.len() - 5) as u32,
                48000,
                1u64 << 32,
                0.5, // Reduce gain to avoid clipping
                0.5,
                false,
            )
        };
        mixer.trigger(cmd);
    }

    assert_eq!(mixer.active_voice_count(), 8);

    let mut output = [0.0f32; 128];
    unsafe {
        mixer.render_block(&mut output);
    }

    // Should produce audible output
    let rms = (output.iter().map(|&s| s * s).sum::<f32>() / output.len() as f32).sqrt();
    assert!(rms > 0.05, "Expected significant RMS from 8 voices, got {}", rms);

    // Should not clip (limiter should prevent this)
    let peak = output.iter().map(|&s| s.abs()).fold(0.0f32, f32::max);
    assert!(peak <= 1.0, "Output should be limited to ±1.0, got peak={}", peak);
}

#[test]
fn test_thirty_two_simultaneous_voices() {
    let samples = generate_click();
    let mut mixer = Mixer::new(48000);

    // Trigger 32 voices (full polyphony)
    for _ in 0..32 {
        let cmd = unsafe {
            PlayCommand::new(
                samples[2..].as_ptr(),
                (samples.len() - 5) as u32,
                48000,
                1u64 << 32,
                0.25, // Lower gain
                0.25,
                false,
            )
        };
        mixer.trigger(cmd);
    }

    assert_eq!(mixer.active_voice_count(), 32);

    let mut output = [0.0f32; 256];
    unsafe {
        mixer.render_block(&mut output);
    }

    // All output should be finite
    assert!(output.iter().all(|&s| s.is_finite()));

    // Should produce output
    let has_signal = output.iter().any(|&s| s.abs() > 0.01);
    assert!(has_signal);

    // Limiter should prevent clipping
    let peak = output.iter().map(|&s| s.abs()).fold(0.0f32, f32::max);
    assert!(peak <= 1.0, "Peak should be limited, got {}", peak);
}

#[test]
fn test_voice_stealing() {
    let samples = generate_click();
    let mut mixer = Mixer::new(48000);

    // Fill all 32 voices
    for _ in 0..32 {
        let cmd = unsafe {
            PlayCommand::new(
                samples[2..].as_ptr(),
                (samples.len() - 5) as u32,
                48000,
                1u64 << 32,
                0.5,
                0.5,
                false,
            )
        };
        mixer.trigger(cmd);
    }

    assert_eq!(mixer.active_voice_count(), 32);

    // Trigger one more - should steal the oldest
    let cmd = unsafe {
        PlayCommand::new(
            samples[2..].as_ptr(),
            (samples.len() - 5) as u32,
            48000,
            1u64 << 32,
            0.5,
            0.5,
            false,
        )
    };
    mixer.trigger(cmd);

    // Still 32 voices (one was stolen)
    assert_eq!(mixer.active_voice_count(), 32);
}

#[test]
fn test_pitch_variation() {
    let samples = generate_sine_wave(440.0, 48000, 0.1);
    let mut mixer = Mixer::new(48000);

    // Play at 2x speed (one octave up)
    let cmd = unsafe {
        PlayCommand::new(
            samples[2..].as_ptr(),
            (samples.len() - 5) as u32,
            48000,
            2u64 << 32, // 2.0 playback rate
            1.0,
            1.0,
            false,
        )
    };

    mixer.trigger(cmd);

    let mut output = [0.0f32; 128];
    unsafe {
        mixer.render_block(&mut output);
    }

    // Should complete faster (voice should be done sooner)
    assert!(output.iter().all(|&s| s.is_finite()));
}

#[test]
fn test_sample_rate_conversion() {
    // 44.1kHz source, 48kHz output
    let samples = generate_sine_wave(440.0, 44100, 0.1);
    let mut mixer = Mixer::new(48000);

    // Calculate step for 44.1 -> 48kHz conversion
    let step = ((44100u64 << 32) / 48000) as u64;

    let cmd = unsafe {
        PlayCommand::new(
            samples[2..].as_ptr(),
            (samples.len() - 5) as u32,
            44100,
            step,
            1.0,
            1.0,
            false,
        )
    };

    mixer.trigger(cmd);

    let mut output = [0.0f32; 256];
    unsafe {
        mixer.render_block(&mut output);
    }

    // Should resample correctly
    assert!(output.iter().all(|&s| s.is_finite()));
    let has_signal = output.iter().any(|&s| s.abs() > 0.01);
    assert!(has_signal);
}

#[test]
fn test_stereo_panning() {
    let samples = generate_click();
    let mut mixer = Mixer::new(48000);

    // Pan hard left
    let cmd_left = unsafe {
        PlayCommand::new(
            samples[2..].as_ptr(),
            (samples.len() - 5) as u32,
            48000,
            1u64 << 32,
            1.0, // left
            0.0, // right
            false,
        )
    };

    mixer.trigger(cmd_left);

    let mut output = [0.0f32; 64];
    unsafe {
        mixer.render_block(&mut output);
    }

    // Check that left channel has more energy than right
    let left_energy: f32 = output.iter().step_by(2).map(|&s| s * s).sum();
    let right_energy: f32 = output.iter().skip(1).step_by(2).map(|&s| s * s).sum();

    assert!(left_energy > right_energy * 10.0,
            "Left channel should dominate, left={}, right={}", left_energy, right_energy);
}

#[test]
fn test_queue_integration() {
    let samples = generate_click();
    let queue: SpscRing<PlayCommand> = SpscRing::with_capacity(256);
    let mut mixer = Mixer::new(48000);

    // Push 10 commands
    for _ in 0..10 {
        let cmd = unsafe {
            PlayCommand::new(
                samples[2..].as_ptr(),
                (samples.len() - 5) as u32,
                48000,
                1u64 << 32,
                0.5,
                0.5,
                false,
            )
        };
        queue.push(cmd).expect("Queue should have space");
    }

    // All commands should be consumed
    assert!(!queue.is_empty(), "Queue should have 10 commands");

    // Process block (should drain queue and render)
    let mut output = [0.0f32; 64]; // Shorter to catch voices before they finish
    unsafe {
        mixer.process_block(&queue, &mut output);
    }

    // All commands should be consumed
    assert!(queue.is_empty());

    // Should have triggered voices (some may have finished in the 64 samples)
    // At minimum, check that we had some output
    let has_output = output.iter().any(|&s| s.abs() > 0.001);
    assert!(has_output, "Expected audio output from queued commands");
}

#[test]
fn test_silence_when_idle() {
    let mut mixer = Mixer::new(48000);

    // No voices triggered
    let mut output = [0.0f32; 128];
    unsafe {
        mixer.render_block(&mut output);
    }

    // Should produce silence
    for &sample in &output {
        assert!(sample.abs() < 0.0001, "Expected silence, got {}", sample);
    }
}