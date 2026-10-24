//! `perf`: the performance budget gate (spec 0007).
//!
//! One command, `cargo perf`, builds the release binaries, measures the
//! committed metrics, checks them against `perf/budgets.toml` and
//! `perf/baseline.json`, prints a table, writes
//! `target/perf/report.json`, and exits 0 only when nothing failed.
//! Runs from `.githooks/pre-push` before each push (AC-12).

mod args;
mod budgets;
mod build;
mod machine;
mod report;
mod size;
mod status;

use std::collections::BTreeMap;
use std::path::Path;

use budgets::Budgets;
use machine::Machine;
use status::Status as CoreStatus;

/// The budgets file, committed.
const BUDGETS_PATH: &str = "perf/budgets.toml";
/// The baseline file, written only by `--accept`.
const BASELINE_PATH: &str = "perf/baseline.json";

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(2);
        }
    }
}

/// Runs the gate. Returns the exit code, or an error when the gate itself
/// could not run (exit 2): invalid budgets, a failed native build, etc.
fn run() -> anyhow::Result<i32> {
    let args = args::Args::parse()?;

    // The runner surface keeps these apart: `--only` never writes the
    // baseline, so accepting from a partial run is a mistake (spec 0007,
    // Runner surface).
    if args.accept && args.only.is_some() {
        anyhow::bail!(
            "--accept cannot be combined with --only: the baseline is only \
             written by a full gate run"
        );
    }

    // Load and validate budgets before anything is built (AC-10). A row
    // with no reason, an unknown metric/os, or alarm_pct on a timing
    // metric stops here, naming the row.
    let budgets = Budgets::load(Path::new(BUDGETS_PATH))?;

    // The machine this gate runs on, for the mismatch rule (AC-14).
    let machine = Machine::detect();
    let machine_matches = machine::machine_matches(&budgets.machine, &machine);

    // Last accepted values, if any.
    let baseline = load_baseline()?;

    // Build the native release binaries once. Size is measured on exactly
    // the release profile that ships (spec 0007, Key invariants). A failed
    // build is exit 2 (AC-1).
    build::build_native()?;

    // Measure every budget row that applies to this OS.
    let mut rows = Vec::new();
    let mut any_fail = false;
    let mut any_hard_fail = false;

    for budget in &budgets.rows {
        if let Some(ref only) = args.only
            && budget.metric.as_str() != only.as_str()
        {
            continue;
        }

        for &os in &budget.os {
            let key = budgets.baseline_key(budget, os);
            let baseline_value = baseline.values.get(&key).copied();

            let (samples, value, note) = measure_metric(budget, os)?;
            let status = core_status(value, budget, baseline_value, machine_matches);
            if status.is_fail() {
                any_fail = true;
            }
            if status.blocks_accept() {
                any_hard_fail = true;
            }
            rows.push(report::build_row(
                budget.metric,
                budget.target,
                os,
                samples,
                value.unwrap_or(0.0),
                budget.limit,
                baseline_value,
                status,
                note,
            ));
        }
    }

    // Print the table (AC-1) and write the JSON report.
    report::print_table(&rows);
    let machine_report = report::MachineReport {
        cpu: machine.cpu.clone(),
        ram_gb: machine.ram_gb,
        os_version: machine.os_version.clone(),
    };
    let report = report::Report {
        machine: machine_report,
        machine_matches,
        rows: rows.clone(),
        v8_signal: report::V8Signal {
            script_share: 0.0,
            triggered: false,
        },
    };
    report::Report::write(&report, &report::report_path())?;

    // --accept: refresh baseline.json when no row hard failed (AC-10). An
    // alarm is allowed here: accepting new numbers is the point.
    if args.accept {
        if any_hard_fail {
            anyhow::bail!("cannot accept a baseline while a budget row fails");
        }
        write_baseline(&budgets, &rows, &machine)?;
    }

    if any_fail { Ok(1) } else { Ok(0) }
}

/// The OS this machine is running, as a budget `Os`.
fn current_os() -> budgets::Os {
    #[cfg(target_os = "macos")]
    {
        budgets::Os::Macos
    }
    #[cfg(target_os = "linux")]
    {
        budgets::Os::Linux
    }
    #[cfg(target_os = "windows")]
    {
        budgets::Os::Windows
    }
}

