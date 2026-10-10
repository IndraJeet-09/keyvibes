//! Phase 16 regression tests: malicious and malformed sound packs.
//!
//! A `.kvpack` is untrusted input: it may be downloaded, copied from a repo,
//! or simply corrupted. Two properties must hold no matter what it contains:
//!
//! * a path in a pack manifest can never reach outside the pack root,
//! * a corrupt or hostile archive fails with a structured error instead of
//!   panicking, reading past the mapping, or handing the mixer an unaligned
//!   pointer.
//!
//! Every corruption below is applied to a known-good pack and the loader must
//! return `Err`. A panic fails the test, which is exactly the point.

use kv_pack::error::PackResult;
use kv_pack::format::{CLIP_ENTRY_SIZE, GUARD_AFTER, GUARD_BEFORE, KEY_ENTRY_SIZE};
use kv_pack::{Header, KvPack, PackError, HEADER_SIZE};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

/// Monotonic suffix so parallel tests never share a scratch directory.
static SCRATCH: AtomicU32 = AtomicU32::new(0);

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "keyvibes-pack-security-{}-{}-{}",
        std::process::id(),
        SCRATCH.fetch_add(1, Ordering::Relaxed),
        label
    ));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

fn golden() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/golden.kvpack"
    ))
    .expect("golden fixture")
}

/// Writes `bytes` to a scratch file and opens it. The mapping outlives the
/// unlink, which is the normal case on Unix and a good extra edge to hit.
fn open_bytes(label: &str, bytes: &[u8]) -> PackResult<KvPack> {
    let dir = scratch(label);
    let path = dir.join("pack.kvpack");
    std::fs::write(&path, bytes).expect("write scratch pack");
    let opened = KvPack::open(&path);
    let _ = std::fs::remove_dir_all(&dir);
    opened
}

