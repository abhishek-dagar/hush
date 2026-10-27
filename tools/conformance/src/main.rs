//! `conformance`: runs WPT reftests and crashtests against the engine and
//! reports a pass rate per area (spec 0005). A developer tool, never shipped.
//!
//! CLI (spec 0005, API surface):
//!   `conformance` / `conformance run` [--area <name>…] [--path <prefix>…]
//!                                       [--jobs <n>] [--update]
//!                                       [--crashtests-only] [--wpt-dir <path>]
//!   `conformance fetch`                 [--wpt-dir <path>]
//!   `conformance --worker`              --wpt-dir <path> --subset <path>
//!                                       [--no-render]

mod compare;
mod discover;
mod expectations;
mod fetch;
mod pool;
mod report;
mod serve;
mod sites;
mod status;
mod subset;
mod worker;

use std::collections::BTreeMap;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use lexopt::ValueExt;
use render::Renderer;

use crate::expectations::Expectations;
use crate::pool::{RunResult, run_pool};
use crate::report::{AreaSummary, conformance_dir, print_summary, write_results};
use crate::sites::Sites;
use crate::status::Status;
use crate::subset::Subset;

/// Where the checked-in `subset.toml` and `expectations/` live, relative to
/// this crate's manifest dir (spec 0005, Where things live).
const SUBSET_NAME: &str = "subset.toml";
const EXPECTATIONS_DIR: &str = "expectations";

/// Parsed command line.
struct Args {
    command: Command,
    /// Override for the snapshot folder (defaults to the OS cache dir).
    wpt_dir: Option<String>,
    /// Override for the subset file (worker/test only).
    subset: Option<String>,
    /// `--area` filters, repeatable; only narrows (AC-17).
    areas: Vec<String>,
    /// `--path` filters, repeatable; only narrows (AC-17).
    paths: Vec<String>,
    jobs: usize,
    update: bool,
    crashtests_only: bool,
    no_render: bool,
    worker: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Run,
    Fetch,
    /// `conformance sites` validates the checked-in proof sites; with
    /// `--capture` it re-renders each page in Chrome and writes `chrome.json`
    /// and `chrome.png` beside them (spec 0009, AC-22).
    Sites {
        capture: bool,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(1)
        }
    }
}

