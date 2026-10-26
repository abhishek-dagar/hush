//! Perf mode: the scripted scenarios the budget gate drives (spec 0007).
//!
//! The browser parses `--perf <scenario>` and `--perf-report <file>` here,
//! records marks while a scenario runs, and writes a one run report to the
//! path the gate chose. Without `--perf` none of this runs: no report, no
//! marks, no changed behaviour (AC-11). The report is a small line based
//! text format, one `key value` pair per line, so the shipped binary needs
//! no new dependencies (bundle budget, AGENTS.md); `tools/perf` parses it.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracing::info;

/// The window size perf mode opens at, in logical px (spec 0007, Scenarios):
/// a fixed, known viewport so numbers from different runs compare.
pub(crate) const PERF_WINDOW_SIZE: (f64, f64) = (1280.0, 800.0);

/// How long after `load` the memory scenario waits before it samples the
/// footprints (spec 0007, AC-5).
pub(crate) const MEMORY_SAMPLE_DELAY: Duration = Duration::from_secs(2);

/// How long after `load` the scroll scenario waits before it feeds its
/// first wheel step (spec 0007, AC-6).
pub(crate) const SCROLL_START_DELAY: Duration = Duration::from_millis(500);

/// Which scripted scenario perf mode runs (spec 0007, Scenarios).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scenario {
    /// Cold start: from spawn to the first frame with `about:start` content.
    Start,
    /// Memory: footprint of both processes, 2 s after `load`.
    Memory,
    /// Scroll: frame CPU time over 240 wheel fed scroll steps.
    Scroll,
    /// Load: navigation to `load` on the script workload page.
    Load,
    /// Browse: the article, the start page and back, sampled 2 s after the
    /// last `load` (spec 0032, AC-11).
    Browse,
}

impl Scenario {
    /// Parses a scenario name, as the gate spells it.
    pub(crate) fn parse(name: &str) -> Option<Self> {
        match name {
            "start" => Some(Self::Start),
            "memory" => Some(Self::Memory),
            "scroll" => Some(Self::Scroll),
            "load" => Some(Self::Load),
            "browse" => Some(Self::Browse),
            _ => None,
        }
    }

    /// The name reports and the report file use.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Memory => "memory",
            Self::Scroll => "scroll",
            Self::Load => "load",
            Self::Browse => "browse",
        }
    }
}

/// The browser process's parsed perf flags (spec 0007, Scenarios).
#[derive(Debug, Clone)]
pub(crate) struct PerfArgs {
    /// Which scenario to run.
    pub(crate) scenario: Scenario,
    /// Where the one run report is written.
    pub(crate) report: PathBuf,
}

/// Parses the browser's perf flags out of the raw command line. `None` is
/// perf off: the browser behaves exactly as today (AC-11). An unknown
/// scenario, or `--perf` without `--perf-report`, is an error before any
/// window opens (AC-11).
pub(crate) fn browser_args(args: &[String]) -> Result<Option<PerfArgs>, crate::EmbedError> {
    let mut scenario = None;
    let mut report = None;
    let mut names = args.iter();
    while let Some(arg) = names.next() {
        match arg.as_str() {
            "--perf" => {
                let value = names.next().ok_or(crate::EmbedError::PerfMissingReport)?;
                scenario = Some(
                    Scenario::parse(value)
                        .ok_or_else(|| crate::EmbedError::PerfUnknownScenario(value.clone()))?,
                );
            }
            "--perf-report" => {
                let value = names.next().ok_or(crate::EmbedError::PerfMissingReport)?;
                report = Some(PathBuf::from(value));
            }
            _ => {}
        }
    }
    match (scenario, report) {
        (None, _) => Ok(None),
        (Some(scenario), Some(report)) => Ok(Some(PerfArgs { scenario, report })),
        // `--perf` without a report path cannot record anything (AC-11).
        (Some(_), None) => Err(crate::EmbedError::PerfMissingReport),
    }
}

/// Whether the content process was launched with `--perf` (spec 0007, IPC
/// messages): it learns perf mode only from its launch argument.
pub(crate) fn content_args(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--perf")
}

/// The current time as microseconds since the Unix epoch, for marks that
/// outlive the process (the gate subtracts its own spawn stamp).
pub(crate) fn unix_now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

