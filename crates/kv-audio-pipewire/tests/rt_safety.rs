//! Phase 8: real-time safety proof by source audit.
//!
//! `crates/kv-mixer/tests/rt_safety.rs` proves the hot path allocates nothing
//! by measurement. This test proves the complementary claim statically: the
//! source the callback runs contains no lock, no logging, no blocking call,
//! and no I/O - the things a runtime measurement on an idle machine would
//! never catch because the code simply never took that branch.

/// The callback itself, extracted verbatim from `stream.rs`.
const STREAM_SOURCE: &str = include_str!("../src/stream.rs");

/// Files that run on, or are reached from, the audio thread.
const RT_SOURCES: &[(&str, &str)] = &[
    ("realtime.rs", include_str!("../src/realtime.rs")),
    (
        "kv-mixer/mixer.rs",
        include_str!("../../kv-mixer/src/mixer.rs"),
    ),
    (
        "kv-mixer/variation.rs",
        include_str!("../../kv-mixer/src/variation.rs"),
    ),
    (
        "kv-mixer/voice.rs",
        include_str!("../../kv-mixer/src/voice.rs"),
    ),
    (
        "kv-mixer/interpolation.rs",
        include_str!("../../kv-mixer/src/interpolation.rs"),
    ),
    (
        "kv-mixer/limiter.rs",
        include_str!("../../kv-mixer/src/limiter.rs"),
    ),
];

/// Constructs, logging, blocking, and I/O APIs banned on the audio thread.
///
/// Deliberately excludes `assert!`, `panic!` and `expect`: those are static
/// invariant guards that build with a `&'static str` payload and never touch
/// the heap or a file descriptor. `keyvibes stress` separately proves the
/// callback stays inside its wall-clock budget, so an unreachable invariant
/// guard costs nothing.
const BANNED: &[&str] = &[
    "Mutex",
    "RwLock",
    "Condvar",
    ".lock()",
    "eprintln!",
    "println!",
    "dbg!",
    "to_string",
    "format!",
    "String",
    "Vec<",
    "Box<",
    "vec!",
    "std::thread",
    "thread::",
    "SystemTime",
    "Instant",
    "std::fs",
    "File::",
    "std::process::Command",
    "read_to_string",
    "std::alloc",
    "sleep(",
];

/// Strict list applied to `process_callback` itself, on top of [`BANNED`]:
/// the callback must not panic either.
const CALLBACK_ONLY: &[&str] = &["assert!", "panic!", "expect(", "unwrap("];

/// Line comments and doc comments, so documentation prose cannot trip the
/// audit (`render_block`'s own docs, for example, say "no locks").
fn strip_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| match line.find("//") {
            Some(index) => &line[..index],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extracts one top-level function body, including its signature.
///
/// Works because none of these functions contain a nested item whose closing
/// brace sits at column zero.
fn function_body<'a>(source: &'a str, signature: &str, file: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("{file}: could not find `{signature}`"));
    let body = &source[start..];
    let mut offset = 0;
    for line in body.lines() {
        offset += line.len() + 1;
        if line == "}" {
            return &body[..offset];
        }
    }
    panic!("{file}: `{signature}` was never closed");
}

fn banned_hits<'a>(clean_source: &str, banned: &'a [&'a str]) -> Vec<&'a str> {
    banned
        .iter()
        .copied()
        .filter(|token| clean_source.contains(token))
        .collect()
}

#[test]
fn process_callback_contains_no_rt_violations() {
    // `process_callback` is a thin bracket around `process_inner`: the wrapper
    // only sets the in-callback flag the control plane reads when it decides
    // whether a pack may be retired, and the work happens in the body. Both
    // run on the audio thread, so both are audited.
    let entry = function_body(STREAM_SOURCE, "fn process_callback", "stream.rs");
    let body = function_body(STREAM_SOURCE, "fn process_inner", "stream.rs");

    for (name, source) in [("process_callback", entry), ("process_inner", body)] {
        let clean = strip_comments(source);
        let mut hits = banned_hits(&clean, BANNED);
        hits.extend(banned_hits(&clean, CALLBACK_ONLY));
        hits.sort_unstable();
        assert!(
            hits.is_empty(),
            "{name}() calls into banned APIs: {hits:?}\n\n---\n{source}"
        );
    }

    // The wrapper must bracket the body exactly once on each side, so the
    // in-callback flag is raised for every path through the audio work.
    for marker in [
        "set_in_callback(true)",
        "process_inner(stream, state)",
        "set_in_callback(false)",
    ] {
        assert!(
            entry.contains(marker),
            "process_callback() no longer contains `{marker}`; the audit \
             needs updating before it can be trusted"
        );
    }

    // The audit is only meaningful if it really inspected the audio work.
    for marker in [
        "state.queue.pop()",
        "state.mixer.trigger",
        "state.mixer.render_block",
        "stats.callback_hist.record_ns",
    ] {
        assert!(
            body.contains(marker),
            "process_inner() no longer contains `{marker}`; the audit \
             needs updating before it can be trusted"
        );
    }
}

#[test]
fn real_time_sources_ban_locks_logging_and_io() {
    for (name, source) in RT_SOURCES {
        let hits = banned_hits(&strip_comments(source), BANNED);
        assert!(
            hits.is_empty(),
            "{name} is reachable from the audio thread but mentions {hits:?}"
        );
    }
}

#[test]
fn logging_is_confined_to_the_control_plane_helper() {
    // `stream.rs` is allowed exactly one logging call site, inside
    // `tracing_error_free_log`, which only the control-plane loop thread
    // reaches. Everything else in the file - and every caller the callback
    // makes - must be silent.
    let clean = strip_comments(STREAM_SOURCE);
    let helper_start = clean
        .find("fn tracing_error_free_log")
        .expect("helper missing");
    let helper = function_body(&clean, "fn tracing_error_free_log", "stream.rs");
    let helper_end = helper_start + helper.len();
    let mut sites = 0;
    for token in ["eprintln!", "println!", "dbg!"] {
        let mut search = 0;
        while let Some(offset) = clean[search..].find(token) {
            let index = search + offset;
            search = index + token.len();
            sites += 1;
            assert!(
                index >= helper_start && index < helper_end,
                "`{token}` found outside `tracing_error_free_log` at byte \
                 {index}:\n{}",
                &clean[index.saturating_sub(120)..(index + 120).min(clean.len())]
            );
        }
    }

    // `println!` also matches inside `eprintln!`, so two hits at one byte
    // range is still the single call site inside the helper.
    assert_eq!(
        sites, 2,
        "stream.rs should log exactly once, inside `tracing_error_free_log`"
    );
}
