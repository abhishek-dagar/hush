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

    for budget in &budgets.rows {
        if let Some(ref only) = args.only
            && budget.metric.as_str() != only.as_str()
        {
            continue;
        }

        for &os in &budget.os {
            let key = budgets.baseline_key(budget, os);
            let baseline_value = baseline.values.get(&key).copied();

            // Only the current OS is measured natively; other OSes attempt
            // a cross build and become `not_measured` with a hint (AC-3).
            if os != current_os() {
                rows.push(report::build_row(
                    budget.metric,
                    budget.target,
                    os,
                    vec![],
                    0.0,
                    budget.limit,
                    baseline_value,
                    core_status(None, budget, baseline_value, machine_matches),
                    Some(format!(
                        "not measured on {}; no native {} build on this machine",
                        os.as_str(),
                        os.as_str()
                    )),
                ));
                continue;
            }

            let (samples, value, note) = measure_metric(budget, os)?;
            let status = core_status(Some(value), budget, baseline_value, machine_matches);
            if status.is_fail() {
                any_fail = true;
            }
            rows.push(report::build_row(
                budget.metric,
                budget.target,
                os,
                samples,
                value,
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

    // --accept: refresh baseline.json only when nothing failed (AC-10).
    if args.accept {
        if any_fail {
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

/// Measures one budget row on the current OS. For task 1 only bundle_size
/// is measured; every other metric returns `not_measured` until its task
/// lands (spec 0007, Build plan task 1).
fn measure_metric(
    budget: &budgets::BudgetRow,
    os: budgets::Os,
) -> anyhow::Result<(Vec<f64>, f64, Option<String>)> {
    match budget.metric {
        budgets::Metric::BundleSize => {
            let value = size::measure(budget.target, os)
                .ok_or_else(|| anyhow::anyhow!("could not measure bundle size"))?;
            Ok((vec![value], value, None))
        }
        _ => {
            // Runtime metrics (cold start, memory, scroll, script load) are
            // implemented in later tasks; they are not_measured here.
            Ok((vec![], 0.0, Some("not yet implemented".to_owned())))
        }
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

/// A coarse timestamp without adding the `time` or `chrono` dependency.
fn record_timestamp() -> String {
    let now = std::time::SystemTime::now();
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Crude ISO-8601-ish from seconds since epoch; good enough for a record.
    let days = secs / 86400;
    let year = 1970 + (days / 365) as u32;
    format!("{year}-10-21")
}
