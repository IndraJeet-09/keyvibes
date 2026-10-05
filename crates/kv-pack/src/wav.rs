//! WAV decoding, validation, and canonicalization.
//!
//! This module is builder-only: the runtime never sees WAV files. It converts
//! source audio into the canonical KVPack v1 representation:
//!
//! ```text
//! mono · PCM i16 · little-endian · explicit pack sample rate
//! ```
//!
//! Guard samples are *not* added here; they are applied later when a
//! `ClipData` is constructed, so `CanonicalPcm` always holds logical frames.

use crate::error::{PackError, PackResult};
use crate::format::*;
use std::path::Path;

/// Canonical (logical, guard-free) PCM for one clip.
#[derive(Debug, Clone)]
pub struct CanonicalPcm {
    /// Mono i16 samples (logical frames only).
    pub samples: Vec<i16>,
    /// Sample rate of the source.
    pub sample_rate: u32,
    /// Number of logical frames (`samples.len()`).
    pub frames: u32,
}

/// Source audio decoded to normalized `f32`, still in its original channel
/// layout (interleaved).
///
/// This is the input of the Phase 5 processing pipeline: `decode_wav` produces
/// it, [`SourceAudio::to_mono`] converts it to [`MonoAudio`], and
/// `AudioProcessor` takes it from there. The legacy `load_wav` path never
/// constructs this type.
#[derive(Debug, Clone)]
pub struct SourceAudio {
    /// Interleaved samples in `[-1.0, +1.0]`, `frames * channels` long.
    pub samples: Vec<f32>,
    /// Sample rate of the source.
    pub sample_rate: u32,
    /// Channel count of the source (1 or 2).
    pub channels: u16,
    /// Human-readable encoding, e.g. `"16-bit PCM"`.
    pub encoding: String,
}

impl SourceAudio {
    /// Number of frames (`samples.len() / channels`).
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }

    /// Downmixes to mono by averaging the channels of each frame.
    ///
    /// Mono is returned unchanged. The reported `channels` of the result is
    /// still the **source** channel count, so processing reports can say what
    /// the file actually contained.
    pub fn to_mono(&self) -> MonoAudio {
        let mono = if self.channels <= 1 {
            self.samples.clone()
        } else {
            let channels = self.channels as usize;
            self.samples
                .chunks_exact(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32)
                .collect()
        };
        MonoAudio {
            samples: mono,
            sample_rate: self.sample_rate,
            channels: self.channels,
        }
    }
}

/// Mono `f32` audio ready for `AudioProcessor::process`.
///
/// `channels` is the *source* channel count kept for reporting; `samples` is
/// always mono.
#[derive(Debug, Clone)]
pub struct MonoAudio {
    /// Mono samples in `[-1.0, +1.0]`.
    pub samples: Vec<f32>,
    /// Sample rate of the source.
    pub sample_rate: u32,
    /// Source channel count (1 or 2).
    pub channels: u16,
}

/// Maps a `hound` error to a more actionable pack error.
fn map_wav_error(e: hound::Error) -> PackError {
    match e {
        hound::Error::IoError(e) => PackError::Io(e),
        hound::Error::Unsupported => PackError::UnsupportedFormat(
            "unsupported WAV encoding (only PCM and IEEE float are supported)".to_string(),
        ),
        hound::Error::FormatError(msg) => {
            PackError::UnsupportedFormat(format!("invalid WAV header: {msg}"))
        }
        hound::Error::InvalidSampleFormat => PackError::UnsupportedFormat(
            "WAV sample format does not match the requested decoding format".to_string(),
        ),
        other => PackError::Wav(other),
    }
}

/// Describes an unsupported (sample format, bit depth) combination.
fn describe_encoding(format: hound::SampleFormat, bits: u16) -> String {
    match format {
        hound::SampleFormat::Int => format!("{bits}-bit PCM"),
        hound::SampleFormat::Float => format!("{bits}-bit IEEE float"),
    }
}

