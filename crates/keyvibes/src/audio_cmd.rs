//! `keyvibes analyze` and `keyvibes process`: offline per-clip tools.
//!
//! Both commands share the Phase 5 pipeline (`kv_pack::processing`) but have
//! different contracts:
//!
//! - **analyze** is read-only: it decodes sources (a WAV, or every WAV a
//!   `pack.toml` references), runs the pipeline in memory, and prints what
//!   would happen — trims, gain, warnings. Nothing is written.
//! - **process** renders one WAV through the pipeline and writes the result
//!   as a 16-bit mono PCM WAV so it can be auditioned or inspected.

use anyhow::{Context, Result};
use kv_pack::dither::DitherSeed;
use kv_pack::error::PackError;
use kv_pack::manifest::resolve_source;
use kv_pack::processing::{AudioProcessor, ProcessedAudio, ProcessingConfig, ProcessingReport};
use kv_pack::wav::{decode_wav, write_wav_i16, SourceAudio};
use kv_pack::PackManifest;
use std::path::{Path, PathBuf};

/// `keyvibes analyze <wav|pack.toml>...`
///
/// WAV paths are analyzed with the default `[processing]` config; manifest
/// paths use the config declared in their `[processing]` section, so the
/// output shows exactly what `pack build` would do.
pub fn analyze(sources: &[PathBuf]) -> Result<()> {
    let mut analyzed = 0usize;

    for source in sources {
        let is_manifest = source
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("toml"));

        if is_manifest {
            analyzed += analyze_manifest(source)?;
        } else {
            analyze_clip(source, &ProcessingConfig::default())
                .with_context(|| format!("failed to analyze {}", source.display()))?;
            println!();
            analyzed += 1;
        }
    }

    if analyzed == 0 {
        anyhow::bail!("no sources to analyze");
    }
    Ok(())
}

/// `keyvibes process <input.wav> -o <output.wav>`
///
/// Runs one file through DC correction, trim, fade, loudness normalization,
/// peak protection, and dithered quantization using the default config.
pub fn process(input: &Path, output: &Path) -> Result<()> {
    let source =
        decode_wav(input).with_context(|| format!("failed to decode {}", input.display()))?;

    let cfg = ProcessingConfig::default();
    let processed = AudioProcessor::new(cfg.clone())
        .process(source.to_mono())
        .with_context(|| format!("failed to process {}", input.display()))?;

    let seed = if cfg.dither {
        Some(DitherSeed::from_u64(cfg.dither_seed))
    } else {
        None
    };
    let pcm = kv_pack::processing::encode_i16(&processed.samples, processed.sample_rate, seed)
        .with_context(|| format!("failed to encode {}", input.display()))?;

    write_wav_i16(output, pcm.sample_rate, &pcm.samples)
        .with_context(|| format!("failed to write {}", output.display()))?;

    print_header(&format!(
        "{} ({}, {} Hz, {}ch -> mono)",
        input.display(),
        source.encoding,
        source.sample_rate,
        source.channels
    ));
    print_report(&processed.report);
    println!(
        "wrote {} ({} frames, 16-bit mono PCM)",
        output.display(),
        pcm.frames
    );
    Ok(())
}

/// Analyzes every source referenced by a manifest.
fn analyze_manifest(manifest_path: &Path) -> Result<usize> {
    let manifest = PackManifest::load(manifest_path)
        .with_context(|| format!("failed to load {}", manifest_path.display()))?;
    let cfg = manifest
        .processing_config()
        .with_context(|| format!("invalid [processing] in {}", manifest_path.display()))?;
    let root = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let keys = manifest.resolved_keys()?;

    if !cfg.enabled {
        println!(
            "{}: [processing] enabled = false (report shows the pipeline that is currently disabled)",
            manifest_path.display()
        );
    }

    let mut count = 0usize;
    for key in &keys {
        for (variant, relative) in key.samples.iter().enumerate() {
            let absolute = resolve_source(root, relative)
                .map_err(|e| wrap_context(&key.physical_key.to_string(), relative, e))?;
            let source = decode_wav(&absolute)
                .map_err(|e| wrap_context(&key.physical_key.to_string(), relative, e))?;

            print_header(&format!(
                "{}  [{} variant {}] ({}, {} Hz, {}ch -> mono)",
                relative,
                key.physical_key,
                variant,
                source.encoding,
                source.sample_rate,
                source.channels,
            ));
            print_process_report(&source, &cfg, relative, &key.physical_key.to_string())?;
            println!();
            count += 1;
        }
    }
    Ok(count)
}

/// Runs the pipeline on one already-decoded source and prints the report.
fn print_process_report(
    source: &SourceAudio,
    cfg: &ProcessingConfig,
    relative: &str,
    key: &str,
) -> Result<()> {
    let processed = AudioProcessor::new(cfg.clone())
        .process(source.to_mono())
        .map_err(|e| wrap_context(key, relative, e))?;
    print_report(&processed.report);
    Ok(())
}

/// Decodes and analyzes one standalone WAV file.
fn analyze_clip(path: &Path, cfg: &ProcessingConfig) -> Result<()> {
    let source = decode_wav(path)?;
    print_header(&format!(
        "{}  ({}, {} Hz, {}ch -> mono)",
        path.display(),
        source.encoding,
        source.sample_rate,
        source.channels
    ));
    let processed: ProcessedAudio = AudioProcessor::new(cfg.clone()).process(source.to_mono())?;
    print_report(&processed.report);
    Ok(())
}

/// Wraps any error the way the builder does, so all CLI failures share one
/// multi-line layout: Key / File / Reason.
fn wrap_context(key: &str, file: &str, e: PackError) -> anyhow::Error {
    anyhow::Error::new(PackError::BuildContext {
        key: key.to_string(),
        file: file.to_string(),
        reason: e.to_string(),
    })
}

fn print_header(text: &str) {
    println!("{text}");
}

/// Prints the full per-clip processing report (shared by both commands).
fn print_report(r: &ProcessingReport) {
    println!(
        "  frames:    {:>7} -> {:<7} (lead {}, tail {}, preroll {}, fade {})",
        r.original_frames,
        r.processed_frames,
        r.leading_frames_removed,
        r.trailing_frames_removed,
        r.preroll_frames,
        r.fade_frames,
    );
    println!(
        "  duration:  {:>7.1} ms -> {:.1} ms",
        r.original_duration_seconds * 1000.0,
        r.processed_duration_seconds * 1000.0
    );
    println!(
        "  peak:      {:>7.2} -> {:.2} dBFS",
        r.original_peak_dbfs, r.final_peak_dbfs
    );
    println!(
        "  rms:       {:>7.2} -> {:.2} dBFS",
        r.original_rms_dbfs, r.final_rms_dbfs
    );
    println!(
        "  dc:        {:+.4} corrected (original {:+.4}, final {:+.4})",
        r.dc_correction, r.original_dc_offset, r.final_dc_offset
    );
    println!(
        "  gain:      {:+.2} dB (onset frame {}, tail frame {})",
        r.gain_db, r.onset_frame, r.tail_frame
    );

    if r.warnings.is_empty() {
        println!("  warnings:  none");
    } else {
        println!("  warnings:  {}", r.warnings.len());
        for warning in &r.warnings {
            println!("    - {warning}");
        }
    }
}