/// The marks the render thread records during a scenario. The main thread
/// merges them into the run report (spec 0007, Run report).
#[derive(Debug, Default)]
pub(crate) struct RenderMarks {
    /// When the first frame with page content was presented, Unix µs
    /// (scenario `start`, AC-4).
    pub(crate) first_frame_unix_us: Option<u64>,
    /// Each scroll frame's CPU time, µs, in feed order (scenario `scroll`,
    /// AC-6).
    pub(crate) scroll_frames_us: Vec<u32>,
}

/// The one run report the browser writes to `--perf-report` (spec 0007,
/// Run report). Fields a scenario does not produce stay empty and are left
/// out of the file.
#[derive(Debug, Default)]
pub(crate) struct RunReport {
    /// Which scenario ran.
    pub(crate) scenario: &'static str,
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
    /// Serialises to the line format: one `key value` pair per line. Empty
    /// fields are omitted; the gate treats a missing key as "not produced".
    pub(crate) fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str("scenario ");
        out.push_str(self.scenario);
        out.push('\n');
        let mut pair = |key: &str, value: String| {
            out.push_str(key);
            out.push(' ');
            out.push_str(&value);
            out.push('\n');
        };
        if let Some(us) = self.first_frame_unix_us {
            pair("first_frame_unix_us", us.to_string());
        }
        if let Some(us) = self.nav_to_load_us {
            pair("nav_to_load_us", us.to_string());
        }
        if let Some(us) = self.script_us {
            pair("script_us", us.to_string());
        }
        if !self.scroll_frames_us.is_empty() {
            let frames = self
                .scroll_frames_us
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            pair("scroll_frames_us", frames);
        }
        if let Some(bytes) = self.footprint_browser_bytes {
            pair("footprint_browser_bytes", bytes.to_string());
        }
        if let Some(bytes) = self.footprint_content_bytes {
            pair("footprint_content_bytes", bytes.to_string());
        }
        out
    }

    /// Writes the report to a temp file then renames it, so the gate never
    /// reads a partial file (spec 0007, Run report).
    pub(crate) fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("perf.tmp");
        std::fs::write(&temp, self.to_text())?;
        std::fs::rename(&temp, path)
    }
}

/// The browser's live perf state: what it runs and since when (the exit
/// log line, spec 0007, Observability).
pub(crate) struct PerfState {
    /// The parsed `--perf` flags.
    pub(crate) args: PerfArgs,
    /// When the browser started, for the run duration in the exit log.
    pub(crate) started: std::time::Instant,
}

/// The scroll driver (spec 0007, AC-6): feeds 120 wheel steps of 100 CSS px
/// down, then 120 up, one step per frame, through the same path real wheel
/// input takes. The step size and count are constants of this spec, not
/// tunables.
pub(crate) struct ScrollDriver {
    /// Wheel steps left in the downward run.
    down_left: u32,
    /// Wheel steps left in the upward run.
    up_left: u32,
}

/// One wheel step's length, in CSS px (spec 0007, AC-6).
const SCROLL_STEP_PX: f32 = 100.0;

/// Steps down and then steps up (spec 0007, AC-6).
const SCROLL_STEPS: u32 = 120;

impl ScrollDriver {
    /// A fresh driver, ready for its first downward step.
    pub(crate) fn new() -> Self {
        Self {
            down_left: SCROLL_STEPS,
            up_left: SCROLL_STEPS,
        }
    }

    /// The next step's delta in CSS px, as a wheel event would carry it:
    /// offsets grow downward first, then shrink back (the browser flips the
    /// content direction of wheel deltas, so down is positive here).
    /// `None` when all 240 steps have run.
    pub(crate) fn next_step(&mut self) -> Option<(f32, f32)> {
        if self.down_left > 0 {
            self.down_left -= 1;
            return Some((0.0, SCROLL_STEP_PX));
        }
        if self.up_left > 0 {
            self.up_left -= 1;
            return Some((0.0, -SCROLL_STEP_PX));
        }
        None
    }

    /// How many steps are left, for tests and logs.
    #[cfg(test)]
    pub(crate) fn left(&self) -> u32 {
        self.down_left + self.up_left
    }

    /// Whether all 240 steps have been fed.
    pub(crate) fn is_done(&self) -> bool {
        self.down_left == 0 && self.up_left == 0
    }
}