/// Entry point. Setup failures (no `git`, a bad `subset.toml`, no GPU when
/// required) are converted to exit 2 here; regressions to exit 1.
fn run() -> Result<ExitCode> {
    let args = parse_args()?;

    // Worker mode: run one test at a time over line JSON on stdin/stdout.
    if args.worker {
        let wpt_dir = std::path::Path::new(
            args.wpt_dir
                .as_deref()
                .context("--worker requires --wpt-dir")?,
        );
        let subset_path = std::path::Path::new(
            args.subset
                .as_deref()
                .context("--worker requires --subset")?,
        );
        worker::run(wpt_dir, args.no_render, subset_path).context("worker failed")?;
        return Ok(ExitCode::from(0));
    }

    // Locate the checked-in subset file.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .context("CARGO_MANIFEST_DIR is not set (run via `cargo run -p conformance`)")?;

    // `conformance sites` does not need the WPT snapshot: validate (and
    // optionally capture) the checked-in proof sites and return.
    if matches!(args.command, Command::Sites { .. }) {
        let sites = Sites::load(std::path::Path::new(&manifest_dir))
            .with_context(|| "could not load proof sites")?;
        if let Command::Sites { capture } = args.command {
            if capture {
                return sites
                    .capture(std::path::Path::new(&manifest_dir))
                    .context("site capture failed");
            }
            println!("{} proof site(s) ready for capture:", sites.sites.len());
            for site in &sites.sites {
                println!("  {}", site.name);
            }
            return Ok(ExitCode::from(0));
        }
    }

    let subset_path: std::path::PathBuf = args
        .subset
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(&manifest_dir).join(SUBSET_NAME));
    let subset = Subset::load(&subset_path)
        .with_context(|| format!("could not read subset file {}", subset_path.display()))?;

    if args.command == Command::Fetch {
        // `conformance fetch`: only the snapshot step (AC-2).
        let dir = wpt_dir_resolve(&args, &subset)?;
        fetch::ensure(
            &dir,
            &subset.wpt_commit,
            &subset.sparse_paths(),
            fetch::WPT_REMOTE,
        )
        .context("could not fetch the WPT snapshot")?;
        return Ok(ExitCode::from(0));
    }

    let dir = wpt_dir_resolve(&args, &subset)?;

    // AC-2 / AC-15: ensure the snapshot exists before discovering tests.
    fetch::ensure(
        &dir,
        &subset.wpt_commit,
        &subset.sparse_paths(),
        fetch::WPT_REMOTE,
    )
    .context("could not fetch the WPT snapshot")?;

    // AC-3: discover and classify every test under the areas.
    let mut tests = discover::discover(&dir, &subset).context("test discovery failed")?;

    // AC-17: `--area` and `--path` only narrow the subset.
    tests.retain(|test| {
        let area_ok = args.areas.is_empty() || args.areas.contains(&test.area);
        let path_ok = args.paths.is_empty()
            || args
                .paths
                .iter()
                .any(|prefix| test.path.starts_with(prefix.as_str()));
        area_ok && path_ok
    });

    // With --crashtests-only only crashtests run (AC-13).
    let crashtest_only = args.crashtests_only;
    if crashtest_only {
        tests.retain(|test| matches!(test.kind, discover::Kind::Crashtest));
    }

    // AC-13: probe for a GPU adapter once unless crashtests-only.
    let no_render = crashtest_only || args.no_render;
    if !no_render && Renderer::new_headless(1, 1).is_err() {
        eprintln!("error: no GPU adapter available");
        return Ok(ExitCode::from(2));
    }

    let jobs = if args.jobs > 0 {
        args.jobs
    } else {
        std::thread::available_parallelism().map_or(1, |n| n.get())
    };

    // AC-12: clear the output directory at the start of the run, so the
    // failure images the workers write are not wiped at the end.
    report::clear_output().context("could not clear the output directory")?;
    let started = std::time::Instant::now();

    // AC-8 / AC-9: run the tests across worker processes.
    let results = run_pool(&dir, no_render, &subset_path, jobs, tests);

    // Group results by area and gate against expectations (AC-10).
    let mut by_area: BTreeMap<String, Vec<RunResult>> = BTreeMap::new();
    for result in &results {
        by_area
            .entry(result.test.area.clone())
            .or_default()
            .push(result.clone());
    }

    let expectations_dir = std::path::Path::new(&manifest_dir).join(EXPECTATIONS_DIR);

    let mut area_summaries = Vec::new();
    let mut regressions: Vec<String> = Vec::new();
    let mut unexpected_passes: Vec<String> = Vec::new();
    let mut stale_expectations: Vec<String> = Vec::new();
    let mut under_floor: Vec<String> = Vec::new();
    let mut all_tests_json: Vec<serde_json::Value> = Vec::new();

    for area in &subset.areas {
        let area_results: Vec<RunResult> = by_area.get(&area.name).cloned().unwrap_or_default();

        let expectations_text =
            std::fs::read_to_string(expectations_dir.join(format!("{}.txt", area.name)))
                .unwrap_or_default();
        let expectations = Expectations::parse(&area.name, &expectations_text)
            .with_context(|| format!("invalid expectations for {}", area.name))?;

        let discovered: Vec<String> = area_results.iter().map(|r| r.test.path.clone()).collect();

        let area_results_refs: Vec<(String, Status)> = area_results
            .iter()
            .map(|r| (r.test.path.clone(), r.status))
            .collect();

        // Regressions and unexpected passes (AC-10).
        regressions.extend(
            expectations
                .regressions(&area_results_refs)
                .into_iter()
                .map(|s| s.to_owned()),
        );
        unexpected_passes.extend(
            expectations
                .unexpected_passes(&area_results_refs)
                .into_iter()
                .map(|s| s.to_owned()),
        );

        // Stale lines: only when the whole area ran (AC-10, AC-11).
        let whole_area =
            args.paths.is_empty() && (args.areas.is_empty() || args.areas.contains(&area.name));
        if whole_area {
            stale_expectations.extend(expectations.stale(&discovered));
        }

        // --update rewrites expectations (AC-11).
        if args.update {
            let mut expectations = expectations.clone();
            expectations.update(&area_results_refs, &discovered, whole_area);
            let path = expectations_dir.join(format!("{}.txt", area.name));
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            expectations
                .save(&path)
                .context("could not write expectations")?;
        }

        // Build the summary row (AC-16).
        let mut total = 0usize;
        let mut pass = 0usize;
        let mut fail = 0usize;
        let mut crash = 0usize;
        let mut timeout = 0usize;
        let mut error = 0usize;
        let mut skip = 0usize;
        for r in &area_results {
            total += 1;
            match r.status {
                Status::Pass => pass += 1,
                Status::Fail => fail += 1,
                Status::Crash => crash += 1,
                Status::Timeout => timeout += 1,
                Status::Error => error += 1,
                Status::Skip => skip += 1,
            }
        }
        let pass_rate = if total - skip == 0 {
            0.0
        } else {
            (pass as f64) / ((total - skip) as f64) * 100.0
        };
        // AC-21: a run whose pass rate for an area is under its floor exits 1
        // and names the area. A narrowed run measures too few tests to speak
        // for the area, so only a whole area run is gated. The summary still
        // prints, and `results.json` still lands, so the run can be read.
        if let Some(floor) = area.min_pass_rate
            && whole_area
            && pass_rate < floor as f64
        {
            under_floor.push(format!(
                "area `{}` pass rate {:.1}% is below the min_pass_rate floor of {:.1}%",
                area.name, pass_rate, floor
            ));
        }
        area_summaries.push(AreaSummary {
            name: area.name.clone(),
            total,
            pass,
            fail,
            crash,
            timeout,
            error,
            skip,
            pass_rate,
        });

        for r in &area_results {
            all_tests_json.push(serde_json::json!({
                "path": r.test.path,
                "area": r.test.area,
                "kind": match r.test.kind {
                    discover::Kind::Reftest => "reftest",
                    discover::Kind::Crashtest => "crashtest",
                    discover::Kind::Skip(_) => "skip",
                },
                "status": r.status.as_str(),
                "expected": expectations
                    .expected(&r.test.path)
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>(),
                "retried": r.retried,
                "duration_ms": r.duration_ms,
                "parsed_as": "html",
                "message": r.message,
            }));
        }
    }

    // AC-12: write the reports; the output directory was cleared at the
    // start of the run.
    print_summary(&area_summaries);

    let started_at = iso8601_now();
    let results_json = serde_json::json!({
        "wpt_commit": subset.wpt_commit,
        "started_at": started_at,
        "duration_ms": started.elapsed().as_millis() as u64,
        "jobs": jobs,
        "areas": area_summaries.iter().map(|s| serde_json::json!({
            "name": s.name,
            "total": s.total,
            "pass": s.pass,
            "fail": s.fail,
            "crash": s.crash,
            "timeout": s.timeout,
            "error": s.error,
            "skip": {
                "server": 0,
                "manual": 0,
                "testharness": 0,
                "other_kind": s.skip,
            },
            "pass_rate": s.pass_rate,
        })).collect::<Vec<_>>(),
        "tests": all_tests_json,
        "regressions": regressions,
        "unexpected_passes": unexpected_passes,
        "stale_expectations": stale_expectations,
    });
    write_results(&conformance_dir().join("results.json"), &results_json)
        .context("could not write results.json")?;

    if !regressions.is_empty() {
        eprintln!();
        eprintln!("Regressions:");
        for r in &regressions {
            eprintln!("  {r}");
        }
    }
    if !unexpected_passes.is_empty() {
        eprintln!();
        eprintln!("Unexpected passes:");
        for p in &unexpected_passes {
            eprintln!("  {p}");
        }
    }
    if !stale_expectations.is_empty() {
        eprintln!();
        eprintln!("Stale expectations:");
        for s in &stale_expectations {
            eprintln!("  {s}");
        }
    }
    if !under_floor.is_empty() {
        eprintln!();
        eprintln!("Below the min_pass_rate floor:");
        for area in &under_floor {
            eprintln!("  {area}");
        }
    }

    let code = if regressions.is_empty() && under_floor.is_empty() {
        ExitCode::from(0)
    } else {
        ExitCode::from(1)
    };
    Ok(code)
}

