//! `subset.toml`: the pinned WPT commit and the areas a run covers
//! (spec 0005, Data model sketch).

use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

/// Folders every fetch takes beside the areas: Ahem, the shared scripts,
/// and the support and reference files tests link to. `css/CSS2/support`
/// and `css/CSS2/reference` hold the references of about 630 CSS2 tests.
pub const SHARED_PATHS: &[&str] = &[
    "fonts",
    "resources",
    "css/support",
    "css/reference",
    "css/CSS2/support",
    "css/CSS2/reference",
];

/// The whole file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subset {
    /// The WPT commit, 40 lowercase hex characters.
    pub wpt_commit: String,
    /// Path prefixes from the WPT root, removed before classification.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// The areas, in file order.
    #[serde(rename = "area")]
    pub areas: Vec<Area>,
}

/// One area: a name, the folders it covers, and an optional pass-rate floor.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Area {
    /// Unique, `[a-z0-9-]+`; also the expectations file's name.
    pub name: String,
    /// Folders from the WPT root.
    pub paths: Vec<String>,
    /// The pass-rate floor, in percent. A run whose pass rate for this area
    /// is under the floor exits 1 and names the area (spec 0009, AC-21).
    /// Absent until the first full run measures it; only ever raised after.
    #[serde(default)]
    pub min_pass_rate: Option<f32>,
}

impl Subset {
    /// Reads and checks the file at `path` (AC-15).
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("could not read {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("{} is not a valid subset", path.display()))
    }

    /// Parses and checks `text`, trimming the slashes around each path.
    pub fn parse(text: &str) -> Result<Self> {
        let mut subset: Self = toml::from_str(text)?;
        let commit = &subset.wpt_commit;
        ensure!(
            commit.len() == 40
                && commit
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "wpt_commit `{commit}` is not 40 lowercase hex characters"
        );
        ensure!(!subset.areas.is_empty(), "no [[area]] is listed");
        for (i, area) in subset.areas.iter().enumerate() {
            let name = &area.name;
            ensure!(
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "area name `{name}` is not [a-z0-9-]+"
            );
            ensure!(
                subset.areas[..i].iter().all(|other| other.name != *name),
                "area `{name}` is listed twice"
            );
            ensure!(!area.paths.is_empty(), "area `{name}` has no paths");
            if let Some(rate) = area.min_pass_rate {
                ensure!(
                    (0.0..=100.0).contains(&rate),
                    "area `{name}` has min_pass_rate {rate} outside 0..=100"
                );
            }
        }
        for path in subset
            .exclude
            .iter_mut()
            .chain(subset.areas.iter_mut().flat_map(|a| a.paths.iter_mut()))
        {
            *path = path.trim_matches('/').to_owned();
            if path.is_empty()
                || path
                    .split('/')
                    .any(|s| s == ".." || s == "." || s.is_empty())
            {
                bail!("path `{path}` must be a folder below the WPT root");
            }
        }
        Ok(subset)
    }

    /// The area named `name`, if listed.
    pub fn area(&self, name: &str) -> Option<&Area> {
        self.areas.iter().find(|area| area.name == name)
    }

    /// The folders to check out: every area's paths, then the shared ones.
    pub fn sparse_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = Vec::new();
        let all = self
            .areas
            .iter()
            .flat_map(|a| a.paths.iter().map(String::as_str));
        for path in all.chain(SHARED_PATHS.iter().copied()) {
            if !paths.iter().any(|p| p == path) {
                paths.push(path.to_owned());
            }
        }
        paths
    }

    /// Every area, in file order, with a path that holds `path` (AC-3).
    /// A path can belong to multiple areas when their paths overlap, e.g.
    /// `css/CSS2/normal-flow` is covered by both `normal-flow` and `floats`.
    pub fn areas_of(&self, path: &str) -> Vec<&Area> {
        self.areas
            .iter()
            .filter(|area| area.paths.iter().any(|prefix| under(path, prefix)))
            .collect()
    }

    /// The first area, in file order, with a path that holds `path` (AC-3).
    pub fn area_of(&self, path: &str) -> Option<&Area> {
        self.areas_of(path).into_iter().next()
    }

    /// Whether an `exclude` prefix removes `path`.
    pub fn excluded(&self, path: &str) -> bool {
        self.exclude
            .iter()
            .any(|prefix| path.starts_with(prefix.as_str()))
    }
}