/// The running total of wall time spent inside the script engine (spec
/// 0007, Script timing). Shared by every page of the content process and
/// read when the browser asks for marks. AC-11: only exists in perf mode.
#[derive(Default)]
pub(crate) struct ScriptTimer {
    total_us: std::sync::atomic::AtomicU64,
}

impl ScriptTimer {
    /// Wraps `engine` so every call's wall time adds up here. The page
    /// never reads a clock; the measuring lives in this module (AGENTS.md,
    /// Functional core).
    pub(crate) fn wrap(
        self: &std::sync::Arc<Self>,
        engine: Box<dyn page::ScriptEngine>,
    ) -> Box<dyn page::ScriptEngine> {
        Box::new(TimedEngine {
            engine,
            timer: std::sync::Arc::clone(self),
        })
    }

    /// The total wall time inside the engine so far, µs.
    pub(crate) fn total_us(&self) -> u64 {
        self.total_us.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Adds the time since `started` to the total.
    fn add(&self, started: std::time::Instant) {
        self.total_us.fetch_add(
            started.elapsed().as_micros() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
}

/// Adds up wall time across every [`page::ScriptEngine`] call (spec 0007,
/// Script timing). Perf mode only: without `--perf` no engine is wrapped
/// (AC-11).
struct TimedEngine {
    engine: Box<dyn page::ScriptEngine>,
    timer: std::sync::Arc<ScriptTimer>,
}

impl page::ScriptEngine for TimedEngine {
    fn eval_classic(&mut self, source: &str, url: &str) -> Result<(), page::ScriptError> {
        let started = std::time::Instant::now();
        let result = self.engine.eval_classic(source, url);
        self.timer.add(started);
        result
    }

    fn fire_event(
        &mut self,
        target: page::EventTarget,
        event: &str,
    ) -> Result<(), page::ScriptError> {
        let started = std::time::Instant::now();
        let result = self.engine.fire_event(target, event);
        self.timer.add(started);
        result
    }

    fn run_timer(&mut self, id: page::TimerId) -> Result<(), page::ScriptError> {
        let started = std::time::Instant::now();
        let result = self.engine.run_timer(id);
        self.timer.add(started);
        result
    }

    fn run_microtasks(&mut self) -> Result<(), page::ScriptError> {
        let started = std::time::Instant::now();
        let result = self.engine.run_microtasks();
        self.timer.add(started);
        result
    }

    fn sweep_wrappers(&mut self) {
        let started = std::time::Instant::now();
        self.engine.sweep_wrappers();
        self.timer.add(started);
    }

    fn collect_garbage(&mut self) {
        let started = std::time::Instant::now();
        self.engine.collect_garbage();
        self.timer.add(started);
    }

    fn heap_bytes(&self) -> Option<u64> {
        self.engine.heap_bytes()
    }
}

/// The physical footprint of this process in bytes (AC-5): each process
/// reads its own, at sample time. `None` on OSes without a reader (spec
/// 0007, Value sourcing). The shell build forbids `unsafe` outright and
/// never runs the memory scenario, so the reader is chrome build only; a
/// chrome free perf run reports no footprint and its memory rows become
/// `not_measured`, honestly.
#[cfg(all(target_os = "macos", feature = "chrome"))]
pub(crate) fn footprint_bytes() -> Option<u64> {
    #[allow(
        unsafe_code,
        reason = "proc_pid_rusage is the documented macOS way to read the \
                  physical footprint; the kernel fills a plain stack value \
                  (spec 0007, Value sourcing)"
    )]
    unsafe {
        // SAFETY: `info` is a valid, zeroed `rusage_info_v2` on the stack,
        // and `proc_pid_rusage` only fills it for this process's pid.
        let mut info: libc::rusage_info_v2 = std::mem::zeroed();
        // RUSAGE_INFO_V2 is flavor 2 (<sys/resource.h>); the C API takes
        // the address of the struct as a `rusage_info_t *`.
        let ok = libc::proc_pid_rusage(
            libc::getpid(),
            2,
            std::ptr::from_mut(&mut info).cast::<libc::rusage_info_t>(),
        );
        (ok == 0).then_some(info.ri_phys_footprint)
    }
}

/// No reader on this OS yet: memory rows become `not_measured` (spec 0007,
/// Value sourcing; Linux and Windows rows arrive with their CI runners).
#[cfg(not(all(target_os = "macos", feature = "chrome")))]
pub(crate) fn footprint_bytes() -> Option<u64> {
    None
}

/// Logs the one start line perf mode prints (spec 0007, Observability).
pub(crate) fn log_start(args: &PerfArgs) {
    info!(
        scenario = args.scenario.as_str(),
        report = %args.report.display(),
        "perf mode on"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_names_round_trip() {
        for name in ["start", "memory", "scroll", "load", "browse"] {
            let parsed = Scenario::parse(name).expect("known scenario");
            assert_eq!(parsed.as_str(), name);
        }
        assert_eq!(Scenario::parse("nope"), None);
    }

    #[test]
    fn no_perf_flags_means_perf_off() {
        let args = vec!["some-url".to_owned()];
        assert!(browser_args(&args).unwrap().is_none());
    }

    #[test]
    fn both_flags_parse() {
        let args = vec![
            "--perf-report".to_owned(),
            "/tmp/r.json".to_owned(),
            "https://example.test".to_owned(),
            "--perf".to_owned(),
            "start".to_owned(),
        ];
        let parsed = browser_args(&args).unwrap().expect("perf on");
        assert_eq!(parsed.scenario, Scenario::Start);
        assert_eq!(parsed.report, PathBuf::from("/tmp/r.json"));
    }

    #[test]
    fn unknown_scenario_is_an_error() {
        let args = vec![
            "--perf".to_owned(),
            "fly".to_owned(),
            "--perf-report".to_owned(),
            "/tmp/r".to_owned(),
        ];
        assert!(browser_args(&args).is_err());
    }

    #[test]
    fn perf_without_report_path_is_an_error() {
        let args = vec!["--perf".to_owned(), "start".to_owned()];
        assert!(browser_args(&args).is_err());
        let args = vec![
            "--perf".to_owned(),
            "start".to_owned(),
            "--perf-report".to_owned(),
        ];
        assert!(browser_args(&args).is_err());
    }

    #[test]
    fn a_stray_report_path_alone_stays_perf_off() {
        let args = vec!["--perf-report".to_owned(), "/tmp/r".to_owned()];
        assert!(browser_args(&args).unwrap().is_none());
    }

    #[test]
    fn content_flag_detected() {
        assert!(content_args(&["--perf".to_owned()]));
        assert!(!content_args(&["other".to_owned()]));
    }

    #[test]
    fn scroll_driver_runs_down_then_up() {
        let mut driver = ScrollDriver::new();
        assert_eq!(driver.left(), 240);
        // Every downward step grows the offset by one step of 100 CSS px.
        for _ in 0..120 {
            assert_eq!(driver.next_step(), Some((0.0, 100.0)));
        }
        // Then every upward step shrinks it again.
        for _ in 0..120 {
            assert_eq!(driver.next_step(), Some((0.0, -100.0)));
        }
        // And then it is done.
        assert_eq!(driver.next_step(), None);
        assert_eq!(driver.left(), 0);
    }

    #[test]
    fn report_text_holds_produced_fields_only() {
        let report = RunReport {
            scenario: "start",
            first_frame_unix_us: Some(42),
            ..RunReport::default()
        };
        let text = report.to_text();
        assert!(text.contains("scenario start\n"));
        assert!(text.contains("first_frame_unix_us 42\n"));
        assert!(!text.contains("nav_to_load_us"));
        assert!(!text.contains("scroll_frames_us"));
    }

    #[test]
    fn scroll_frames_are_a_comma_list() {
        let report = RunReport {
            scenario: "scroll",
            scroll_frames_us: vec![1, 2, 3],
            ..RunReport::default()
        };
        assert!(report.to_text().contains("scroll_frames_us 1,2,3\n"));
    }

    #[test]
    fn report_writes_atomically_and_completely() {
        let dir = std::env::temp_dir().join("embed-perf-test");
        let path = dir.join("run.json");
        let report = RunReport {
            scenario: "memory",
            nav_to_load_us: Some(7),
            footprint_browser_bytes: Some(1),
            footprint_content_bytes: Some(2),
            ..RunReport::default()
        };
        report.write(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("scenario memory\n"));
        assert!(text.contains("nav_to_load_us 7\n"));
        assert!(text.contains("footprint_browser_bytes 1\n"));
        assert!(text.contains("footprint_content_bytes 2\n"));
        // The temp file is gone after the rename.
        assert!(!path.with_extension("perf.tmp").exists());
        std::fs::remove_file(&path).unwrap();
    }
}