/// Loads a WAV file and converts it to canonical mono i16 PCM.
///
/// # Supported sources
///
/// | Encoding            | Channels | Conversion            |
/// |---------------------|----------|-----------------------|
/// | PCM 8-bit unsigned  | 1 or 2   | sign + scale          |
/// | PCM 16-bit          | 1 or 2   | direct                |
/// | PCM 24-bit          | 1 or 2   | truncate to 16-bit    |
/// | PCM 32-bit           | 1 or 2   | truncate to 16-bit    |
/// | IEEE float 32-bit   | 1 or 2   | clamp + quantize      |
///
/// Stereo is downmixed with `(L + R) / 2`. More than two channels is rejected
/// rather than blindly averaged.
///
/// # Errors
///
/// Returns an actionable error for corrupt/truncated/empty files, unsupported
/// encodings, out-of-range sample rates, and non-finite float samples.
pub fn load_wav(path: &Path) -> PackResult<CanonicalPcm> {
    let mut reader = hound::WavReader::open(path).map_err(map_wav_error)?;
    let spec = reader.spec();

    if spec.channels == 0 {
        return Err(PackError::UnsupportedFormat(
            "WAV file declares zero channels".to_string(),
        ));
    }
    if spec.channels > 2 {
        return Err(PackError::UnsupportedFormat(format!(
            "unsupported channel layout: {} channels (only mono and stereo are supported)",
            spec.channels
        )));
    }

    if spec.sample_rate < MIN_SAMPLE_RATE || spec.sample_rate > MAX_SAMPLE_RATE {
        return Err(PackError::InvalidSampleRate(
            spec.sample_rate,
            MIN_SAMPLE_RATE,
            MAX_SAMPLE_RATE,
        ));
    }

    let expected_frames = reader.duration() as u64;
    if expected_frames == 0 {
        return Err(PackError::ZeroLengthClip(0));
    }
    if expected_frames > MAX_CLIP_FRAMES as u64 {
        return Err(PackError::ClipTooLarge {
            clip: 0,
            frames: expected_frames as u32,
            max: MAX_CLIP_FRAMES,
        });
    }

    let channels = spec.channels as usize;

    let mono: Vec<i16> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 8 | 16 | 24 | 32) => {
            let raw: Vec<i32> = reader
                .samples::<i32>()
                .collect::<Result<_, _>>()
                .map_err(map_wav_error)?;
            check_frame_count(raw.len(), expected_frames, channels)?;
            let scaled: Vec<i16> = match spec.bits_per_sample {
                8 => raw.iter().map(|&v| (v as i16).wrapping_mul(256)).collect(),
                16 => raw.iter().map(|&v| v as i16).collect(),
                24 => raw.iter().map(|&v| (v >> 8) as i16).collect(),
                _ => raw.iter().map(|&v| (v >> 16) as i16).collect(),
            };
            downmix_i16(scaled, channels)
        }
        (hound::SampleFormat::Float, 32) => {
            let raw: Vec<f32> = reader
                .samples::<f32>()
                .collect::<Result<_, _>>()
                .map_err(map_wav_error)?;
            check_frame_count(raw.len(), expected_frames, channels)?;
            let mixed = downmix_f32(raw, channels)?;
            mixed
                .iter()
                .map(|&s| quantize_f32(s))
                .collect::<PackResult<Vec<i16>>>()?
        }
        (format, bits) => {
            return Err(PackError::UnsupportedFormat(format!(
                "unsupported WAV encoding: {}",
                describe_encoding(format, bits)
            )));
        }
    };

    if mono.is_empty() {
        return Err(PackError::ZeroLengthClip(0));
    }
    if mono.len() > MAX_CLIP_FRAMES as usize {
        return Err(PackError::ClipTooLarge {
            clip: 0,
            frames: mono.len() as u32,
            max: MAX_CLIP_FRAMES,
        });
    }

    Ok(CanonicalPcm {
        frames: mono.len() as u32,
        sample_rate: spec.sample_rate,
        samples: mono,
    })
}

