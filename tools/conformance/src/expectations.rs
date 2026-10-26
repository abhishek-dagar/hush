//! Expectations files: known non passing tests (spec 0005, AC-10, AC-11).
//!
//! Every non PASS line carries a reason, written as a trailing comment: either
//! `# needs: <feature or spec>` for a failure the harness cannot reach yet, or
//! `# bug: <one line>` for a failure this engine must fix (spec 0009, AC-21).

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::status::Status;

/// Why a test is expected not to pass (spec 0009, AC-21).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// Out of reach: a feature or spec the harness does not support yet.
    Needs(String),
    /// A failure this engine must fix.
    Bug(String),
}

impl Reason {
    /// The text as it appears after `#` in an expectations file.
    fn text(&self) -> String {
        match self {
            Self::Needs(text) => format!("needs: {text}"),
            Self::Bug(text) => format!("bug: {text}"),
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text())
    }
}

/// Parses the reason on an expectation line, after the `#`.
fn parse_reason(text: &str, lineno: usize) -> Result<Reason> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix("needs:") {
        let rest = rest.trim();
        if rest.is_empty() {
            bail!("empty `# needs:` reason on line {lineno}");
        }
        return Ok(Reason::Needs(rest.to_owned()));
    }
    if let Some(rest) = text.strip_prefix("bug:") {
        let rest = rest.trim();
        if rest.is_empty() {
            bail!("empty `# bug:` reason on line {lineno}");
        }
        return Ok(Reason::Bug(rest.to_owned()));
    }
    bail!("reason on line {lineno} must start with `# needs:` or `# bug:`: {text}")
}

/// Parsed expectations for one area.
#[derive(Debug, Clone)]
pub struct Expectations {
    /// Area name.
    #[allow(dead_code, reason = "carries area identity for debugging")]
    pub area: String,
    /// path -> expected statuses.
    map: BTreeMap<String, Vec<Status>>,
    /// path -> why it does not pass.
    reasons: BTreeMap<String, Reason>,
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
        let mut reasons = BTreeMap::new();
        for (lineno, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (body, comment) = match line.split_once('#') {
                Some((body, comment)) => (body.trim_end(), Some(comment)),
                None => (line, None),
            };
            let mut parts = body.split_whitespace();
            let path = parts.next().context("empty expectation line")?.to_owned();
            let statuses: Vec<Status> = parts
                .map(|token| {
                    Status::parse(token).ok_or_else(|| {
                        anyhow::anyhow!("unknown status `{token}` on line {}", lineno + 1)
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            if statuses.is_empty() {
                bail!("no status on line {}: {body}", lineno + 1);
            }
            let needs_reason = statuses.iter().any(|status| *status != Status::Pass);
            let reason = match comment {
                Some(comment) => Some(parse_reason(comment, lineno + 1)?),
                None => None,
            };
            if needs_reason && reason.is_none() {
                bail!(
                    "expectation `{path}` on line {} does not pass, so it needs a `# needs:` or `# bug:` reason",
                    lineno + 1
                );
            }
            if let Some(reason) = reason {
                reasons.insert(path.clone(), reason);
            }
            if map.insert(path.clone(), statuses).is_some() {
                bail!("duplicate expectation for `{path}`");
            }
        }
        Ok(Self {
            area: area.to_owned(),
            map,
            reasons,
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
    pub fn regressions<'a>(&self, results: &'a [(String, Status)]) -> Vec<&'a str> {
        results
            .iter()
            .filter(|(path, status)| !self.expected(path).contains(status))
            .map(|(path, _)| path.as_str())
            .collect()
    }

    /// Unexpected passes: listed tests that passed but do not expect PASS.
    pub fn unexpected_passes<'a>(&self, results: &'a [(String, Status)]) -> Vec<&'a str> {
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
            self.reasons
                .retain(|path, _| discovered_set.contains(path.as_str()));
        }
        for (path, status) in ran {
            if *status == Status::Pass {
                self.map.remove(path);
                self.reasons.remove(path);
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
                let mut line = format!(
                    "{} {}",
                    path,
                    statuses
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                if let Some(reason) = self.reasons.get(path) {
                    line.push_str(&format!(" # {reason}"));
                }
                line
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_pass_line_needs_a_reason() {
        // AC-21: an expectation line with no `# needs:` or `# bug:` is a
        // setup error.
        let error = Expectations::parse("tables", "a/b.xht FAIL\n").unwrap_err();
        assert!(
            error.to_string().contains("needs a `# needs:` or `# bug:`"),
            "{error}"
        );
    }

    #[test]
    fn a_pass_line_needs_no_reason() {
        let expectations = Expectations::parse("tables", "a/b.xht PASS\n").unwrap();
        assert!(expectations.expected("a/b.xht").contains(&Status::Pass));
    }

    #[test]
    fn a_reason_must_name_needs_or_bug() {
        let error = Expectations::parse("tables", "a/b.xht FAIL # later\n").unwrap_err();
        assert!(error.to_string().contains("must start with"), "{error}");
    }

    #[test]
    fn needs_and_bug_reasons_round_trip() {
        let text = "\
a/b.xht FAIL # needs: writing modes
c/d.xht SKIP # bug: grid cap drops a row
";
        let expectations = Expectations::parse("tables", text).unwrap();
        assert_eq!(
            expectations.reasons.get("a/b.xht"),
            Some(&Reason::Needs("writing modes".to_owned()))
        );
        assert_eq!(
            expectations.reasons.get("c/d.xht"),
            Some(&Reason::Bug("grid cap drops a row".to_owned()))
        );

        let dir = std::env::temp_dir().join("conformance-expectations-round-trip");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tables.txt");
        expectations.save(&path).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, text);

        let again = Expectations::parse("tables", &written).unwrap();
        assert_eq!(again.reasons, expectations.reasons);
    }

    #[test]
    fn update_keeps_the_reason_of_a_line_that_stays_failing() {
        let mut expectations =
            Expectations::parse("tables", "a/b.xht FAIL # needs: writing modes\n").unwrap();
        expectations.update(
            &[("a/b.xht".to_owned(), Status::Skip)],
            &["a/b.xht".to_owned()],
            true,
        );
        assert_eq!(expectations.expected("a/b.xht"), vec![Status::Skip]);
        assert_eq!(
            expectations.reasons.get("a/b.xht"),
            Some(&Reason::Needs("writing modes".to_owned()))
        );
    }

    #[test]
    fn update_drops_the_reason_of_a_line_that_now_passes() {
        let mut expectations =
            Expectations::parse("tables", "a/b.xht FAIL # needs: writing modes\n").unwrap();
        expectations.update(
            &[("a/b.xht".to_owned(), Status::Pass)],
            &["a/b.xht".to_owned()],
            true,
        );
        assert!(expectations.reasons.is_empty());
    }
}
