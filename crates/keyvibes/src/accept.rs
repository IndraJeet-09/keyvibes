//! Shared reporting for the acceptance commands.
//!
//! Every acceptance command prints the same vocabulary so an operator never
//! has to guess what a line means:
//!
//! * **PASS** - the check ran here and now and held.
//! * **FAIL** - the check ran and did not hold; the command exits non-zero.
//! * **NOT RUN** - the check needs a resource this session does not have
//!   (no PipeWire, no readable keyboard, no physical cable).
//! * **MANUAL** - the operator has to do something while the command runs.

use anyhow::bail;
use std::fmt::Write as _;

/// Outcome of a single check.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// Ran here and held.
    Pass,
    /// Ran here and did not hold.
    Fail,
    /// Could not run on this host.
    NotRun,
    /// Needs a human while the command runs.
    Manual,
}

impl Status {
    /// Fixed-width label used in reports.
    pub fn label(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Fail => "FAIL",
            Status::NotRun => "NOT RUN",
            Status::Manual => "MANUAL",
        }
    }
}

/// One reported line.
pub struct Check {
    /// Outcome.
    pub status: Status,
    /// Short human-readable check name.
    pub name: String,
    /// Why it passed, what it failed on, or what is missing.
    pub detail: String,
}

/// Accumulates checks for one acceptance run.
pub struct Report {
    title: String,
    checks: Vec<Check>,
}

impl Report {
    /// Starts a report for the named command.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            checks: Vec::new(),
        }
    }

    /// Records a check.
    pub fn add(&mut self, status: Status, name: impl Into<String>, detail: impl Into<String>) {
        self.checks.push(Check {
            status,
            name: name.into(),
            detail: detail.into(),
        });
    }

    /// Convenience wrapper for a passing check.
    pub fn pass(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.add(Status::Pass, name, detail);
    }

    /// Convenience wrapper for a failing check.
    pub fn fail(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.add(Status::Fail, name, detail);
    }

    /// Convenience wrapper for a check that could not run here.
    pub fn not_run(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.add(Status::NotRun, name, detail);
    }

    /// Convenience wrapper for an operator-performed step.
    pub fn manual(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.add(Status::Manual, name, detail);
    }

    /// Number of failed checks.
    pub fn failures(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| check.status == Status::Fail)
            .count()
    }

    /// Number of checks that could not run on this host.
    pub fn not_run_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| check.status == Status::NotRun)
            .count()
    }

    /// Number of steps left to the operator.
    pub fn manual_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| check.status == Status::Manual)
            .count()
    }

    /// Prints the report and returns `Err` when any check failed.
    pub fn finish(self) -> anyhow::Result<()> {
        println!("KeyVibes {}", self.title);
        println!();

        let mut out = String::new();
        for check in &self.checks {
            let _ = writeln!(
                out,
                "  [{:<8}] {}{}",
                check.status.label(),
                check.name,
                if check.detail.is_empty() {
                    String::new()
                } else {
                    format!(" - {}", check.detail)
                }
            );
        }
        print!("{out}");

        let failures = self.failures();
        let not_run = self.not_run_count();
        let manual = self.manual_count();

        println!();
        if failures > 0 {
            bail!("{failures} check(s) failed");
        }
        let mut caveats = Vec::new();
        if not_run > 0 {
            caveats.push(format!("{not_run} check(s) not run on this host"));
        }
        if manual > 0 {
            caveats.push(format!("{manual} step(s) left to the operator"));
        }
        if caveats.is_empty() {
            println!("PASS  keyvibes {}", self.title);
        } else {
            println!("PASS  keyvibes {} ({})", self.title, caveats.join(", "));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_fixed_width_when_padded_and_distinct() {
        let labels = [
            Status::Pass.label(),
            Status::Fail.label(),
            Status::NotRun.label(),
            Status::Manual.label(),
        ];
        assert_eq!(labels[0], "PASS");
        assert_eq!(labels[1], "FAIL");
        assert_eq!(labels[2], "NOT RUN");
        assert_eq!(labels[3], "MANUAL");
        for label in labels {
            assert!(label.len() <= 8, "report column assumes 8-wide labels");
            assert_eq!(format!("{label:<8}").len(), 8);
        }
    }

    #[test]
    fn counts_failures_skips_and_manual_steps_separately() {
        let mut report = Report::new("unit");
        report.pass("a", "ok");
        report.fail("b", "bad");
        report.not_run("c", "no hardware");
        report.manual("d", "pull a cable");
        assert_eq!(report.failures(), 1);
        assert_eq!(report.not_run_count(), 1);
        assert_eq!(report.manual_count(), 1);
    }
}