/// Measures one budget row on the current OS. The measured value is `None`
/// when the row could not be measured: a missing cross tool (AC-3), or a
/// runtime metric whose task has not landed yet (spec 0007, Build plan
/// tasks 3 to 6). An unmeasured row is honestly `not_measured`, never a
/// fake pass.
fn measure_metric(
    budget: &budgets::BudgetRow,
    os: budgets::Os,
) -> anyhow::Result<(Vec<f64>, Option<f64>, Option<String>)> {
    match budget.metric {
        budgets::Metric::BundleSize => match size::measure(budget.target, os) {
            Some(value) => Ok((vec![value], Some(value), None)),
            None if os == current_os() => Err(anyhow::anyhow!("could not measure bundle size")),
            // AC-3: the cross tool is missing or the cross build failed.
            None => Ok((
                vec![],
                None,
                Some(format!("not measured: {}", cross_hint(os))),
            )),
        },
        _ => Ok((
            vec![],
            None,
            Some("not measured: runtime metrics land in build plan tasks 3 to 6".to_owned()),
        )),
    }
}

/// The one line install hint for a cross OS whose build did not happen
/// (spec 0007, AC-3).
fn cross_hint(os: budgets::Os) -> String {
    match os {
        budgets::Os::Linux => {
            "install cargo-zigbuild (with Zig on PATH) to measure Linux size".to_owned()
        }
        budgets::Os::Windows => "install cargo-xwin to measure Windows size".to_owned(),
        budgets::Os::Macos => "no native macOS build on this machine".to_owned(),
    }
}

/// The status for a row, applying the machine-mismatch rule (AC-14).
fn core_status(
    value: Option<f64>,
    budget: &budgets::BudgetRow,
    baseline: Option<f64>,
    machine_matches: bool,
) -> CoreStatus {
    status::status(value, budget, baseline, machine_matches)
}

/// Loads the baseline, if present. A missing or unreadable baseline is fine:
/// the alarm is simply skipped for every row (first run).
fn load_baseline() -> anyhow::Result<Baseline> {
    let text = match std::fs::read_to_string(BASELINE_PATH) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Baseline::default());
        }
        Err(err) => return Err(err.into()),
    };
    serde_json::from_str(&text).map_err(Into::into)
}

/// The baseline file shape (spec 0007).
#[derive(Debug, Default, serde::Deserialize)]
#[allow(
    dead_code,
    reason = "fields read by later tasks when comparing to the detected machine"
)]
struct Baseline {
    recorded: String,
    machine: Option<BaselineMachine>,
    #[serde(default)]
    values: BTreeMap<String, f64>,
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code, reason = "fields read by later tasks")]
struct BaselineMachine {
    cpu: String,
    ram_gb: u64,
    os_version: String,
}

/// Writes `baseline.json` from this run (only when `--accept` and no row
/// failed). The machine record is refreshed from this machine (AC-10).
fn write_baseline(
    _budgets: &Budgets,
    rows: &[report::ReportRow],
    machine: &Machine,
) -> anyhow::Result<()> {
    let mut values = serde_json::Map::new();
    for row in rows {
        // A row that was not measured gets no baseline key, so its alarm
        // stays skipped until a run measures it (spec 0007, baseline shape).
        if row.samples.is_empty() {
            continue;
        }
        let key = format!("{}.{}.{}", row.metric, row.target, row.os);
        if let Some(num) = serde_json::Number::from_f64(row.value) {
            values.insert(key, serde_json::Value::Number(num));
        }
    }
    let baseline = serde_json::json!({
        "recorded": record_timestamp(),
        "machine": {
            "cpu": machine.cpu,
            "ram_gb": machine.ram_gb,
            "os_version": machine.os_version,
        },
        "values": serde_json::Value::Object(values),
    });
    report::write_json(Path::new(BASELINE_PATH), &baseline)
}

/// Today's UTC date, `YYYY-MM-DD`, without adding a date dependency.
fn record_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (year, month, day) = civil_from_days((secs / 86400) as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Days since 1970-01-01 to a proleptic Gregorian (year, month, day).
/// Howard Hinnant's `civil_from_days` algorithm (public domain).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_from_epoch_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(58), (1970, 2, 28));
        assert_eq!(civil_from_days(59), (1970, 3, 1)); // 1970 is not a leap year
        assert_eq!(civil_from_days(365), (1971, 1, 1));
        assert_eq!(civil_from_days(20750), (2026, 10, 24));
    }

    #[test]
    fn runtime_metrics_are_not_measured_not_fake_pass() {
        let budget = crate::budgets::BudgetRow {
            metric: crate::budgets::Metric::ColdStart,
            target: crate::budgets::Target::Browser,
            os: vec![crate::budgets::Os::Macos],
            limit: 200.0,
            unit: crate::budgets::Unit::Ms,
            alarm_pct: None,
            reason: "a reason".to_owned(),
        };
        let (samples, value, note) = measure_metric(&budget, crate::budgets::Os::Macos).unwrap();
        assert!(samples.is_empty());
        assert!(value.is_none());
        assert!(note.is_some());
        // The status function turns a missing value into not_measured.
        assert_eq!(
            status::status(value, &budget, None, true),
            status::Status::NotMeasured
        );
    }
}
