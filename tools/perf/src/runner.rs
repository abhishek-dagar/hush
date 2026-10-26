//! Driving the browser through one perf scenario (spec 0007, Scenarios;
//! AC-4, AC-13).
//!
//! Spawns the release browser with `--perf`, waits for it to write its one
//! run report and exit by itself, and reads the report back. A run that
//! hangs is killed at 30 s and fails its metric with `run timed out`
//! (AC-13); a browser that exits nonzero without a report fails the metric
//! with its exit code.

use std::path::Path;
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// One run may take at most this long before the harness kills it (AC-13).
const RUN_TIMEOUT: Duration = Duration::from_secs(30);

/// How a poll loop sleeps between exit checks.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Errors of one scenario run; each becomes that metric's failure reason
/// (AC-13).
#[derive(Debug, thiserror::Error)]
pub(crate) enum RunError {
    /// The browser did not finish within 30 s (AC-13).
    #[error("run timed out")]
    TimedOut,
    /// The browser exited nonzero and left no report (AC-13).
    #[error("the browser exited with code {0} before writing a report")]
    Exit(i32),
    /// The browser died to a signal and left no report.
    #[error("the browser was killed by a signal before writing a report")]
    Signalled,
    /// The report could not be read or the browser could not start.
    #[error("could not run the browser or read its report: {0}")]
    Io(#[from] std::io::Error),
    /// The report is there but lacks the mark the scenario produces.
    #[error("the run report is missing {0}")]
    Missing(&'static str),
}

/// The one run report the browser writes (spec 0007, Run report): line
/// based text, one `key value` pair per line, matching the format the
/// embed crate's private `perf` module writes.
#[derive(Debug, Default)]
pub(crate) struct RunReport {
    /// Which scenario ran.
    pub(crate) scenario: String,
    /// First frame with page content, Unix µs (`start`).
    pub(crate) first_frame_unix_us: Option<u64>,
    /// Navigation start to `load`, µs (`memory`, `scroll`, `load`).
    pub(crate) nav_to_load_us: Option<u64>,
    /// Time spent inside the script engine, µs (`load`).
    pub(crate) script_us: Option<u64>,
    /// Each scroll frame's CPU time, µs (`scroll`).
    pub(crate) scroll_frames_us: Vec<u32>,
    /// Physical footprint of the browser process, bytes (`memory`).
    pub(crate) footprint_browser_bytes: Option<u64>,
    /// Physical footprint of the content process, bytes (`memory`).
    pub(crate) footprint_content_bytes: Option<u64>,
}

impl RunReport {
    /// Reads and parses one run report.
    fn read(path: &Path) -> Result<Self, RunError> {
        let text = std::fs::read_to_string(path)?;
        Ok(Self::parse(&text))
    }

    /// Parses the line format. Unknown keys are ignored, so an older gate
    /// still reads a newer browser's report.
    fn parse(text: &str) -> Self {
        let mut report = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once(' ') else {
                continue;
            };
            match key {
                "scenario" => report.scenario = value.to_owned(),
                "first_frame_unix_us" => report.first_frame_unix_us = value.parse().ok(),
                "nav_to_load_us" => report.nav_to_load_us = value.parse().ok(),
                "script_us" => report.script_us = value.parse().ok(),
                "scroll_frames_us" => {
                    report.scroll_frames_us =
                        value.split(',').filter_map(|n| n.parse().ok()).collect();
                }
                "footprint_browser_bytes" => report.footprint_browser_bytes = value.parse().ok(),
                "footprint_content_bytes" => report.footprint_content_bytes = value.parse().ok(),
                _ => {}
            }
        }
        report
    }
}

/// One finished scenario run: the report the browser wrote, and the Unix
/// µs stamp the harness took just before spawning it.
pub(crate) struct FinishedRun {
    /// What the browser recorded.
    pub(crate) report: RunReport,
    /// The harness's spawn stamp, Unix µs (AC-4: cold start starts here).
    pub(crate) spawned_unix_us: u64,
}

/// Spawns the browser for `scenario`, waits for it to write its report and
/// exit, and reads the report. The browser self exits when the scenario
/// ends; the harness never tells it to stop early. `url` is the page to
/// open, for the scenarios that load one (spec 0007, Scenarios).
pub(crate) fn run_scenario(
    exe: &Path,
    scenario: &str,
    url: Option<&str>,
    report_dir: &Path,
    run_index: usize,
) -> Result<FinishedRun, RunError> {
    let report_path = report_dir.join(format!("{scenario}-{run_index}.perf"));
    let report_path = std::env::current_dir()
        .map(|cwd| cwd.join(&report_path))
        .unwrap_or_else(|_| report_path);
    let _ = std::fs::remove_file(&report_path);

    let spawned_unix_us = unix_now_us();
    let mut command = Command::new(exe);
    command
        .args(["--perf", scenario, "--perf-report"])
        .arg(&report_path);
    if let Some(url) = url {
        command.arg(url);
    }
    let mut child = command.spawn()?;
    let status = wait_with_timeout(&mut child)?;
    let report = match RunReport::read(&report_path) {
        Ok(report) => report,
        Err(err) => {
            // No report: fail with the exit code when there is one (AC-13).
            if !status.success() {
                return Err(exit_error(status));
            }
            return Err(err);
        }
    };
    let _ = std::fs::remove_file(&report_path);
    Ok(FinishedRun {
        report,
        spawned_unix_us,
    })
}

/// Waits for `child`, killing it at 30 s so the gate never hangs (AC-13).
fn wait_with_timeout(child: &mut Child) -> Result<ExitStatus, RunError> {
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            child.wait()?;
            return Err(RunError::TimedOut);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The failure reason for a nonzero exit (AC-13).
fn exit_error(status: ExitStatus) -> RunError {
    match status.code() {
        Some(code) => RunError::Exit(code),
        None => RunError::Signalled,
    }
}

/// Cold start samples (AC-4): 11 runs, the first dropped, the remaining 10
/// reported in ms. Each value is the wall time from just before spawn to
/// the browser's first frame with page content.
pub(crate) fn cold_start(
    exe: &Path,
    report_dir: &Path,
) -> Result<(Vec<f64>, Option<String>), RunError> {
    // The first run warms caches and is dropped (AC-4).
    run_scenario(exe, "start", None, report_dir, 0)?;
    let mut samples = Vec::with_capacity(10);
    for index in 1..=10 {
        let run = run_scenario(exe, "start", None, report_dir, index)?;
        let first = run
            .report
            .first_frame_unix_us
            .ok_or(RunError::Missing("first_frame_unix_us"))?;
        let us = first.saturating_sub(run.spawned_unix_us);
        samples.push(us as f64 / 1000.0);
    }
    Ok((samples, None))
}

/// One memory scenario run's numbers (AC-5).
#[derive(Debug, Clone, Copy)]
pub(crate) struct MemorySample {
    /// The browser process's physical footprint, bytes.
    pub(crate) browser_bytes: Option<u64>,
    /// The content process's physical footprint, bytes.
    pub(crate) content_bytes: Option<u64>,
}

/// Memory samples (AC-5): 3 runs of the memory scenario against the
/// pinned article page. `None` footprints mean this OS has no reader yet.
pub(crate) fn memory(
    exe: &Path,
    url: &str,
    report_dir: &Path,
) -> Result<Vec<MemorySample>, RunError> {
    footprints(exe, "memory", url, report_dir)
}

/// Browse samples (spec 0032, AC-11): 3 runs of the browse scenario, which
/// loads the article, visits the start page, comes back to the article and
/// samples its footprints 2 s after the last `load`. Nothing is measured
/// during the trip: the point is that memory a page leaves behind shows up
/// here after the session, where the single page `memory` row cannot see
/// it.
pub(crate) fn browse(
    exe: &Path,
    url: &str,
    report_dir: &Path,
) -> Result<Vec<MemorySample>, RunError> {
    footprints(exe, "browse", url, report_dir)
}

/// 3 runs of `scenario`, each read for both footprints (AC-5).
fn footprints(
    exe: &Path,
    scenario: &str,
    url: &str,
    report_dir: &Path,
) -> Result<Vec<MemorySample>, RunError> {
    let mut out = Vec::with_capacity(3);
    for index in 0..3 {
        let run = run_scenario(exe, scenario, Some(url), report_dir, index)?;
        out.push(MemorySample {
            browser_bytes: run.report.footprint_browser_bytes,
            content_bytes: run.report.footprint_content_bytes,
        });
    }
    Ok(out)
}

/// Pooled scroll frame CPU times, µs (AC-6): 3 runs of 240 wheel fed steps
/// each, all frames pooled for the p95. A run that recorded no frames at
/// all (a window that never presented) is an error, never a fake zero.
pub(crate) fn scroll(exe: &Path, url: &str, report_dir: &Path) -> Result<Vec<f64>, RunError> {
    let mut frames = Vec::new();
    for index in 0..3 {
        let run = run_scenario(exe, "scroll", Some(url), report_dir, index)?;
        frames.extend(run.report.scroll_frames_us.iter().map(|&us| f64::from(us)));
    }
    if frames.is_empty() {
        return Err(RunError::Missing("scroll_frames_us"));
    }
    Ok(frames)
}

/// Script workload stats (AC-7): the 5 load times in ms, and the script
/// share (median script time over median load time).
pub(crate) fn script_load(
    exe: &Path,
    url: &str,
    report_dir: &Path,
) -> Result<(Vec<f64>, f64), RunError> {
    let mut loads = Vec::with_capacity(5);
    let mut scripts = Vec::with_capacity(5);
    for index in 0..5 {
        let run = run_scenario(exe, "load", Some(url), report_dir, index)?;
        let nav = run
            .report
            .nav_to_load_us
            .ok_or(RunError::Missing("nav_to_load_us"))?;
        loads.push(nav as f64);
        scripts.push(run.report.script_us.unwrap_or(0) as f64);
    }
    let load_us = median(&loads);
    let share = median(&scripts) / load_us;
    let loads_ms = loads.into_iter().map(|us| us / 1000.0).collect();
    Ok((loads_ms, share))
}

/// The nearest rank percentile (spec 0007, Value sourcing): the smallest
/// sample at or above the `p`th percentile of the sorted samples. Empty
/// input gives 0.0; callers never aggregate nothing.
pub(crate) fn percentile(samples: &[f64], p: f64) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (((p / 100.0) * sorted.len() as f64).ceil() as usize).max(1);
    sorted.get(rank - 1).copied().unwrap_or(0.0)
}

/// The median, by nearest rank.
pub(crate) fn median(samples: &[f64]) -> f64 {
    percentile(samples, 50.0)
}

/// The current time as microseconds since the Unix epoch.
fn unix_now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_parses_known_keys() {
        let report = RunReport::parse("scenario start\nfirst_frame_unix_us 42\nunknown_key 7\n");
        assert_eq!(report.scenario, "start");
        assert_eq!(report.first_frame_unix_us, Some(42));
        assert_eq!(report.nav_to_load_us, None);
        assert_eq!(report.scroll_frames_us, Vec::<u32>::new());
    }