/// Writes mono i16 samples as a 16-bit PCM WAV file.
///
/// Used by `keyvibes process` to render a processed clip back to disk so it
/// can be auditioned in any audio tool. The sample rate is written as given
/// (processing never resamples).
pub fn write_wav_i16(path: &Path, sample_rate: u32, samples: &[i16]) -> PackResult<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(map_wav_error)?;
    for &s in samples {
        writer.write_sample(s).map_err(map_wav_error)?;
    }
    writer.finalize().map_err(map_wav_error)?;
    Ok(())
}

/// Verifies that the decoded sample count matches the declared frame count.
fn check_frame_count(actual: usize, expected_frames: u64, channels: usize) -> PackResult<()> {
    let expected = expected_frames
        .checked_mul(channels as u64)
        .ok_or(PackError::IntegerOverflow)?;

    if actual as u64 != expected {
        return Err(PackError::TruncatedAudio {
            expected: expected_frames,
            actual: (actual / channels.max(1)) as u64,
        });
    }
    Ok(())
}

/// Same source validation as [`load_wav`]: channels, sample rate, and frame
/// count bounds. Returns the channel count as `usize`.
fn validate_source(spec: &hound::WavSpec, expected_frames: u64) -> PackResult<usize> {
    if spec.channels == 0 {
        return Err(PackError::UnsupportedFormat(
            "WAV file declares zero channels".to_string(),
        ));
    }
    if spec.channels > 2 {
        return Err(PackError::UnsupportedFormat(format!(
            "unsupported channel layout: {} channels (only mono and stereo are supported)",
            spec.channels
        )));
    }
    if spec.sample_rate < MIN_SAMPLE_RATE || spec.sample_rate > MAX_SAMPLE_RATE {
        return Err(PackError::InvalidSampleRate(
            spec.sample_rate,
            MIN_SAMPLE_RATE,
            MAX_SAMPLE_RATE,
        ));
    }
    if expected_frames == 0 {
        return Err(PackError::ZeroLengthClip(0));
    }
    if expected_frames > MAX_CLIP_FRAMES as u64 {
        return Err(PackError::ClipTooLarge {
            clip: 0,
            frames: expected_frames as u32,
            max: MAX_CLIP_FRAMES,
        });
    }
    Ok(spec.channels as usize)
}

/// Loads a WAV file as normalized `f32` [`SourceAudio`] for processing.
///
/// This is the Phase 5 decode path. It shares all source validation with
/// [`load_wav`] but never quantizes: samples stay in `f32` so the DSP stages
/// see full precision. Integer formats are normalized by their full-scale
/// value:
///
/// | Encoding          | Normalization     |
/// |-------------------|-------------------|
/// | PCM 8-bit signed  | `/ 128`           |
/// | PCM 16-bit        | `/ 32768`         |
/// | PCM 24-bit        | `/ 8388608`       |
/// | PCM 32-bit        | `/ 2147483648`    |
/// | IEEE float 32-bit | as stored         |
///
/// Float samples must be finite; anything else is rejected. Channels stay
/// interleaved — downmixing happens in [`SourceAudio::to_mono`].
pub fn decode_wav(path: &Path) -> PackResult<SourceAudio> {
    let mut reader = hound::WavReader::open(path).map_err(map_wav_error)?;
    let spec = reader.spec();
    let channels = validate_source(&spec, reader.duration() as u64)?;

    let samples: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, bits @ (8 | 16 | 24 | 32)) => {
            let raw: Vec<i32> = reader
                .samples::<i32>()
                .collect::<Result<_, _>>()
                .map_err(map_wav_error)?;
            check_frame_count(raw.len(), reader.duration() as u64, channels)?;
            let scale = match bits {
                8 => 128.0,
                16 => 32768.0,
                24 => 8_388_608.0,
                _ => 2_147_483_648.0,
            };
            raw.into_iter().map(|v| v as f32 / scale).collect()
        }
        (hound::SampleFormat::Float, 32) => {
            let raw: Vec<f32> = reader
                .samples::<f32>()
                .collect::<Result<_, _>>()
                .map_err(map_wav_error)?;
            check_frame_count(raw.len(), reader.duration() as u64, channels)?;
            for (i, &s) in raw.iter().enumerate() {
                if !s.is_finite() {
                    return Err(PackError::NonFiniteSample(format!(
                        "sample index {i} = {s}"
                    )));
                }
            }
            raw
        }
        (format, bits) => {
            return Err(PackError::UnsupportedFormat(format!(
                "unsupported WAV encoding: {}",
                describe_encoding(format, bits)
            )));
        }
    };

    Ok(SourceAudio {
        samples,
        sample_rate: spec.sample_rate,
        channels: spec.channels,
        encoding: describe_encoding(spec.sample_format, spec.bits_per_sample),
    })
}

