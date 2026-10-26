//! The pure status function (spec 0007, AC-8, AC-9, AC-14).
//!
//! Given a measured value, a budget row, an optional baseline value and
//! whether this machine matches `[machine]` in budgets.toml, returns the
//! row's status. The same function is used for every metric, so the rules
//! are tested once.

use crate::budgets::Metric;

/// What one budget row came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
    /// Under budget and under the regression alarm.
    Pass,
    /// Over budget: fails the gate.
    Fail,
    /// Under budget but over the regression alarm: fails the gate.
    Alarm,
    /// Under budget but over the goal: reported, never failing (AC-16).
    OverGoal,
    /// Reported but never failing: machine mismatch (timing/memory only).
    Info,
    /// Not measured (cross tool missing, footprint reader unavailable).
    NotMeasured,
}

impl Status {
    /// Whether this status counts as a failure of the gate.
    pub fn is_fail(self) -> bool {
        matches!(self, Status::Fail | Status::Alarm)
    }

    /// Whether this status blocks `--accept` (AC-10): only a hard budget
    /// break does. An alarm is allowed when accepting, since accepting new
    /// numbers is the point.
    pub fn blocks_accept(self) -> bool {
        matches!(self, Status::Fail)
    }

    /// The name reports and tables use.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Fail => "fail",
            Status::Alarm => "alarm",
            Status::OverGoal => "over_goal",
            Status::Info => "info",
            Status::NotMeasured => "not_measured",
        }
    }
}

/// Computes the status of one row (pure; no clocks, no file system).
///
/// # Inputs
/// - `value`: the aggregated measured value, or `None` when it could not be
///   measured at all.
/// - `budget`: the committed budget row.
/// - `baseline`: the last accepted value for this row, if any.
/// - `machine_matches`: whether the current machine matches `[machine]`.
///
/// # Rules
/// - Not measured → `not_measured`.
/// - Machine mismatch: timing and memory → `info`; size still gates.
/// - Over the limit → `fail`.
/// - Over baseline × (1 + alarm_pct) → `alarm` (size and memory only).
/// - Otherwise → `pass`.
pub fn status(
    value: Option<f64>,
    budget: &crate::budgets::BudgetRow,
    baseline: Option<f64>,
    machine_matches: bool,
) -> Status {
    let Some(value) = value else {
        return Status::NotMeasured;
    };

    // Machine mismatch (AC-14): timing and memory rows are info, never
    // failing; size rows still gate below.
    if !machine_matches && !is_size_metric(budget.metric) {
        return Status::Info;
    }

    // Over the hard ceiling → fail.
    if value > budget.limit {
        return Status::Fail;
    }

    // Regression alarm (AC-8): over baseline × (1 + alarm_pct) fails even
    // when under budget. Size and memory only; timing metrics print a
    // warning instead and never alarm.
    if !budget.metric.is_timing()
        && let (Some(baseline), Some(alarm_pct)) = (baseline, budget.alarm_pct)
        && baseline > 0.0
        && value > baseline * (1.0 + alarm_pct / 100.0)
    {
        return Status::Alarm;
    }

    // Softer goal: over goal but within limit warns but doesn't fail
    // (AC-16).
    if let Some(goal) = budget.goal
        && value > goal
    {
        return Status::OverGoal;
    }

    Status::Pass
}

/// Whether a metric is a deterministic size/memory value (as opposed to
/// timing). Size metrics still gate under machine mismatch; memory and
/// timing become info.
fn is_size_metric(metric: Metric) -> bool {
    matches!(metric, Metric::BundleSize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(metric: Metric) -> crate::budgets::BudgetRow {
        crate::budgets::BudgetRow {
            metric,
            target: crate::budgets::Target::Browser,
            os: vec![crate::budgets::Os::Macos],
            limit: 100.0,
            goal: None,
            unit: crate::budgets::Unit::Mb,
            alarm_pct: None,
            reason: "a reason".to_owned(),
        }
    }

    #[test]
    fn under_budget_is_pass() {
        let r = row(Metric::BundleSize);
        assert_eq!(status(Some(50.0), &r, None, true), Status::Pass);
    }

    #[test]
    fn over_budget_is_fail() {
        let r = row(Metric::BundleSize);
        assert_eq!(status(Some(150.0), &r, None, true), Status::Fail);
    }

    #[test]
    fn not_measured_is_not_measured() {
        let r = row(Metric::BundleSize);
        assert_eq!(status(None, &r, None, true), Status::NotMeasured);
    }

    #[test]
    fn machine_mismatch_turns_timing_into_info() {
        let r = row(Metric::ColdStart);
        // timing: info even when way over budget
        assert_eq!(status(Some(999.0), &r, None, false), Status::Info);
        // size still gates under mismatch
        let r = row(Metric::BundleSize);
        assert_eq!(status(Some(999.0), &r, None, false), Status::Fail);
        assert_eq!(status(Some(50.0), &r, None, false), Status::Pass);
    }

    #[test]
    fn alarm_over_baseline_fails_even_under_budget() {
        let mut r = row(Metric::MemoryTotal);
        r.limit = 1000.0;
        r.alarm_pct = Some(10.0);
        // baseline 100, alarm threshold 110: value 105 is under both
        assert_eq!(status(Some(105.0), &r, Some(100.0), true), Status::Pass);
        // value 111 is over the alarm threshold but under budget: alarm
        assert_eq!(status(Some(111.0), &r, Some(100.0), true), Status::Alarm);
        // value 100 is exactly baseline: no alarm
        assert_eq!(status(Some(100.0), &r, Some(100.0), true), Status::Pass);
        // value 110 is exactly at the alarm threshold (not above): pass
        assert_eq!(status(Some(110.0), &r, Some(100.0), true), Status::Pass);
    }

    #[test]
    fn timing_metric_never_alarms() {
        let mut r = row(Metric::ColdStart);
        r.unit = crate::budgets::Unit::Ms;
        r.alarm_pct = Some(10.0); // would be a validation error, but here we test status only
        // way over baseline but under budget: pass (no alarm for timing)
        assert_eq!(status(Some(50.0), &r, Some(10.0), true), Status::Pass);
    }

    #[test]
    fn no_baseline_means_no_alarm() {
        let r = row(Metric::BundleSize);
        assert_eq!(status(Some(99.0), &r, None, true), Status::Pass);
    }

    #[test]
    fn accept_is_blocked_by_fail_not_by_alarm() {
        let mut r = row(Metric::MemoryTotal);
        r.limit = 1000.0;
        r.alarm_pct = Some(10.0);
        // A hard break blocks accepting (AC-10).
        assert!(status(Some(1500.0), &r, None, true).blocks_accept());
        // An alarm does not: accepting new numbers is the point.
        assert!(!status(Some(111.0), &r, Some(100.0), true).blocks_accept());
        assert!(status(Some(111.0), &r, Some(100.0), true).is_fail());
    }
}