/// Resolves the snapshot folder: `--wpt-dir` overrides the cache-dir default
/// (spec 0005, Value sourcing).
fn wpt_dir_resolve(args: &Args, subset: &Subset) -> Result<std::path::PathBuf> {
    if let Some(dir) = &args.wpt_dir {
        return Ok(std::path::PathBuf::from(dir));
    }
    let cache = dirs::cache_dir().context("could not determine the OS cache directory")?;
    Ok(cache
        .join("rust-browser")
        .join("wpt")
        .join(&subset.wpt_commit))
}

fn parse_args() -> Result<Args> {
    let mut parser = lexopt::Parser::from_env();
    let mut command: Option<Command> = None;
    let mut wpt_dir: Option<String> = None;
    let mut subset: Option<String> = None;
    let mut areas: Vec<String> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    let mut jobs: usize = 0;
    let mut update = false;
    let mut crashtests_only = false;
    let mut no_render = false;
    let mut worker = false;
    let mut capture = false;

    while let Some(arg) = parser.next()? {
        match arg {
            lexopt::Arg::Long("help") | lexopt::Arg::Short('h') => {
                print_help();
                std::process::exit(0);
            }
            lexopt::Arg::Long("capture") => capture = true,
            lexopt::Arg::Long("worker") => worker = true,
            lexopt::Arg::Long("no-render") => no_render = true,
            lexopt::Arg::Long("wpt-dir") => wpt_dir = Some(parser.value()?.string()?),
            lexopt::Arg::Long("subset") => subset = Some(parser.value()?.string()?),
            lexopt::Arg::Long("area") => areas.push(parser.value()?.string()?),
            lexopt::Arg::Long("path") => paths.push(parser.value()?.string()?),
            lexopt::Arg::Long("jobs") => jobs = parser.value()?.parse()?,
            lexopt::Arg::Long("update") => update = true,
            lexopt::Arg::Long("crashtests-only") => crashtests_only = true,
            lexopt::Arg::Long("fetch") => command = Some(Command::Fetch),
            lexopt::Arg::Long("run") => command = Some(Command::Run),
            lexopt::Arg::Value(val) => {
                let s = val.string()?;
                match s.as_str() {
                    "run" => command = Some(Command::Run),
                    "fetch" => command = Some(Command::Fetch),
                    "sites" => command = Some(Command::Sites { capture }),
                    other => bail!("unknown argument `{other}`"),
                }
            }
            _ => bail!("unexpected argument `{arg:?}`"),
        }
    }

    // `sites --capture` and `--capture sites` mean the same thing: apply a
    // `--capture` that came after the `sites` positional.
    if let Some(Command::Sites { capture: false }) = command
        && capture
    {
        command = Some(Command::Sites { capture: true });
    }

    Ok(Args {
        command: command.unwrap_or(Command::Run),
        wpt_dir,
        subset,
        areas,
        paths,
        jobs,
        update,
        crashtests_only,
        no_render,
        worker,
    })
}

