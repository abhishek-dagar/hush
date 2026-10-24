//! Finds the tests under the subset's areas and classifies each one by the
//! *Test discovery* table (spec 0005, AC-3). Links and metas come from the
//! engine's own parse of the file, so discovery and loading agree.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use base::atoms::{LocalName, QualName};
use dom::{Document, NodeId};
use html::{Parser, ParserOptions};
use page::DomSink;
use serde::Serialize;
use url::Url;

use crate::serve;
use crate::subset::Subset;

/// What a test file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Drawn and compared with its references.
    Reftest,
    /// Loaded and drawn; passes when nothing crashes or hangs.
    Crashtest,
    /// Counted, never run.
    Skip(SkipReason),
}

/// Why a test is skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// Needs WPT's Python server (`.sub.`, `.py`).
    Server,
    /// A manual test.
    Manual,
    /// A testharness.js test.
    Testharness,
    /// Neither a reftest nor a crashtest.
    OtherKind,
}

impl SkipReason {
    /// Every reason, in report order.
    pub const ALL: [Self; 4] = [
        Self::Server,
        Self::Manual,
        Self::Testharness,
        Self::OtherKind,
    ];

    /// The name the reports use.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Manual => "manual",
            Self::Testharness => "testharness",
            Self::OtherKind => "other_kind",
        }
    }
}

/// How long a test may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timeout {
    /// 10 s.
    Normal,
    /// 60 s, from `<meta name=timeout content=long>`.
    Long,
}

/// One discovered test file.
#[derive(Debug, Clone)]
pub struct Test {
    /// From the WPT root, `/` separators.
    pub path: String,
    /// The first area that holds it.
    pub area: String,
    /// Its kind.
    pub kind: Kind,
    /// Its timeout.
    pub timeout: Timeout,
    /// An `.xht` or `.xhtml` file, parsed as HTML (AC-18).
    pub xhtml: bool,
}

/// How a reference relates to the page that links it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    /// `rel=match`: must render the same.
    Match,
    /// `rel=mismatch`: must render differently.
    Mismatch,
}

/// What a page's markup says about it as a test.
#[derive(Debug, Default, Clone)]
pub struct Meta {
    /// `<link rel=match|mismatch href>`, resolved against the page URL.
    pub refs: Vec<(Relation, Url)>,
    /// Every `<meta name=fuzzy content>`, as written.
    pub fuzzy: Vec<String>,
    /// `<meta name=timeout content=long>`.
    pub long_timeout: bool,
    /// A `<script src>` ending in `/resources/testharness.js`.
    pub testharness: bool,
    /// Every linked URL (`href` of `<link>`, any element's `src`), resolved.
    pub links: Vec<Url>,
}

/// Whether `name` is a file a test can be: HTML or XHTML.
pub fn is_page(name: &str) -> bool {
    [".html", ".htm", ".xht", ".xhtml"]
        .iter()
        .any(|ext| name.ends_with(ext))
}

/// Parses `bytes` as the page at `url` and reads its metadata.
pub fn parse_meta(url: &str, bytes: &[u8]) -> Meta {
    let mut document = Document::new(url);
    let mut parser = Parser::new(ParserOptions::default());
    let mut sink = DomSink::new(&mut document);
    parser.feed(&mut sink, bytes);
    parser.end(&mut sink);
    read_meta(&document)
}

