//! The two `/proc/self` facts the long-running checks report.
//!
//! Read here rather than in each command so the numbers mean the same thing
//! everywhere: resident set from `VmRSS`, thread count from `Threads`.

/// Resident set size in KiB, or `0` when `/proc` cannot be read.
pub(crate) fn rss_kib() -> u64 {
    status_field("VmRSS:")
        .and_then(|value| value.split_whitespace().next().and_then(|v| v.parse().ok()))
        .unwrap_or(0)
}

/// Number of threads this process currently has, or `0` when it cannot be
/// read.
pub(crate) fn thread_count() -> u64 {
    status_field("Threads:")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

/// The value after a `/proc/self/status` label, trimmed.
fn status_field(label: &str) -> Option<String> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with(label))?;
    Some(line[label.len()..].trim().to_string())
}