fn print_help() {
    println!("conformance - runs WPT reftests and crashtests against the engine (spec 0005)");
    println!();
    println!("Usage:");
    println!("    conformance [run] [--area <name>…] [--path <prefix>…] [--jobs <n>]");
    println!("                  [--update] [--crashtests-only] [--wpt-dir <path>]");
    println!("    conformance fetch [--wpt-dir <path>]");
    println!("    conformance sites [--capture]");
    println!("    conformance --worker --wpt-dir <path> --subset <path> [--no-render]");
    println!();
    println!("Options:");
    println!("    --area <name>        Only run tests in the given area (repeatable)");
    println!(
        "    --path <prefix>      Only run tests whose path starts with the prefix (repeatable)"
    );
    println!("    --jobs <n>           Number of worker processes (default: number of cores)");
    println!("    --update             Rewrite expectations files from the results");
    println!("    --crashtests-only    Run only crashtests; works without a GPU");
    println!("    --capture            With `sites`: capture Chrome's rendering of each site");
    println!("    --wpt-dir <path>     Snapshot folder (default: OS cache dir)");
    println!("    --subset <path>      Subset file (worker/test only)");
    println!("    --no-render          Worker: do not create a renderer (crashtests-only)");
    println!("    --worker             Internal: run one test at a time on stdin/stdout");
    println!("    -h, --help           Print this help");
}

/// The current time as an ISO-8601 UTC string, computed with `std::time` math
/// only (spec 0005, Value sourcing: `started_at`). No date crate.
fn iso8601_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    civil_from_days(secs / 86400)
        .map(|(y, m, d)| {
            let secs = secs % 86400;
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                y,
                m,
                d,
                secs / 3600,
                (secs % 3600) / 60,
                secs % 60
            )
        })
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned())
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 -> (year, month,
/// day). Correct for the Gregorian calendar including leap years.
fn civil_from_days(days: u64) -> Option<(u32, u32, u32)> {
    let z = (days as i64) + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    Some((y as u32, m as u32, d as u32))
}
