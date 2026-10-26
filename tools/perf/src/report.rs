//! Reporting: the terminal table and `target/perf/report.json` (spec 0007,
//! AC-1).

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::budgets::{Metric, Os, Target};
use crate::status::Status;

/// One budget row in the report.
#[derive(Debug, Clone, Serialize)]
pub struct ReportRow {
    pub metric: String,
    pub target: String,
    pub os: String,
    /// The measured samples for this run.
    pub samples: Vec<f64>,
    /// The aggregated value compared to the budget.
    pub value: f64,
    /// The committed limit.
    pub budget: f64,
    /// The committed goal, if any (AC-16).
    pub goal: Option<f64>,
    /// The last accepted baseline, if any.
    pub baseline: Option<f64>,
    pub status: String,
    pub note: Option<String>,
}

/// The whole `target/perf/report.json`.
#[derive(Debug, Serialize)]
pub struct Report {
    pub machine: MachineReport,
    pub machine_matches: bool,
    pub rows: Vec<ReportRow>,
    pub v8_signal: V8Signal,
}

#[derive(Debug, Serialize)]
pub struct MachineReport {
    pub cpu: String,
    pub ram_gb: u64,
    pub os_version: String,
}

/// The script-share signal: reported, never failing (AC-7).
#[derive(Debug, Clone, Serialize)]
pub struct V8Signal {
    pub script_share: f64,
    pub triggered: bool,
}

impl Report {
    /// Writes `report.json` to a temp file then renames it, so a partial
    /// file is never read (spec 0007, Run report).
    pub fn write(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, text)?;
        std::fs::rename(&temp, path)?;
        Ok(())
    }
}

/// Prints the one-table-row-per-metric report (AC-1).
pub fn print_table(rows: &[ReportRow]) {
    println!(
        "{:<32} {:<10} {:<8} {:>12} {:>10} {:>10} {:>10} {:<10} {:<10}",
        "metric", "target", "os", "value", "budget", "goal", "baseline", "status", "note"
    );
    for row in rows {
        let baseline = row
            .baseline
            .map(|b| format!("{b:.1}"))
            .unwrap_or_else(|| "-".to_owned());
        let goal = row
            .goal
            .map(|g| format!("{g:.1}"))
            .unwrap_or_else(|| "-".to_owned());
        let value = if row.samples.len() > 1 {
            format!("{:.1} ({} samples)", row.value, row.samples.len())
        } else {
            format!("{:.1}", row.value)
        };
        println!(
            "{:<32} {:<10} {:<8} {:>12} {:>10} {:>10} {:>10} {:<10} {:<10}",
            format!("{} ({})", row.metric, row.unit_label()),
            row.target,
            row.os,
            value,
            format!("{:.1}", row.budget),
            goal,
            baseline,
            row.status,
            row.note.as_deref().unwrap_or("-")
        );
    }
}

impl ReportRow {
    /// The unit label for display.
    pub fn unit_label(&self) -> &'static str {
        match self.metric.as_str() {
            "bundle_size" | "memory_total" | "memory_content" | "memory_after_browse" => "MB",
            _ => "ms",
        }
    }
}

/// Builds a `ReportRow` from a measured value.
#[allow(
    clippy::too_many_arguments,
    reason = "each field is a separate report column"
)]
pub fn build_row(
    metric: Metric,
    target: Target,
    os: Os,
    samples: Vec<f64>,
    value: f64,
    limit: f64,
    goal: Option<f64>,
    baseline: Option<f64>,
    status: Status,
    note: Option<String>,
) -> ReportRow {
    ReportRow {
        metric: metric.as_str().to_owned(),
        target: target.as_str().to_owned(),
        os: os.as_str().to_owned(),
        samples,
        value,
        budget: limit,
        goal,
        baseline,
        status: status.as_str().to_owned(),
        note,
    }
}

/// The target directory for the perf report: `target/perf`.
pub fn perf_dir() -> PathBuf {
    PathBuf::from("target/perf")
}

/// The report path: `target/perf/report.json`.
pub fn report_path() -> PathBuf {
    perf_dir().join("report.json")
}

/// Writes any JSON value to `path` via temp file then rename, so a partial
/// file is never read (spec 0007, Run report).
pub fn write_json(path: &Path, value: &serde_json::Value) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(value)?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, path)?;
    Ok(())
}

impl Default for ReportRow {
    fn default() -> Self {
        Self {
            metric: String::new(),
            target: String::new(),
            os: String::new(),
            samples: vec![],
            value: 0.0,
            budget: 0.0,
            goal: None,
            baseline: None,
            status: String::new(),
            note: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_row_unit_labels() {
        let row = ReportRow {
            metric: "bundle_size".to_owned(),
            target: "browser".to_owned(),
            os: "macos".to_owned(),
            samples: vec![],
            value: 0.0,
            budget: 0.0,
            goal: None,
            baseline: None,
            status: "pass".to_owned(),
            note: None,
        };
        assert_eq!(row.unit_label(), "MB");
        let row = ReportRow {
            metric: "cold_start".to_owned(),
            ..Default::default()
        };
        assert_eq!(row.unit_label(), "ms");
        let row = ReportRow {
            metric: "memory_after_browse".to_owned(),
            ..Default::default()
        };
        assert_eq!(row.unit_label(), "MB");
    }

    #[test]
    fn report_is_serializable() {
        let report = Report {
            machine: MachineReport {
                cpu: "Apple M4".to_owned(),
                ram_gb: 16,
                os_version: "macOS 26.0".to_owned(),
            },
            machine_matches: true,
            rows: vec![],
            v8_signal: V8Signal {
                script_share: 0.0,
                triggered: false,
            },
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("machine_matches"));
    }
}