/// Downmixes interleaved i16 samples to mono.
///
/// Stereo uses `(L + R) / 2` computed in `i32` so the sum can never overflow.
fn downmix_i16(interleaved: Vec<i16>, channels: usize) -> Vec<i16> {
    if channels <= 1 {
        return interleaved;
    }

    interleaved
        .chunks_exact(channels)
        .map(|frame| {
            let sum: i32 = frame.iter().map(|&s| s as i32).sum();
            (sum / channels as i32) as i16
        })
        .collect()
}

/// Downmixes interleaved f32 samples to mono (average of all channels).
fn downmix_f32(interleaved: Vec<f32>, channels: usize) -> PackResult<Vec<f32>> {
    for (i, &s) in interleaved.iter().enumerate() {
        if !s.is_finite() {
            return Err(PackError::NonFiniteSample(format!(
                "sample index {i} = {s}"
            )));
        }
    }

    if channels <= 1 {
        return Ok(interleaved);
    }

    Ok(interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect())
}

/// Quantizes a normalized float sample to i16.
///
/// ```text
/// finite check → clamp to [-1, +1] → scale → round → clamp → i16
/// ```
///
/// NaN and ±Inf are rejected explicitly; they are never wrapped or clamped
/// into the pack.
pub fn quantize_f32(sample: f32) -> PackResult<i16> {
    if !sample.is_finite() {
        return Err(PackError::NonFiniteSample(format!("{sample}")));
    }

    let clamped = sample.clamp(-1.0, 1.0);
    // Scale by 32768 so that -1.0 maps to i16::MIN, then clamp the +1.0 edge
    // (which would be 32768) back into range.
    let scaled = (clamped * 32768.0).round();
    Ok(scaled.clamp(i16::MIN as f32, i16::MAX as f32) as i16)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav(path: &Path, spec: hound::WavSpec, frames: &[(i16, i16)]) {
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for &(l, r) in frames {
            if spec.channels == 2 {
                writer.write_sample(l).unwrap();
                writer.write_sample(r).unwrap();
            } else {
                writer.write_sample(l).unwrap();
            }
        }
        writer.finalize().unwrap();
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("kvpack-wav-test-{}-{}", std::process::id(), name));
        p
    }

    #[test]
    fn test_quantize_basic() {
        assert_eq!(quantize_f32(0.0).unwrap(), 0);
        assert_eq!(quantize_f32(1.0).unwrap(), i16::MAX);
        assert_eq!(quantize_f32(-1.0).unwrap(), i16::MIN);
        assert_eq!(quantize_f32(0.5).unwrap(), 16384);
        assert_eq!(quantize_f32(-0.5).unwrap(), -16384);
        // Out-of-range values are clamped, not wrapped.
        assert_eq!(quantize_f32(2.0).unwrap(), i16::MAX);
        assert_eq!(quantize_f32(-2.0).unwrap(), i16::MIN);
        // Non-finite values are rejected.
        assert!(matches!(
            quantize_f32(f32::NAN),
            Err(PackError::NonFiniteSample(_))
        ));
        assert!(matches!(
            quantize_f32(f32::INFINITY),
            Err(PackError::NonFiniteSample(_))
        ));
    }

    #[test]
    fn test_downmix_stereo_average() {
        let mixed = downmix_i16(vec![100, 200, -100, 100], 2);
        assert_eq!(mixed, vec![150, 0]);
    }

    #[test]
    fn test_downmix_stereo_no_overflow() {
        // i16::MIN + i16::MIN would overflow i16; the i32 path must not.
        let mixed = downmix_i16(vec![i16::MIN, i16::MIN], 2);
        assert_eq!(mixed, vec![i16::MIN]);
        let mixed = downmix_i16(vec![i16::MAX, i16::MAX], 2);
        assert_eq!(mixed, vec![i16::MAX]);
    }

    #[test]
    fn test_load_mono_16bit_roundtrip() {
        let path = temp_path("mono16.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        write_wav(&path, spec, &[(0, 0), (100, 0), (-100, 0), (12345, 0)]);

        let pcm = load_wav(&path).unwrap();
        assert_eq!(pcm.sample_rate, 48000);
        assert_eq!(pcm.frames, 4);
        assert_eq!(pcm.samples, vec![0, 100, -100, 12345]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_load_stereo_16bit_downmixes() {
        let path = temp_path("stereo16.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        write_wav(&path, spec, &[(100, 200), (-100, 0)]);

        let pcm = load_wav(&path).unwrap();
        assert_eq!(pcm.sample_rate, 44100);
        assert_eq!(pcm.samples, vec![150, -50]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_load_float_32() {
        let path = temp_path("float32.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for s in [0.0f32, 0.5, -0.5, 1.0, -1.0] {
            writer.write_sample(s).unwrap();
        }
        writer.finalize().unwrap();

        let pcm = load_wav(&path).unwrap();
        assert_eq!(pcm.samples, vec![0, 16384, -16384, i16::MAX, i16::MIN]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_reject_three_channels() {
        let path = temp_path("three-channel.wav");
        let spec = hound::WavSpec {
            channels: 3,
            sample_rate: 48000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for _ in 0..8 {
            writer.write_sample(0i16).unwrap();
            writer.write_sample(0i16).unwrap();
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();

        let err = load_wav(&path).unwrap_err();
        assert!(
            err.to_string().contains("unsupported channel layout"),
            "unexpected error: {err}"
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_reject_out_of_range_sample_rate() {
        let path = temp_path("bad-rate.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 4000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        writer.write_sample(0i16).unwrap();
        writer.finalize().unwrap();

        assert!(matches!(
            load_wav(&path),
            Err(PackError::InvalidSampleRate(4000, ..))
        ));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_reject_empty_wav() {
        let path = temp_path("empty.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::create(&path, spec).unwrap();
        writer.finalize().unwrap();

        assert!(matches!(load_wav(&path), Err(PackError::ZeroLengthClip(_))));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_reject_garbage_file() {
        let path = temp_path("garbage.wav");
        std::fs::write(&path, b"this is definitely not a wav file").unwrap();

        let err = load_wav(&path).unwrap_err();
        assert!(
            matches!(err, PackError::UnsupportedFormat(_) | PackError::Io(_)),
            "unexpected error: {err}"
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_reject_truncated_file() {
        let path = temp_path("truncated.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        write_wav(&path, spec, &[(1, 0); 100]);

        // Truncate the file in half after writing it.
        let data = std::fs::read(&path).unwrap();
        let half = data.len() / 2;
        std::fs::write(&path, &data[..half]).unwrap();

        let err = load_wav(&path);
        assert!(err.is_err(), "truncated WAV must be rejected");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_reject_non_finite_float_samples() {
        let path = temp_path("nan.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        writer.write_sample(0.5f32).unwrap();
        writer.write_sample(f32::NAN).unwrap();
        writer.finalize().unwrap();

        assert!(matches!(
            load_wav(&path),
            Err(PackError::NonFiniteSample(_))
        ));

        std::fs::remove_file(&path).ok();
    }

    // --- Phase 5: f32 decode path ----------------------------------------

    #[test]
    fn test_decode_mono_16bit_normalizes() {
        let path = temp_path("dec-mono16.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        write_wav(
            &path,
            spec,
            &[(0, 0), (16384, 0), (-16384, 0), (i16::MIN, 0)],
        );

        let src = decode_wav(&path).unwrap();
        assert_eq!(src.sample_rate, 48000);
        assert_eq!(src.channels, 1);
        assert_eq!(src.encoding, "16-bit PCM");
        assert_eq!(src.frames(), 4);
        assert_eq!(src.samples, vec![0.0, 0.5, -0.5, -1.0]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_decode_8bit_is_signed() {
        let path = temp_path("dec-mono8.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 8,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for v in [0i8, 64, -64, -128] {
            writer.write_sample(v).unwrap();
        }
        writer.finalize().unwrap();

        let src = decode_wav(&path).unwrap();
        assert_eq!(src.encoding, "8-bit PCM");
        assert_eq!(src.samples, vec![0.0, 0.5, -0.5, -1.0]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_decode_24bit_normalizes() {
        let path = temp_path("dec-mono24.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 24,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for v in [0i32, 4_194_304, -4_194_304] {
            writer.write_sample(v).unwrap();
        }
        writer.finalize().unwrap();

        let src = decode_wav(&path).unwrap();
        assert_eq!(src.encoding, "24-bit PCM");
        assert_eq!(src.samples, vec![0.0, 0.5, -0.5]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_decode_32bit_int_normalizes() {
        let path = temp_path("dec-mono32.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for v in [0i32, 1_073_741_824, i32::MIN] {
            writer.write_sample(v).unwrap();
        }
        writer.finalize().unwrap();

        let src = decode_wav(&path).unwrap();
        assert_eq!(src.encoding, "32-bit PCM");
        assert_eq!(src.samples, vec![0.0, 0.5, -1.0]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_decode_float32_keeps_values() {
        let path = temp_path("dec-float32.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for s in [0.25f32, -0.75, 0.123_456] {
            writer.write_sample(s).unwrap();
        }
        writer.finalize().unwrap();

        let src = decode_wav(&path).unwrap();
        assert_eq!(src.encoding, "32-bit IEEE float");
        assert_eq!(src.samples, vec![0.25, -0.75, 0.123_456]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_to_mono_averages_stereo() {
        let src = SourceAudio {
            samples: vec![100.0, 0.0, -1.0, 1.0, 0.4, -0.4],
            sample_rate: 48000,
            channels: 2,
            encoding: "16-bit PCM".to_string(),
        };
        let mono = src.to_mono();
        assert_eq!(mono.samples, vec![50.0, 0.0, 0.0]);
        assert_eq!(mono.channels, 2, "source channels preserved for reports");
        assert_eq!(mono.sample_rate, 48000);
    }

    #[test]
    fn test_to_mono_passes_mono_through() {
        let src = SourceAudio {
            samples: vec![0.1, -0.2],
            sample_rate: 44100,
            channels: 1,
            encoding: "16-bit PCM".to_string(),
        };
        let mono = src.to_mono();
        assert_eq!(mono.samples, vec![0.1, -0.2]);
        assert_eq!(mono.channels, 1);
    }

    #[test]
    fn test_decode_rejects_three_channels() {
        let path = temp_path("dec-three-channel.wav");
        let spec = hound::WavSpec {
            channels: 3,
            sample_rate: 48000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for _ in 0..4 {
            writer.write_sample(0i16).unwrap();
            writer.write_sample(0i16).unwrap();
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();

        let err = decode_wav(&path).unwrap_err();
        assert!(
            err.to_string().contains("unsupported channel layout"),
            "unexpected error: {err}"
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_decode_rejects_bad_sample_rate() {
        let path = temp_path("dec-bad-rate.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 4000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        writer.write_sample(100i16).unwrap();
        writer.finalize().unwrap();

        assert!(matches!(
            decode_wav(&path),
            Err(PackError::InvalidSampleRate(4000, ..))
        ));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_decode_rejects_empty_wav() {
        let path = temp_path("dec-empty.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::create(&path, spec).unwrap();
        writer.finalize().unwrap();

        assert!(matches!(
            decode_wav(&path),
            Err(PackError::ZeroLengthClip(_))
        ));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_decode_rejects_non_finite_float_samples() {
        let path = temp_path("dec-nan.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        writer.write_sample(0.5f32).unwrap();
        writer.write_sample(f32::INFINITY).unwrap();
        writer.finalize().unwrap();

        assert!(matches!(
            decode_wav(&path),
            Err(PackError::NonFiniteSample(_))
        ));

        std::fs::remove_file(&path).ok();
    }
}
