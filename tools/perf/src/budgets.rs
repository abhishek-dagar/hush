//! Budget parsing and validation (spec 0007, AC-10).
//!
//! Reads `main/perf/budgets.toml`, validates every row before anything is
//! built, and exposes the validated budget set plus the machine it was
//! authored on.

use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;

/// A validated set of budgets and the machine they were written for.
pub struct Budgets {
    /// The machine this file was authored on.
    pub machine: Machine,
    /// One row per metric to measure, in file order.
    pub rows: Vec<BudgetRow>,
}

/// The machine the budgets were authored on (AC-14).
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code, reason = "fields read by machine detection in later tasks")]
pub struct Machine {
    /// CPU brand string, e.g. "Apple M4".
    pub cpu: String,
    /// RAM in gigabytes.
    pub ram_gb: u64,
    /// OS name, e.g. "macos".
    pub os: String,
}

/// One `[[budget]]` row.
#[derive(Debug, Clone, Deserialize)]
pub struct BudgetRow {
    /// What to measure.
    pub metric: Metric,
    /// Which app the row applies to.
    pub target: Target,
    /// Operating systems the row runs on; non empty.
    pub os: Vec<Os>,
    /// The ceiling, in the row's unit.
    pub limit: f64,
    #[allow(dead_code, reason = "used for display in later tasks")]
    /// `MB` (10^6 bytes) or `ms`.
    pub unit: Unit,
    /// Regression alarm as a percent over baseline; size and memory only.
    #[serde(default)]
    pub alarm_pct: Option<f64>,
    /// Why this number matters; non empty.
    pub reason: String,
}

/// What to measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    BundleSize,
    ColdStart,
    MemoryTotal,
    MemoryContent,
    ScrollFrameCpuP95,
    ScriptLoad,
}

impl Metric {
    /// Whether this metric's value is a time (as opposed to a size or memory
    /// footprint). Timing metrics never carry `alarm_pct`.
    pub fn is_timing(self) -> bool {
        matches!(
            self,
            Metric::ColdStart | Metric::ScrollFrameCpuP95 | Metric::ScriptLoad
        )
    }

    /// Whether this metric is a deterministic size or memory value that
    /// supports a regression alarm.
    #[allow(dead_code, reason = "used for alarm validation in later tasks")]
    pub fn supports_alarm(self) -> bool {
        !self.is_timing()
    }
}

/// Which app a row applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Browser,
    Shell,
}

/// An operating system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Os {
    Macos,
    Linux,
    Windows,
}

impl Os {
    /// The name this OS is written with in budgets and reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Os::Macos => "macos",
            Os::Linux => "linux",
            Os::Windows => "windows",
        }
    }
}

/// The unit a budget is measured in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Unit {
    Mb,
    Ms,
}

impl Unit {
    #[allow(dead_code, reason = "used for display in later tasks")]
    pub fn as_str(self) -> &'static str {
        match self {
            Unit::Mb => "MB",
            Unit::Ms => "ms",
        }
    }
}

/// A unique key for a row: one row per (metric, target, os entry).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RowKey {
    pub metric: Metric,
    pub target: Target,
    pub os: Os,
}

/// Errors validating a budgets file.
#[derive(Debug, thiserror::Error)]
pub enum BudgetsError {
    /// The file could not be read or parsed.
    #[error("could not read budgets file {0}: {1}")]
    Io(std::path::PathBuf, std::io::Error),
    #[error("could not parse budgets file {0}: {1}")]
    Parse(std::path::PathBuf, toml::de::Error),
    /// One row failed validation; the message names the row.
    #[error("{0}")]
    Row(String),
}

impl Budgets {
    /// Loads and validates `path`.
    pub fn load(path: &Path) -> Result<Self, BudgetsError> {
        let text = std::fs::read_to_string(path)
            .map_err(|err| BudgetsError::Io(path.to_path_buf(), err))?;
        let raw: RawBudgets =
            toml::from_str(&text).map_err(|err| BudgetsError::Parse(path.to_path_buf(), err))?;
        raw.validate(path)
    }

    /// The baseline key this row matches, e.g. `bundle_size.browser.macos`.
    pub fn baseline_key(&self, row: &BudgetRow, os: Os) -> String {
        format!(
            "{}.{}.{}",
            row.metric.as_str(),
            row.target.as_str(),
            os.as_str()
        )
    }
}

impl Metric {
    pub fn as_str(self) -> &'static str {
        match self {
            Metric::BundleSize => "bundle_size",
            Metric::ColdStart => "cold_start",
            Metric::MemoryTotal => "memory_total",
            Metric::MemoryContent => "memory_content",
            Metric::ScrollFrameCpuP95 => "scroll_frame_cpu_p95",
            Metric::ScriptLoad => "script_load",
        }
    }
}

impl Target {
    pub fn as_str(self) -> &'static str {
        match self {
            Target::Browser => "browser",
            Target::Shell => "shell",
        }
    }
}

