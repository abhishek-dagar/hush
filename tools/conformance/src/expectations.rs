//! Expectations files: known non passing tests (spec 0005, AC-10, AC-11).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::status::Status;

/// Parsed expectations for one area.
#[derive(Debug, Clone)]
pub struct Expectations {
    /// Area name.
    #[allow(dead_code, reason = "carries area identity for debugging")]
    pub area: String,
    /// path -> expected statuses.
    map: BTreeMap<String, Vec<Status>>,
}

impl Expectations {
    /// Loads `expectations/<area>.txt` from `dir`. A missing file means
    /// every test expects PASS.
    pub fn load(dir: &Path, area: &str) -> Result<Self> {
        let path = dir.join(format!("{area}.txt"));
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).context(format!("could not read {}", path.display())),
        };
        Self::parse(area, &text)
    }

    /// Parses text.
    pub fn parse(area: &str, text: &str) -> Result<Self> {
        let mut map = BTreeMap::new();
        for (lineno, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let path = parts.next().context("empty expectation line")?.to_owned();
            let statuses: Vec<Status> = parts
                .map(|token| {
                    Status::parse(token)
                        .ok_or_else(|| anyhow::anyhow!("unknown status `{token}` on line {}", lineno + 1))
                })
                .collect::<Result<Vec<_>>>()?;
            if statuses.is_empty() {
                bail!("no status on line {}: {line}", lineno + 1);
            }
            if map.insert(path.clone(), statuses).is_some() {
                bail!("duplicate expectation for `{path}`");
            }
        }
        Ok(Self {
            area: area.to_owned(),
            map,
        })
    }

    /// Expected statuses for `path`; unlisted means PASS.
    pub fn expected(&self, path: &str) -> Vec<Status> {
        self.map
            .get(path)
            .cloned()
            .unwrap_or_else(|| vec![Status::Pass])
    }

    /// Whether `path` is listed.
    pub fn contains(&self, path: &str) -> bool {
        self.map.contains_key(path)
    }

    /// All listed paths.
    #[allow(dead_code, reason = "may be used for future stale detection")]
    pub fn paths(&self) -> Vec<String> {
        self.map.keys().cloned().collect()
    }

    /// Regressions: paths whose actual status is not in the expected set.
    pub fn regressions<'a>(
        &self,
        results: &'a [(String, Status)],
    ) -> Vec<&'a str> {
        results
            .iter()
            .filter(|(path, status)| !self.expected(path).contains(status))
            .map(|(path, _)| path.as_str())
            .collect()
    }

    /// Unexpected passes: listed tests that passed but do not expect PASS.
    pub fn unexpected_passes<'a>(
        &self,
        results: &'a [(String, Status)],
    ) -> Vec<&'a str> {
        results
            .iter()
            .filter(|(path, status)| {
                *status == Status::Pass
                    && self.contains(path)
                    && !self.expected(path).contains(&Status::Pass)
            })
            .map(|(path, _)| path.as_str())
            .collect()
    }

    /// Stale lines: listed paths not among `discovered`.
    pub fn stale(&self, discovered: &[String]) -> Vec<String> {
        let set: std::collections::BTreeSet<&str> = discovered.iter().map(String::as_str).collect();
        self.map
            .keys()
            .filter(|p| !set.contains(p.as_str()))
            .cloned()
            .collect()
    }

    /// Rewrites expectations from results. `ran` is the list of (path, status)
    /// that ran. `discovered` is all discovered paths in the area. `whole_area`
    /// means every test in the area ran (so stale lines can be dropped).
    /// `filter_paths`/`filter_areas` narrowing is handled by the caller passing
    /// only the ran tests; unrun tests keep their lines.
    pub fn update(&mut self, ran: &[(String, Status)], discovered: &[String], whole_area: bool) {
        if whole_area {
            let discovered_set: std::collections::BTreeSet<&str> =
                discovered.iter().map(String::as_str).collect();
            self.map
                .retain(|path, _| discovered_set.contains(path.as_str()));
        }
        for (path, status) in ran {
            if *status == Status::Pass {
                self.map.remove(path);
                continue;
            }
            // flaky lines (more than one status) are kept as they are
            if matches!(self.map.get(path), Some(e) if e.len() > 1) {
                continue;
            }
            self.map.insert(path.clone(), vec![*status]);
        }
    }

    /// Writes the file to `path`.
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut lines: Vec<String> = self
            .map
            .iter()
            .map(|(path, statuses)| {
                format!(
                    "{} {}",
                    path,
                    statuses
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            })
            .collect();
        lines.sort();
        let body = lines.join("\n");
        let body = if body.is_empty() {
            String::new()
        } else {
            body + "\n"
        };
        std::fs::write(path, body)
            .with_context(|| format!("could not write {}", path.display()))?;
        Ok(())
    }
}