/// Reads the test metadata from `document`'s elements, in tree order.
pub fn read_meta(document: &Document) -> Meta {
    let base = Url::parse(document.url()).ok();
    let resolve = |href: &str| base.as_ref().and_then(|base| base.join(href.trim()).ok());
    let attribute = |name: &str| QualName::attribute(LocalName::from(name));
    let (rel, href, src) = (attribute("rel"), attribute("href"), attribute("src"));
    let (name, content) = (attribute("name"), attribute("content"));
    let mut meta = Meta::default();
    let mut stack: Vec<NodeId> = vec![document.root()];
    while let Some(id) = stack.pop() {
        let children: Vec<NodeId> = document.children(id).collect();
        stack.extend(children.into_iter().rev());
        let Some(element) = document.node(id).and_then(|node| node.as_element()) else {
            continue;
        };
        if let Some(url) = element.attribute(&src).and_then(resolve) {
            if url.path().ends_with("/resources/testharness.js") {
                meta.testharness = true;
            }
            meta.links.push(url);
        }
        match &*element.name().local {
            "link" => {
                let Some(url) = element.attribute(&href).and_then(resolve) else {
                    continue;
                };
                let rels = element.attribute(&rel).unwrap_or_default();
                for token in rels.split_ascii_whitespace() {
                    if token.eq_ignore_ascii_case("match") {
                        meta.refs.push((Relation::Match, url.clone()));
                    } else if token.eq_ignore_ascii_case("mismatch") {
                        meta.refs.push((Relation::Mismatch, url.clone()));
                    }
                }
                meta.links.push(url);
            }
            "meta" => {
                let value = element.attribute(&content).unwrap_or_default();
                match element
                    .attribute(&name)
                    .map(str::to_ascii_lowercase)
                    .as_deref()
                {
                    Some("fuzzy") => meta.fuzzy.push(value.to_owned()),
                    Some("timeout") if value.trim().eq_ignore_ascii_case("long") => {
                        meta.long_timeout = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    meta
}

/// Walks every area of `subset` under `root` and classifies each page
/// found, in path order. Two passes: the first parses every page, so the
/// second knows which pages another page uses as a reference (rule 2).
pub fn discover(root: &Path, subset: &Subset) -> Result<Vec<Test>> {
    let mut files = BTreeSet::new();
    for area in &subset.areas {
        for path in &area.paths {
            walk(root, path, &mut files)?;
        }
    }
    let files: Vec<String> = files.into_iter().filter(|p| !subset.excluded(p)).collect();
    let metas = parse_all(root, &files);
    let referenced: HashSet<String> = metas
        .iter()
        .flat_map(|meta| &meta.refs)
        .filter_map(|(_, url)| serve::path_for(url))
        .collect();
    let mut tests = Vec::new();
    for (path, meta) in files.iter().zip(&metas) {
        let Some(area) = subset.area_of(path) else {
            continue;
        };
        let Some(kind) = classify(root, path, meta, &referenced) else {
            continue;
        };
        let name = path.rsplit('/').next().unwrap_or(path);
        tests.push(Test {
            path: path.clone(),
            area: area.name.clone(),
            kind,
            timeout: if meta.long_timeout {
                Timeout::Long
            } else {
                Timeout::Normal
            },
            xhtml: name.ends_with(".xht") || name.ends_with(".xhtml"),
        });
    }
    Ok(tests)
}

/// Adds every page below `root/dir` to `files`, as paths from `root`. A
/// folder the snapshot does not have adds nothing.
fn walk(root: &Path, dir: &str, files: &mut BTreeSet<String>) -> Result<()> {
    let full = root.join(dir);
    if !full.is_dir() {
        return Ok(());
    }
    let entries =
        std::fs::read_dir(&full).with_context(|| format!("could not list {}", full.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("could not list {}", full.display()))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let path = format!("{dir}/{name}");
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            walk(root, &path, files)?;
        } else if is_page(&name) {
            files.insert(path);
        }
    }
    Ok(())
}

/// Parses every file, spread over the machine's cores. An unreadable file
/// has no metadata, so it is skipped as `other_kind`.
fn parse_all(root: &Path, files: &[String]) -> Vec<Meta> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let chunk = files.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let handles: Vec<_> = files
            .chunks(chunk)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|path| match std::fs::read(root.join(path)) {
                            Ok(bytes) => parse_meta(&serve::url_for(path), &bytes),
                            Err(_) => Meta::default(),
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap_or_default())
            .collect()
    })
}

/// Applies the *Test discovery* table to one page: `None` for a reference
/// or support file, which is not a test.
pub fn classify(
    root: &Path,
    path: &str,
    meta: &Meta,
    referenced: &HashSet<String>,
) -> Option<Kind> {
    let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
    let segments: Vec<&str> = dir.split('/').collect();
    // Rule 2.
    let support_dir = segments
        .iter()
        .any(|s| matches!(*s, "support" | "resources" | "reference" | "reftest"));
    let ref_name = ["-ref.", "-ref-", "-notref.", "-noref."]
        .iter()
        .any(|marker| name.contains(marker));
    if support_dir || ref_name || referenced.contains(path) {
        return None;
    }
    // Rule 3.
    if name.contains("-manual.") {
        return Some(Kind::Skip(SkipReason::Manual));
    }
    // Rule 4.
    let stem = name.split('.').next().unwrap_or(name);
    let python_beside = root.join(dir).join(format!("{stem}.py")).is_file();
    let server_link = meta.links.iter().any(|url| {
        let path = url.path();
        path.ends_with(".py") || path.contains(".sub.")
    });
    if name.contains(".sub.") || python_beside || server_link {
        return Some(Kind::Skip(SkipReason::Server));
    }
    // Rule 5.
    if meta.testharness {
        return Some(Kind::Skip(SkipReason::Testharness));
    }
    // Rule 6.
    if segments.contains(&"crashtests") || stem.ends_with("-crash") {
        return Some(Kind::Crashtest);
    }
    // Rules 7 and 8.
    if meta.refs.is_empty() {
        Some(Kind::Skip(SkipReason::OtherKind))
    } else {
        Some(Kind::Reftest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(path: &str, markup: &str) -> Option<Kind> {
        kind_with(path, markup, &HashSet::new())
    }

    fn kind_with(path: &str, markup: &str, referenced: &HashSet<String>) -> Option<Kind> {
        let meta = parse_meta(&serve::url_for(path), markup.as_bytes());
        classify(Path::new("/nonexistent"), path, &meta, referenced)
    }

    const REFTEST: &str = "<link rel=match href=a-ref.html>";

    #[test]
    fn metadata_comes_from_the_parsed_markup() {
        let meta = parse_meta(
            &serve::url_for("css/x/t.xht"),
            br#"<html xmlns="http://www.w3.org/1999/xhtml"><head>
            <link rel="match" href="../reference/r.xht"/>
            <link rel="Mismatch help" href="/b.html"/>
            <meta name="fuzzy" content="0-2;0-10"/>
            <meta name="timeout" content="long"/>
            <script src="/resources/testharness.js"></script>
            </head><body><img src="i.png"/></body></html>"#,
        );
        let refs: Vec<_> = meta
            .refs
            .iter()
            .map(|(rel, url)| (*rel, serve::path_for(url).unwrap()))
            .collect();
        assert_eq!(
            refs,
            [
                (Relation::Match, "css/reference/r.xht".to_owned()),
                (Relation::Mismatch, "b.html".to_owned()),
            ]
        );
        assert_eq!(meta.fuzzy, ["0-2;0-10"]);
        assert!(meta.long_timeout && meta.testharness);
        assert!(meta.links.iter().any(|u| u.path() == "/css/x/i.png"));
    }

    #[test]
    fn each_discovery_rule_decides_in_order() {
        // Rule 2: references and support files are not tests.
        assert_eq!(kind("css/a/support/t.html", REFTEST), None);
        assert_eq!(kind("css/a/reference/t.html", REFTEST), None);
        assert_eq!(kind("css/a/t-ref.html", REFTEST), None);
        assert_eq!(kind("css/a/t-notref.xht", REFTEST), None);
        assert_eq!(kind("css/a/t-ref-2.html", REFTEST), None);
        let linked = HashSet::from(["css/a/plain.html".to_owned()]);
        assert_eq!(kind_with("css/a/plain.html", REFTEST, &linked), None);
        // Rule 3.
        assert_eq!(
            kind("css/a/t-manual.html", REFTEST),
            Some(Kind::Skip(SkipReason::Manual))
        );
        // Rule 4.
        assert_eq!(
            kind("css/a/t.sub.html", REFTEST),
            Some(Kind::Skip(SkipReason::Server))
        );
        assert_eq!(
            kind(
                "css/a/t.html",
                "<link rel=stylesheet href=s.py><link rel=match href=r.html>"
            ),
            Some(Kind::Skip(SkipReason::Server))
        );
        // Rule 5.
        assert_eq!(
            kind(
                "css/a/t.html",
                "<script src=/resources/testharness.js></script>"
            ),
            Some(Kind::Skip(SkipReason::Testharness))
        );
        // Rule 6.
        assert_eq!(kind("css/a/crashtests/t.html", ""), Some(Kind::Crashtest));
        assert_eq!(kind("css/a/t-crash.html", REFTEST), Some(Kind::Crashtest));
        assert_eq!(
            kind("css/a/t-crash-001.html", ""),
            Some(Kind::Skip(SkipReason::OtherKind)),
            "-crash must sit right before the extension"
        );
        // Rules 7 and 8.
        assert_eq!(kind("css/a/t.html", REFTEST), Some(Kind::Reftest));
        assert_eq!(
            kind("css/a/t.html", "<link rel=mismatch href=x.html>"),
            Some(Kind::Reftest)
        );
        assert_eq!(
            kind("css/a/t.html", "<p>hi"),
            Some(Kind::Skip(SkipReason::OtherKind))
        );
    }
}
