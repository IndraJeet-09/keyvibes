//! PipeWire-reported XRUN counters.
//!
//! `struct pw_stream_events` in PipeWire 1.6.8 has no `xrun` member, so a
//! client stream is never notified about an xrun directly. PipeWire *does*
//! publish the count per node: `pw-top`'s `ERR` column is documented as
//! "Total of Xruns and Errors", where for a follower node it increments when
//! the node started processing but did not finish before the graph cycle
//! deadline.
//!
//! This module reads that counter from the control plane. It never runs on
//! the real-time thread: spawning `pw-top` allocates and blocks.

use std::process::Command;

/// Default PipeWire node name KeyVibes registers (see [`crate::stream`]).
pub const NODE_NAME: &str = "keyvibes";

/// One sample of PipeWire's per-node error counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeXruns {
    /// PipeWire node id. Changes when the stream is rebuilt.
    pub node_id: u64,
    /// Cumulative `ERR` for that node.
    pub errors: u64,
}

impl NodeXruns {
    /// Reads the counter for `node_name`.
    ///
    /// Returns `None` when `pw-top` is missing, when PipeWire is not running,
    /// or when the node is not (yet) visible.
    pub fn read(node_name: &str) -> Option<Self> {
        // Two frames: the first can still be mid-discovery with `---` fields.
        let output = Command::new("pw-top")
            .args(["-b", "-n", "2"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?;

        // Last matching row wins: batch mode prints one row per frame.
        text.lines()
            .find_map(|line| Self::parse_line(line, node_name))
    }

    /// Parses one `pw-top` batch row.
    ///
    /// Layout: `S ID QUANT RATE WAIT BUSY W/Q B/Q ERR [FORMAT...] NAME`.
    /// `ERR` is field 8 whether or not the FORMAT columns are present.
    fn parse_line(line: &str, node_name: &str) -> Option<Self> {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 9 {
            return None;
        }
        if !fields.contains(&node_name) {
            return None;
        }
        Some(Self {
            node_id: fields[1].parse().ok()?,
            errors: fields[8].parse().ok()?,
        })
    }
}

/// Baseline for measuring XRUNs over a window.
#[derive(Debug, Clone, Copy, Default)]
pub struct XrunBaseline {
    sample: Option<NodeXruns>,
}

impl XrunBaseline {
    /// Captures the current counter for `node_name`.
    pub fn capture(node_name: &str) -> Self {
        Self {
            sample: NodeXruns::read(node_name),
        }
    }

    /// Counter captured by [`capture`](Self::capture), if any.
    pub fn sample(&self) -> Option<NodeXruns> {
        self.sample
    }

    /// XRUNs accumulated since the baseline was captured.
    ///
    /// Returns `None` when the counter cannot be read. If the stream was
    /// rebuilt in between the node id changes and the counter restarted, so
    /// the new node's count is used as-is.
    pub fn delta(&self, node_name: &str) -> Option<u64> {
        let current = NodeXruns::read(node_name)?;
        match self.sample {
            None => Some(current.errors),
            Some(base) if base.node_id == current.node_id => {
                Some(current.errors.saturating_sub(base.errors))
            }
            Some(_) => Some(current.errors),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_running_row_with_format_columns() {
        let line =
            "R   90      0      0  12.1us  28.9us  0.00  0.00    7    F32LE 2 48000  + keyvibes";
        let sample = NodeXruns::parse_line(line, "keyvibes").expect("parses");
        assert_eq!(sample.node_id, 90);
        assert_eq!(sample.errors, 7);
    }

    #[test]
    fn parses_idle_row_without_format_columns() {
        let line =
            "C   90      0      0    ---     ---   ---   ---     0                  keyvibes";
        let sample = NodeXruns::parse_line(line, "keyvibes").expect("parses");
        assert_eq!(sample.node_id, 90);
        assert_eq!(sample.errors, 0);
    }

    #[test]
    fn ignores_other_nodes() {
        let line =
            "R   81   1024  48000 139.8us  77.2us  0.01  0.00    0    S16LE 2 48000 bluez_output.1";
        assert!(NodeXruns::parse_line(line, "keyvibes").is_none());
    }

    #[test]
    fn ignores_malformed_lines() {
        assert!(NodeXruns::parse_line("", "keyvibes").is_none());
        assert!(NodeXruns::parse_line("S ID ERR", "keyvibes").is_none());
    }
}