/// Whether `path` is `folder` or inside it, by whole segments.
pub fn under(path: &str, folder: &str) -> bool {
    path.strip_prefix(folder)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "46175634c0210e97365ef864020ff3be6358ea41";

    fn subset(body: &str) -> Result<Subset> {
        Subset::parse(&format!("wpt_commit = \"{COMMIT}\"\n{body}"))
    }

    #[test]
    fn a_test_belongs_to_the_first_area_that_holds_it() {
        let subset = subset(
            "[[area]]\nname = \"a\"\npaths = [\"css/x/deep/\"]\n\
             [[area]]\nname = \"b\"\npaths = [\"/css/x\"]\n",
        )
        .unwrap();
        assert_eq!(subset.area_of("css/x/deep/t.html").unwrap().name, "a");
        assert_eq!(subset.area_of("css/x/t.html").unwrap().name, "b");
        assert!(
            subset.area_of("css/xy/t.html").is_none(),
            "whole segments only"
        );
        assert_eq!(
            subset.sparse_paths()[..2],
            ["css/x/deep".to_owned(), "css/x".to_owned()]
        );
    }

    #[test]
    fn a_test_belongs_to_every_area_whose_paths_hold_it() {
        let subset = subset(
            "[[area]]\nname = \"a\"\npaths = [\"css/x/deep/\"]\n\
             [[area]]\nname = \"b\"\npaths = [\"/css/x\"]\n",
        )
        .unwrap();
        let areas: Vec<_> = subset.areas_of("css/x/deep/t.html");
        assert_eq!(areas.len(), 2);
        assert_eq!(areas[0].name, "a");
        assert_eq!(areas[1].name, "b");
        let areas: Vec<_> = subset.areas_of("css/x/t.html");
        assert_eq!(areas.len(), 1);
        assert_eq!(areas[0].name, "b");
        let areas: Vec<_> = subset.areas_of("css/xy/t.html");
        assert!(areas.is_empty(), "whole segments only");
    }

    #[test]
    fn bad_files_are_refused() {
        let area = "[[area]]\nname = \"a\"\npaths = [\"css\"]\n";
        assert!(Subset::parse(&format!("wpt_commit = \"ABC\"\n{area}")).is_err());
        assert!(subset("").is_err(), "no areas");
        assert!(
            subset(&format!("{area}{area}")).is_err(),
            "a duplicate name"
        );
        assert!(subset("[[area]]\nname = \"A b\"\npaths = [\"css\"]\n").is_err());
        assert!(subset("[[area]]\nname = \"a\"\npaths = []\n").is_err());
        assert!(subset("[[area]]\nname = \"a\"\npaths = [\"../etc\"]\n").is_err());
        assert!(
            subset(&format!("{area}color = 1\n")).is_err(),
            "unknown fields"
        );
    }

    #[test]
    fn min_pass_rate_parses_and_stays_in_range() {
        let area = "[[area]]\nname = \"a\"\npaths = [\"css\"]\n";
        let good = format!("{area}min_pass_rate = 70\n");
        let parsed = subset(&good).unwrap();
        assert_eq!(parsed.areas[0].min_pass_rate, Some(70.0));

        // Absent is fine.
        let absent = "[[area]]\nname = \"a\"\npaths = [\"css\"]\n";
        assert_eq!(subset(absent).unwrap().areas[0].min_pass_rate, None);

        // Out of range is refused.
        for rate in [-1.0, 100.5] {
            assert!(
                subset(&format!("{area}min_pass_rate = {rate}\n")).is_err(),
                "min_pass_rate {rate} should be out of range"
            );
        }
    }
}
