//! The `--memory-report` breakdown (spec 0032, AC-10).
//!
//! With the flag, the browser asks both processes for their counters two
//! seconds after the page's `load`, then writes one JSON file. Without it,
//! no counters are gathered and nothing is written. The format is written by
//! hand so the shipped binary needs no new dependency (bundle budget,
//! AGENTS.md).

use std::path::{Path, PathBuf};

use ipc::MemoryCategory;
use render::MemoryCounters;
use tracing::{info, warn};

/// The launch flag that turns the report on.
pub(crate) const MEMORY_REPORT_FLAG: &str = "--memory-report";

/// How long after `load` the report is taken (spec 0032, AC-10).
pub(crate) const REPORT_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

/// The report path from the command line, if the flag is present and has a
/// value.
pub(crate) fn path_arg(args: &[String]) -> Option<PathBuf> {
    let at = args.iter().position(|arg| arg == MEMORY_REPORT_FLAG)?;
    args.get(at + 1).map(PathBuf::from)
}

/// The report's live state: it waits until both processes answered.
#[derive(Debug)]
pub(crate) struct State {
    path: PathBuf,
    pending: Option<Pending>,
}

#[derive(Debug, Default)]
struct Pending {
    browser: Option<MemoryCounters>,
    content: Option<Vec<(MemoryCategory, u64)>>,
}

impl State {
    /// A report waiting for its two answers.
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            pending: Some(Pending::default()),
        }
    }

    /// The browser side's counters arrived.
    pub(crate) fn browser(&mut self, counters: MemoryCounters) {
        if let Some(pending) = &mut self.pending {
            pending.browser = Some(counters);
        }
    }

    /// The content process's counters arrived.
    pub(crate) fn content(&mut self, counters: Vec<(MemoryCategory, u64)>) {
        if let Some(pending) = &mut self.pending {
            pending.content = Some(counters);
        }
    }

    /// Writes the report once both sides answered. Returns whether it wrote.
    pub(crate) fn write_if_ready(&mut self) -> bool {
        let Some(pending) = &mut self.pending else {
            return false;
        };
        let Some(browser) = pending.browser else {
            return false;
        };
        let Some(content) = pending.content.take() else {
            return false;
        };
        let footprint = crate::perf::footprint_bytes();
        match write(&self.path, browser, footprint, &content) {
            Ok(()) => info!(path = %self.path.display(), "memory report written"),
            Err(err) => warn!(
                path = %self.path.display(),
                %err,
                "memory report could not be written"
            ),
        }
        self.pending = None;
        true
    }
}

/// Writes the JSON breakdown: the browser's categories and the content
/// process's (spec 0032, Memory report).
pub(crate) fn write(
    path: &Path,
    browser: MemoryCounters,
    footprint: Option<u64>,
    content: &[(MemoryCategory, u64)],
) -> std::io::Result<()> {
    let mut out = String::from("{\n");
    out.push_str("  \"browser\": {\n");
    let mut first = true;
    field(&mut out, &mut first, "footprint", footprint);
    field(
        &mut out,
        &mut first,
        "font_mapped",
        Some(browser.fonts_mapped),
    );
    field(&mut out, &mut first, "font_heap", Some(browser.fonts_heap));
    field(&mut out, &mut first, "images_gpu", Some(browser.images_gpu));
    field(&mut out, &mut first, "atlas_gpu", Some(browser.atlas_gpu));
    field(&mut out, &mut first, "layers_gpu", Some(browser.layers_gpu));
    field(
        &mut out,
        &mut first,
        "surface_gpu",
        Some(browser.surface_gpu),
    );
    out.push_str("\n  },\n  \"content\": {\n");
    first = true;
    for category in [
        MemoryCategory::Footprint,
        MemoryCategory::JsHeap,
        MemoryCategory::DomNodes,
    ] {
        let value = content
            .iter()
            .find(|(found, _)| *found == category)
            .map(|(_, value)| *value);
        field(&mut out, &mut first, category.as_str(), value);
    }
    out.push_str("\n  }\n}\n");
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("memory.tmp");
    std::fs::write(&temp, out)?;
    std::fs::rename(&temp, path)
}

/// Appends one JSON field, comma separated from the ones before it.
fn field(out: &mut String, first: &mut bool, name: &str, value: Option<u64>) {
    if !*first {
        out.push_str(",\n");
    }
    *first = false;
    out.push_str("    \"");
    out.push_str(name);
    out.push_str("\": ");
    match value {
        Some(value) => out.push_str(&value.to_string()),
        None => out.push_str("null"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_takes_the_path_after_it() {
        let args = vec!["--memory-report".to_owned(), "/tmp/report.json".to_owned()];
        assert_eq!(path_arg(&args), Some(PathBuf::from("/tmp/report.json")));
        assert_eq!(path_arg(&["about:start".to_owned()]), None);
    }

    #[test]
    fn the_report_holds_every_category_for_both_processes() {
        let path = std::env::temp_dir()
            .join("embed-memory-report-test")
            .join("report.json");
        let browser = MemoryCounters {
            fonts_mapped: 11,
            fonts_heap: 12,
            images_gpu: 13,
            atlas_gpu: 14,
            layers_gpu: 15,
            surface_gpu: 16,
        };
        let content = vec![
            (MemoryCategory::Footprint, 21),
            (MemoryCategory::JsHeap, 22),
            (MemoryCategory::DomNodes, 23),
        ];
        write(&path, browser, Some(20), &content).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        for name in [
            "footprint",
            "font_mapped",
            "font_heap",
            "images_gpu",
            "atlas_gpu",
            "layers_gpu",
            "surface_gpu",
            "js_heap",
            "dom_nodes",
        ] {
            assert!(text.contains(&format!("\"{name}\"")), "{name} missing");
        }
        assert!(text.contains("\"footprint\": 20"));
        assert!(text.contains("\"dom_nodes\": 23"));
    }

    #[test]
    fn a_report_without_a_footprint_reader_writes_null() {
        let path = std::env::temp_dir()
            .join("embed-memory-report-test")
            .join("null.json");
        write(&path, MemoryCounters::default(), None, &[]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"footprint\": null"));
    }
}