/// The known-good fixture must load, or every assertion below is meaningless.
#[test]
fn golden_pack_still_loads() {
    let dir = scratch("golden");
    let path = dir.join("golden.kvpack");
    std::fs::copy(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/golden.kvpack"),
        &path,
    )
    .expect("copy fixture");
    let pack = KvPack::open(&path).expect("golden fixture must open");
    assert!(pack.stats().clips > 0);
    assert!(pack.stats().keys > 0);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// header corruptions
// ---------------------------------------------------------------------------

/// Every single-field header corruption, applied to a valid pack.
/// A single edit applied to a pack's bytes.
type Mutation = Box<dyn Fn(&mut Vec<u8>)>;

#[test]
fn corrupt_headers_fail_gracefully() {
    let base = golden();

    let mut cases: Vec<(&str, Mutation)> = Vec::new();

    cases.push(("bad magic", Box::new(|b| b[0] = 0x00)));
    cases.push(("unsupported version", Box::new(|b| b[8] = 99)));
    cases.push(("wrong header size", Box::new(|b| b[12] = 7)));
    cases.push((
        "file size claim mismatch",
        Box::new(|b| {
            let claim = u64::MAX;
            b[16..24].copy_from_slice(&claim.to_le_bytes());
        }),
    ));
    cases.push((
        "key count above the limit",
        Box::new(|b| {
            b[24..28].copy_from_slice(&10_000u32.to_le_bytes());
        }),
    ));
    cases.push((
        "clip count above the limit",
        Box::new(|b| {
            b[28..32].copy_from_slice(&1_000_000u32.to_le_bytes());
        }),
    ));
    cases.push((
        "metadata offset past EOF",
        Box::new(|b| {
            let off = u64::MAX - 8;
            b[32..40].copy_from_slice(&off.to_le_bytes());
        }),
    ));
    cases.push((
        "metadata region past EOF",
        Box::new(|b| {
            b[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        }),
    ));
    cases.push((
        "key table offset past EOF",
        Box::new(|b| {
            let off = u64::MAX - 4;
            b[44..52].copy_from_slice(&off.to_le_bytes());
        }),
    ));
    cases.push((
        "key table size mismatch",
        Box::new(|b| {
            b[52..56].copy_from_slice(&999u32.to_le_bytes());
        }),
    ));
    cases.push((
        "clip table offset past EOF",
        Box::new(|b| {
            let off = u64::MAX - 4;
            b[56..64].copy_from_slice(&off.to_le_bytes());
        }),
    ));
    cases.push((
        "clip table size mismatch",
        Box::new(|b| {
            b[64..68].copy_from_slice(&7u32.to_le_bytes());
        }),
    ));
    cases.push((
        "sample offset misaligned",
        Box::new(|b| {
            let off = u64::from_le_bytes(b[68..76].try_into().unwrap()) | 1;
            b[68..76].copy_from_slice(&off.to_le_bytes());
        }),
    ));
    cases.push((
        "sample offset past EOF",
        Box::new(|b| {
            let off = u64::MAX - 4;
            b[68..76].copy_from_slice(&off.to_le_bytes());
        }),
    ));
    cases.push((
        "sample region overflows offset",
        Box::new(|b| {
            b[76..84].copy_from_slice(&u64::MAX.to_le_bytes());
        }),
    ));
    cases.push((
        "sample rate out of range",
        Box::new(|b| {
            b[84..88].copy_from_slice(&1u32.to_le_bytes());
        }),
    ));
    cases.push((
        "unsupported channel count",
        Box::new(|b| {
            b[88..90].copy_from_slice(&2u16.to_le_bytes());
        }),
    ));
    cases.push((
        "unknown sample format",
        Box::new(|b| {
            b[90..92].copy_from_slice(&7u16.to_le_bytes());
        }),
    ));
    cases.push(("reserved bytes not zero", Box::new(|b| b[92] = 1)));

    for (label, mutate) in cases {
        let mut bytes = base.clone();
        mutate(&mut bytes);
        let outcome = open_bytes(label, &bytes);
        assert!(
            outcome.is_err(),
            "{label}: loader accepted a corrupt header"
        );
    }
}

/// Truncated files of every interesting size must be rejected up front.
#[test]
fn truncated_files_fail_gracefully() {
    let base = golden();
    for len in [0usize, 1, 8, 64, 119] {
        if len > base.len() {
            continue;
        }
        let outcome = open_bytes(&format!("truncate-{len}"), &base[..len]);
        assert!(outcome.is_err(), "a {len}-byte file was accepted");
    }

    // One byte short of a complete header: the classic off-by-one.
    let outcome = open_bytes("truncate-header", &base[..HEADER_SIZE as usize - 1]);
    assert!(matches!(outcome, Err(PackError::FileTruncated { .. })));
}

/// Tables whose contents contradict the header must be rejected.
#[test]
fn inconsistent_tables_fail_gracefully() {
    let base = golden();
    let header = Header::parse(&base, base.len() as u64).expect("golden header");

    // Key entry whose clip range runs past the end of the clip table.
    let mut bytes = base.clone();
    let key_off = header.key_table_offset as usize;
    // `first_clip` is the last 4 bytes of an 8-byte key entry.
    bytes[key_off + KEY_ENTRY_SIZE - 4..key_off + KEY_ENTRY_SIZE]
        .copy_from_slice(&(header.clip_count.saturating_add(3)).to_le_bytes());
    let outcome = open_bytes("first-clip-out-of-range", &bytes);
    assert!(
        outcome.is_err(),
        "a key pointing past the clip table was accepted"
    );

    // A legal variant count whose range runs past the end of the clip table.
    let mut bytes = base.clone();
    // Key entry layout: physical_key u16 @0, variant_count u16 @2,
    // first_clip u32 @4.
    let clip_count = header.clip_count;
    assert!(clip_count > 1, "fixture needs more than one clip");
    bytes[key_off + 2..key_off + 4].copy_from_slice(&16u16.to_le_bytes());
    bytes[key_off + 4..key_off + 8].copy_from_slice(&(clip_count - 1).to_le_bytes());
    let outcome = open_bytes("clip-range-overflow", &bytes);
    assert!(
        outcome.is_err(),
        "an overflowing key→clip range was accepted"
    );

    // Clip whose sample offset is odd: would produce an unaligned `i16`
    // pointer for the mixer.
    let mut bytes = base.clone();
    let clip_off = header.clip_table_offset as usize;
    let mut offset = u64::from_le_bytes(bytes[clip_off..clip_off + 8].try_into().unwrap());
    if offset == 0 {
        offset = 1;
    } else {
        offset |= 1;
    }
    bytes[clip_off..clip_off + 8].copy_from_slice(&offset.to_le_bytes());
    let outcome = open_bytes("odd-clip-offset", &bytes);
    assert!(
        outcome.is_err(),
        "a misaligned clip offset was accepted: {offset}"
    );

    // Clip whose stored length contradicts its guard/frame counts.
    let mut bytes = base.clone();
    bytes[clip_off + 20..clip_off + CLIP_ENTRY_SIZE].copy_from_slice(&u32::MAX.to_le_bytes());
    let outcome = open_bytes("stored-frames-mismatch", &bytes);
    assert!(
        outcome.is_err(),
        "a clip whose stored_frames lied was accepted"
    );

    // A self-consistent clip that still runs past the end of the sample
    // region: this one passes the parser and must be caught by validation.
    let mut bytes = base.clone();
    let frames = kv_pack::MAX_CLIP_FRAMES;
    bytes[clip_off + 8..clip_off + 12].copy_from_slice(&frames.to_le_bytes());
    bytes[clip_off + 20..clip_off + CLIP_ENTRY_SIZE]
        .copy_from_slice(&(frames + GUARD_BEFORE + GUARD_AFTER).to_le_bytes());
    let outcome = open_bytes("clip-past-samples", &bytes);
    assert!(
        outcome.is_err(),
        "a clip extending past the sample region was accepted"
    );
}

/// A file that is not a pack at all - and a path that is not even a file.
#[test]
fn non_pack_inputs_fail_gracefully() {
    let outcome = open_bytes("garbage", b"this is not a sound pack");
    assert!(outcome.is_err());

    let outcome = open_bytes("empty", b"");
    assert!(outcome.is_err());

    let dir = scratch("directory");
    let outcome = KvPack::open(&dir);
    assert!(
        outcome.is_err(),
        "opening a directory must not panic or succeed"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The format caps a pack at 2 GB; a larger file is refused before it is
/// ever mapped.
#[test]
fn oversized_files_are_refused_before_mapping() {
    let dir = scratch("oversized");
    let path = dir.join("huge.kvpack");
    {
        let file = std::fs::File::create(&path).expect("create sparse file");
        file.set_len(kv_pack::MAX_PACK_SIZE + 1)
            .expect("extend sparsely");
    }
    let outcome = KvPack::open(&path);
    assert!(
        outcome.is_err(),
        "a file larger than MAX_PACK_SIZE was accepted"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// path traversal
// ---------------------------------------------------------------------------

/// A manifest may only read sources that live inside its own directory, even
/// after symlinks are resolved.
#[cfg(feature = "builder")]
#[test]
fn manifest_sources_cannot_escape_the_pack_root() {
    use kv_pack::manifest::resolve_source;

    let root = scratch("traversal-root");
    let outside = scratch("traversal-outside");
    std::fs::write(root.join("inside.wav"), b"RIFF").expect("seed inside");
    std::fs::write(outside.join("secret.wav"), b"RIFF").expect("seed outside");

    // A symlink inside the root that points out of it.
    std::os::unix::fs::symlink(outside.join("secret.wav"), root.join("link.wav")).expect("symlink");
    // A symlinked directory that points out of it.
    std::os::unix::fs::symlink(&outside, root.join("escape")).expect("dir symlink");

    let rejections = [
        "",
        "..",
        "../secret.wav",
        "../../etc/passwd",
        "/etc/passwd",
        "/etc/shadow",
        "./../secret.wav",
        "sub/../../secret.wav",
        "link.wav",
        "escape/secret.wav",
        "..\\..\\windows\\system32",
    ];
    for candidate in rejections {
        let outcome = resolve_source(&root, candidate);
        assert!(
            outcome.is_err(),
            "manifest source `{candidate}` escaped the pack root: {outcome:?}"
        );
    }

    // A genuinely nested file inside the root still works.
    std::fs::create_dir_all(root.join("sounds")).expect("subdir");
    std::fs::write(root.join("sounds/a.wav"), b"RIFF").expect("seed nested");
    assert!(resolve_source(&root, "sounds/a.wav").is_ok());

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&outside);
}

/// Pack metadata is text only: nothing in it names a program to run.
#[cfg(feature = "builder")]
#[test]
fn manifest_schema_has_no_executable_fields() {
    let source = include_str!("../src/manifest.rs");
    for banned in [
        "std::process",
        "Command::new",
        "system(",
        "popen",
        "exec",
        "/bin/sh",
    ] {
        assert!(
            !source.contains(banned),
            "manifest.rs mentions `{banned}` - pack metadata must never be executable"
        );
    }
}
