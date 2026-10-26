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
mod runner;
mod serve;
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
/// The pinned pages, served locally for the runtime scenarios.
const PAGES_PATH: &str = "perf/pages";

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

    // The local page server for the scenarios that load a page (spec 0007,
    // Pinned pages). Started once; the OS picks a free port.
    let pages = serve::PageServer::start(Path::new(PAGES_PATH))?;
    let article_url = format!("{}/article/", pages.base_url);
    let script_url = format!("{}/script/", pages.base_url);

    // Measure every budget row that applies to this OS.
    let mut rows = Vec::new();
    let mut any_fail = false;
    let mut any_hard_fail = false;
    let mut v8_signal = report::V8Signal {
        script_share: 0.0,
        triggered: false,
    };
    // The two memory rows share one set of runs (spec 0007, AC-5).
    let mut memory_cache: Option<Result<Vec<runner::MemorySample>, runner::RunError>> = None;
    // The after browse row has its own runs: the session is part of the
    // measurement (spec 0032, AC-11).
    let mut browse_cache: Option<Result<Vec<runner::MemorySample>, runner::RunError>> = None;

    for budget in &budgets.rows {
        if let Some(ref only) = args.only
            && budget.metric.as_str() != only.as_str()
        {
            continue;
        }

        for &os in &budget.os {
            let key = budgets.baseline_key(budget, os);
            let baseline_value = baseline.values.get(&key).copied();

            let measurement = measure_metric(
                budget,
                os,
                &article_url,
                &script_url,
                &mut memory_cache,
                &mut browse_cache,
            )?;
            // The V8 signal (AC-7): reported, never failing the gate.
            if let Some(share) = measurement.v8_share {
                v8_signal = report::V8Signal {
                    script_share: share,
                    triggered: share > 0.6,
                };
                if v8_signal.triggered {
                    println!(
                        "V8 signal: script share {:.0}% > 60%, see scope Deferred \"Optional V8 build\"",
                        share * 100.0
                    );
                }
            }
            let status = measurement.forced.unwrap_or_else(|| {
                core_status(measurement.value, budget, baseline_value, machine_matches)
            });
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
                measurement.samples,
                measurement.value.unwrap_or(0.0),
                budget.limit,
                budget.goal,
                baseline_value,
                status,
                measurement.note,
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
        v8_signal,
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

/// What measuring one row produced.
struct Measurement {
    /// The measured samples for this run.
    samples: Vec<f64>,
    /// The aggregated value compared to the budget, or `None` when the row
    /// could not be measured.
    value: Option<f64>,
    /// A one line note for the report.
    note: Option<String>,
    /// When measuring itself failed (a timed out run, a crash), the row
    /// fails regardless of the budget (AC-13); otherwise the pure status
    /// function decides.
    forced: Option<CoreStatus>,
    /// The script share, when this row measured the script workload (AC-7).
    v8_share: Option<f64>,
}

impl Measurement {
    /// A measurement whose run failed: the reason goes in the note, the
    /// row fails (AC-13).
    fn failed(err: runner::RunError) -> Self {
        Self::failed_note(err.to_string())
    }

    /// A measurement whose run failed, with the reason already rendered.
    fn failed_note(note: String) -> Self {
        Self {
            samples: vec![],
            value: None,
            note: Some(note),
            forced: Some(CoreStatus::Fail),
            v8_share: None,
        }
    }

    /// An honest not-measured row with a reason.
    fn not_measured(note: impl Into<String>) -> Self {
        Self {
            samples: vec![],
            value: None,
            note: Some(note.into()),
            forced: None,
            v8_share: None,
        }
    }

    /// A measured row.
    fn measured(samples: Vec<f64>, value: f64, note: Option<String>) -> Self {
        Self {
            samples,
            value: Some(value),
            note,
            forced: None,
            v8_share: None,
        }
    }
}

/// Measures one budget row on the current OS. The measured value is `None`
/// when the row could not be measured: a missing cross tool (AC-3), or a
/// footprint reader this OS lacks (spec 0007, Failure handling). An
/// unmeasured row is honestly `not_measured`, never a fake pass.
fn measure_metric(
    budget: &budgets::BudgetRow,
    os: budgets::Os,
    article_url: &str,
    script_url: &str,
    memory_cache: &mut Option<Result<Vec<runner::MemorySample>, runner::RunError>>,
    browse_cache: &mut Option<Result<Vec<runner::MemorySample>, runner::RunError>>,
) -> anyhow::Result<Measurement> {
    let exe = || build::native_exe(budget.target);
    let runs_dir = || report::perf_dir().join("runs");
    match budget.metric {
        budgets::Metric::BundleSize => match size::measure(budget.target, os) {
            Some(value) => Ok(Measurement::measured(vec![value], value, None)),
            None if os == current_os() => Err(anyhow::anyhow!("could not measure bundle size")),
            // AC-3: the cross tool is missing or the cross build failed.
            None => Ok(Measurement::not_measured(format!(
                "not measured: {}",
                cross_hint(os)
            ))),
        },
        budgets::Metric::ColdStart => {
            // A run that times out or crashes fails this row, it never
            // aborts the gate (AC-13).
            let first = match runner::cold_start(&exe(), &runs_dir()) {
                Ok((samples, _)) => samples,
                Err(err) => return Ok(Measurement::failed(err)),
            };
            let first_median = runner::median(&first);
            if first_median > budget.limit {
                // AC-9: a timing miss gets one rerun with the same repeat
                // count; the metric fails only when the second result also
                // misses, and the report shows both results.
                let second = match runner::cold_start(&exe(), &runs_dir()) {
                    Ok((samples, _)) => samples,
                    Err(err) => return Ok(Measurement::failed(err)),
                };
                let second_median = runner::median(&second);
                let mut samples = first;
                samples.extend_from_slice(&second);
                return Ok(Measurement {
                    samples,
                    value: Some(second_median),
                    note: Some(format!(
                        "near miss on the first run ({first_median:.1} ms); rerun once"
                    )),
                    forced: None,
                    v8_share: None,
                });
            }
            Ok(Measurement::measured(first, first_median, None))
        }
        budgets::Metric::MemoryTotal | budgets::Metric::MemoryContent => {
            // The two memory rows share one set of 3 runs (spec 0007, AC-5).
            if memory_cache.is_none() {
                *memory_cache = Some(runner::memory(&exe(), article_url, &runs_dir()));
            }
            let Some(samples) = memory_cache.as_ref() else {
                return Ok(Measurement::not_measured("not measured: no memory runs"));
            };
            Ok(memory_measurement(
                samples,
                budget.metric == budgets::Metric::MemoryContent,
            ))
        }
        budgets::Metric::MemoryAfterBrowse => {
            // A browsing session, then the same sample as the memory rows
            // (spec 0032, AC-11): the row catches memory that outlives its
            // page, which the single page rows cannot see.
            if browse_cache.is_none() {
                *browse_cache = Some(runner::browse(&exe(), article_url, &runs_dir()));
            }
            let Some(samples) = browse_cache.as_ref() else {
                return Ok(Measurement::not_measured("not measured: no browse runs"));
            };
            Ok(memory_measurement(samples, false))
        }
        budgets::Metric::ScrollFrameCpuP95 => {
            let first = match runner::scroll(&exe(), article_url, &runs_dir()) {
                Ok(frames) => frames,
                Err(err) => return Ok(Measurement::failed(err)),
            };
            let first_p95 = runner::percentile(&first, 95.0) / 1000.0;
            if first_p95 > budget.limit {
                // AC-9, as for every timing metric.
                let second = match runner::scroll(&exe(), article_url, &runs_dir()) {
                    Ok(frames) => frames,
                    Err(err) => return Ok(Measurement::failed(err)),
                };
                let second_p95 = runner::percentile(&second, 95.0) / 1000.0;
                let mut samples = first;
                samples.extend_from_slice(&second);
                let samples = samples.into_iter().map(|us| us / 1000.0).collect();
                return Ok(Measurement {
                    samples,
                    value: Some(second_p95),
                    note: Some(format!(
                        "near miss on the first run ({first_p95:.2} ms); rerun once"
                    )),
                    forced: None,
                    v8_share: None,
                });
            }
            let samples = first.into_iter().map(|us| us / 1000.0).collect();
            Ok(Measurement::measured(samples, first_p95, None))
        }
        budgets::Metric::ScriptLoad => {
            let (first_loads, first_share) =
                match runner::script_load(&exe(), script_url, &runs_dir()) {
                    Ok(result) => result,
                    Err(err) => return Ok(Measurement::failed(err)),
                };
            let first_median = runner::median(&first_loads);
            if first_median > budget.limit {
                // AC-9, as for every timing metric.
                let (second_loads, second_share) =
                    match runner::script_load(&exe(), script_url, &runs_dir()) {
                        Ok(result) => result,
                        Err(err) => return Ok(Measurement::failed(err)),
                    };
                let second_median = runner::median(&second_loads);
                let mut samples = first_loads;
                samples.extend_from_slice(&second_loads);
                return Ok(Measurement {
                    samples,
                    value: Some(second_median),
                    note: Some(format!(
                        "near miss on the first run ({first_median:.1} ms); rerun once"
                    )),
                    forced: None,
                    v8_share: Some(second_share),
                });
            }
            Ok(Measurement {
                samples: first_loads,
                value: Some(first_median),
                note: None,
                forced: None,
                v8_share: Some(first_share),
            })
        }
    }
}

/// The measured values of a memory row from its 3 runs: browser plus
/// content for `memory_total` and `memory_after_browse`, content alone for
/// `memory_content`, in MB (10^6 bytes). A run failure fails the row (AC-13);
/// a missing footprint reader leaves it unmeasured, never a fake pass (spec
/// 0007, Failure handling).
fn memory_measurement(
    samples: &Result<Vec<runner::MemorySample>, runner::RunError>,
    content_only: bool,
) -> Measurement {
    let samples = match samples {
        Ok(samples) => samples,
        Err(err) => return Measurement::failed_note(err.to_string()),
    };
    // A footprint reader this OS lacks leaves the row unmeasured.
    if samples
        .iter()
        .any(|sample| sample.browser_bytes.is_none() || sample.content_bytes.is_none())
    {
        return Measurement::not_measured("not measured: no footprint reader on this OS");
    }
    let values: Vec<f64> = samples
        .iter()
        .map(|sample| {
            let bytes = if content_only {
                sample.content_bytes.unwrap_or(0)
            } else {
                sample.browser_bytes.unwrap_or(0) + sample.content_bytes.unwrap_or(0)
            };
            bytes as f64 / 1_000_000.0
        })
        .collect();
    let value = runner::median(&values);
    Measurement::measured(values, value, None)
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
}