    #[test]
    fn report_parses_all_scenario_fields() {
        let report = RunReport::parse(
            "scenario memory\nnav_to_load_us 100\nscript_us 40\n\
             scroll_frames_us 12,34\nfootprint_browser_bytes 1\n\
             footprint_content_bytes 2\n",
        );
        assert_eq!(report.scenario, "memory");
        assert_eq!(report.nav_to_load_us, Some(100));
        assert_eq!(report.script_us, Some(40));
        assert_eq!(report.scroll_frames_us, vec![12, 34]);
        assert_eq!(report.footprint_browser_bytes, Some(1));
        assert_eq!(report.footprint_content_bytes, Some(2));
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let report = RunReport::parse("\nno-separator\nscenario load\nbad 12x\n");
        assert_eq!(report.scenario, "load");
        assert_eq!(report.nav_to_load_us, None);
    }

    #[test]
    fn percentiles_are_nearest_rank() {
        let samples = vec![10.0, 20.0, 30.0, 40.0];
        // p50 of 4 samples: rank ceil(0.5 * 4) = 2nd of the sorted list.
        assert_eq!(median(&samples), 20.0);
        // p95 of 4: rank ceil(0.95 * 4) = 4th.
        assert_eq!(percentile(&samples, 95.0), 40.0);
        // The input order never matters.
        let shuffled = vec![40.0, 10.0, 30.0, 20.0];
        assert_eq!(median(&shuffled), 20.0);
        // Empty input stays panic free.
        assert_eq!(median(&[]), 0.0);
    }
}