/// The raw, unvalidated TOML shape: deserializes straight into validated
/// row types, then business rules are checked.
#[derive(Debug, Deserialize)]
struct RawBudgets {
    machine: Machine,
    #[serde(default)]
    budget: Vec<BudgetRow>,
}

impl RawBudgets {
    fn validate(self, _path: &Path) -> Result<Budgets, BudgetsError> {
        let mut seen: HashSet<RowKey> = HashSet::new();
        let mut rows = Vec::new();

        for (index, row) in self.budget.into_iter().enumerate() {
            let label = format!("row {index} ({metric})", metric = row.metric.as_str());
            validate_row(&row, &label, &mut seen)?;
            rows.push(row);
        }

        Ok(Budgets {
            machine: self.machine,
            rows,
        })
    }
}

fn validate_row(
    row: &BudgetRow,
    label: &str,
    seen: &mut HashSet<RowKey>,
) -> Result<(), BudgetsError> {
    // Every row needs a reason (AC-10).
    if row.reason.trim().is_empty() {
        return Err(BudgetsError::Row(format!(
            "{label}: `reason` must not be empty"
        )));
    }

    // Limit above zero.
    if !row.limit.is_finite() || row.limit <= 0.0 {
        return Err(BudgetsError::Row(format!(
            "{label}: `limit` must be greater than 0, got {}",
            row.limit
        )));
    }

    // alarm_pct only on size and memory, never on timing (AC-10).
    if let Some(pct) = row.alarm_pct {
        if row.metric.is_timing() {
            return Err(BudgetsError::Row(format!(
                "{label}: `alarm_pct` is not allowed on the `{}` timing metric",
                row.metric.as_str()
            )));
        }
        if !pct.is_finite() || pct <= 0.0 {
            return Err(BudgetsError::Row(format!(
                "{label}: `alarm_pct` must be greater than 0, got {pct}"
            )));
        }
    }

    // Only bundle_size may target the shell (AC-2).
    if row.target == Target::Shell && row.metric != Metric::BundleSize {
        return Err(BudgetsError::Row(format!(
            "{label}: `target = \"shell\"` is only allowed for `bundle_size`, not `{}`",
            row.metric.as_str()
        )));
    }

    // Non empty OS list.
    if row.os.is_empty() {
        return Err(BudgetsError::Row(format!(
            "{label}: `os` must list at least one operating system"
        )));
    }

    // One row per (metric, target, os entry).
    for &os in &row.os {
        let key = RowKey {
            metric: row.metric,
            target: row.target,
            os,
        };
        if !seen.insert(key) {
            return Err(BudgetsError::Row(format!(
                "{label}: duplicate budget for {} on {}",
                row.metric.as_str(),
                os.as_str()
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(metric: Metric, target: Target, os: Vec<Os>) -> BudgetRow {
        BudgetRow {
            metric,
            target,
            os,
            limit: 10.0,
            unit: Unit::Mb,
            alarm_pct: None,
            reason: "a reason".to_owned(),
        }
    }

    #[test]
    fn a_valid_row_passes() {
        let mut seen = HashSet::new();
        let r = row(Metric::BundleSize, Target::Browser, vec![Os::Macos]);
        assert!(validate_row(&r, "row 0", &mut seen).is_ok());
    }

    #[test]
    fn an_empty_reason_is_rejected() {
        let mut seen = HashSet::new();
        let mut r = row(Metric::BundleSize, Target::Browser, vec![Os::Macos]);
        r.reason = "".to_owned();
        let err = validate_row(&r, "row 0", &mut seen).unwrap_err();
        assert!(err.to_string().contains("reason"));
    }

    #[test]
    fn alarm_pct_on_a_timing_metric_is_rejected() {
        let mut seen = HashSet::new();
        let mut r = row(Metric::ColdStart, Target::Browser, vec![Os::Macos]);
        r.alarm_pct = Some(3.0);
        let err = validate_row(&r, "row 0", &mut seen).unwrap_err();
        assert!(err.to_string().contains("alarm_pct"));
        assert!(err.to_string().contains("cold_start"));
    }

    #[test]
    fn shell_only_for_bundle_size() {
        let mut seen = HashSet::new();
        let r = row(Metric::ColdStart, Target::Shell, vec![Os::Macos]);
        let err = validate_row(&r, "row 0", &mut seen).unwrap_err();
        assert!(err.to_string().contains("shell"));
    }

    #[test]
    fn duplicate_rows_are_rejected() {
        let mut seen = HashSet::new();
        let r = row(Metric::BundleSize, Target::Browser, vec![Os::Macos]);
        assert!(validate_row(&r, "row 0", &mut seen).is_ok());
        let err = validate_row(&r, "row 1", &mut seen).unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn zero_limit_is_rejected() {
        let mut seen = HashSet::new();
        let mut r = row(Metric::BundleSize, Target::Browser, vec![Os::Macos]);
        r.limit = 0.0;
        let err = validate_row(&r, "row 0", &mut seen).unwrap_err();
        assert!(err.to_string().contains("limit"));
    }
}
