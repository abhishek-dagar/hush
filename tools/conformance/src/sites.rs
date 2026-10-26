//! Proof sites: the saved pages the engine is checked against (spec 0009,
//! AC-22), and the `conformance sites` subcommand.
//!
//! `conformance sites` validates the checked-in sites: `sites.toml` parses,
//! every listed site folder has an `index.html` and a `LICENSE`, and each
//! site names at most 20 landmarks.
//!
//! `conformance sites --capture` re-renders each page in Chrome and writes
//! `chrome.json` (each landmark's border box plus the Chrome version) and
//! `chrome.png` (Chrome's screenshot) beside the page. Capture needs Chrome
//! (`CHROME` env var) and only runs on a developer machine with Chrome
//! installed: without it, capture stops with exit 2 and a hint. The browser,
//! the shell and the engine never run Chrome.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

/// The whole `sites.toml` file, next to `subset.toml` in the crate folder.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sites {
    #[serde(rename = "site")]
    pub sites: Vec<Site>,
}

/// One proof site: a saved page and the landmarks whose border boxes are
/// compared against Chrome's rendering (spec 0009, AC-22).
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Site {
    /// Display name, also the folder name under `sites/`.
    pub name: String,
    /// The page HTML, relative to the site folder.
    pub path: PathBuf,
    /// CSS selectors for the landmarks, at most 20.
    pub landmarks: Vec<String>,
}

/// One landmark's border box, as Chrome measured it.
#[derive(Debug, Clone, Serialize)]
pub struct LandmarkRect {
    /// The selector that matched the landmark.
    pub selector: String,
    /// The landmark's index in the site's landmark list, so `chrome.json`
    /// can match it back to the selector.
    pub index: usize,
    /// The border box Chrome reported: `[x, y, width, height]` in CSS px.
    pub rect: [f64; 4],
}

impl Sites {
    /// Loads `sites.toml` from `dir` and checks every listed site folder has
    /// an `index.html` and a `LICENSE`, and that each site names at most 20
    /// non-empty landmarks.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("sites.toml");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("could not read {}", path.display()))?;
        let sites: Self = toml::from_str(&text)
            .with_context(|| format!("{} is not valid sites.toml", path.display()))?;
        sites.validate(dir)?;
        Ok(sites)
    }

    fn validate(&self, dir: &Path) -> Result<()> {
        ensure!(!self.sites.is_empty(), "sites.toml lists no sites");
        for site in &self.sites {
            ensure!(!site.name.is_empty(), "a site has no name");
            ensure!(
                site.name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
                "site name `{}` is not a safe folder name",
                site.name
            );
            let folder = Self::folder(site, dir);
            ensure!(
                folder.is_dir(),
                "site folder `{}` is missing",
                folder.display()
            );
            ensure!(
                folder.join(&site.path).is_file(),
                "site `{}` has no page at `{}`",
                site.name,
                folder.join(&site.path).display()
            );
            ensure!(
                folder.join("LICENSE").is_file(),
                "site `{}` has no LICENSE",
                site.name
            );
            ensure!(
                site.landmarks.len() <= 20,
                "site `{}` lists {} landmarks (at most 20)",
                site.name,
                site.landmarks.len()
            );
            for selector in &site.landmarks {
                ensure!(
                    !selector.is_empty(),
                    "site `{}` has an empty landmark selector",
                    site.name
                );
            }
        }
        Ok(())
    }

    /// The folder holding `site`, beside `sites.toml`.
    pub fn folder(site: &Site, dir: &Path) -> PathBuf {
        dir.join("sites").join(&site.name)
    }

    /// Captures Chrome's rendering of every site into its folder: `chrome.json`
    /// (landmark border boxes plus the Chrome version) and `chrome.png`
    /// (Chrome's screenshot). Returns exit code 2 with a hint when `CHROME` is
    /// not set, since capture only runs on a developer machine. Any other
    /// failure is propagated as an error (exit 1).
    pub fn capture(&self, dir: &Path) -> Result<ExitCode> {
        let chrome = match std::env::var("CHROME") {
            Ok(path) if !path.is_empty() => path,
            _ => {
                eprintln!("capture needs Chrome: set CHROME to the path of a Chrome binary, e.g.");
                eprintln!(
                    "  CHROME=/Applications/Google\\ Chrome.app/Contents/MacOS/Google\\ Chrome conformance sites --capture"
                );
                return Ok(ExitCode::from(2));
            }
        };
        for site in &self.sites {
            capture_site(&chrome, site, dir)
                .with_context(|| format!("could not capture site `{}`", site.name))?;
        }
        Ok(ExitCode::from(0))
    }
}

/// One site's captured Chrome data: the landmarks and the Chrome version.
#[derive(Debug, Clone, Serialize)]
pub struct ChromeCapture {
    /// Chrome's version string, e.g. `139.0.7224.0`.
    pub chrome_version: String,
    /// The view port used for the capture, `[width, height]` in CSS px.
    pub viewport: [u32; 2],
    /// Each landmark's border box.
    pub landmarks: Vec<LandmarkRect>,
}

/// Captures one site: injects a landmark-measurement script into a copy of
/// the saved page, runs Chrome headless on the copy with `--dump-dom` and
/// `--screenshot`, and writes `chrome.json` and `chrome.png` beside the page.
/// The saved `index.html` itself is never modified.
fn capture_site(chrome: &str, site: &Site, dir: &Path) -> Result<()> {
    let folder = Sites::folder(site, dir);
    let page = folder.join(&site.path);

    let page_html = std::fs::read_to_string(&page)
        .with_context(|| format!("could not read `{}`", page.display()))?;
    let runner_html = build_runner(&page_html, &site.landmarks);

    // The runner lives in the site folder beside the page. Chrome loads it
    // directly (no iframe), so the measurement script runs on the real page
    // DOM and `--dump-dom` captures the filled-in landmark rects.
    let runner = folder.join(".capture-runner.html");
    std::fs::write(&runner, &runner_html)
        .with_context(|| format!("could not write capture runner for `{}`", site.name))?;

    let screenshot = folder.join("chrome.png");
    let runner_url = file_url(&runner);
    let status = std::process::Command::new(chrome)
        .arg("--headless=new")
        .arg("--disable-gpu")
        .arg("--no-sandbox")
        .arg("--window-size=1280,800")
        .arg("--virtual-time-budget=3000")
        .arg("--run-all-compositor-stages-before-paint")
        .arg("--dump-dom")
        .arg(format!("--screenshot={}", screenshot.display()))
        .arg(&runner_url)
        .status()
        .with_context(|| format!("could not launch Chrome for `{}`", site.name))?;

    if !status.success() {
        bail!(
            "Chrome exited with {status:?} while capturing `{}`",
            site.name
        );
    }

    let output = std::process::Command::new(chrome)
        .arg("--headless=new")
        .arg("--disable-gpu")
        .arg("--no-sandbox")
        .arg("--window-size=1280,800")
        .arg("--virtual-time-budget=3000")
        .arg("--run-all-compositor-stages-before-paint")
        .arg("--dump-dom")
        .arg(&runner_url)
        .output()
        .with_context(|| format!("could not dump DOM for `{}`", site.name))?;

    let dom = String::from_utf8_lossy(&output.stdout);
    let landmarks = parse_rects(&dom, &site.landmarks)
        .with_context(|| format!("could not parse landmark rects for `{}`", site.name))?;

    let capture = ChromeCapture {
        chrome_version: chrome_version(chrome)?,
        viewport: [1280, 800],
        landmarks,
    };
    let json = serde_json::to_string_pretty(&capture).context("could not serialize chrome.json")?;
    std::fs::write(folder.join("chrome.json"), json)
        .with_context(|| format!("could not write chrome.json for `{}`", site.name))?;

    let _ = std::fs::remove_file(&runner);
    Ok(())
}

/// Builds a capture runner: a copy of the page HTML with a landmark
/// measurement script injected. The `#landmarks` block goes right after
/// `<body>` so the measurement script (placed just before `</body>`) can fill
/// it with each landmark's `getBoundingClientRect`.
#[allow(
    clippy::unwrap_used,
    reason = "serde_json::to_string on a Vec<String> cannot fail, so unwrap is safe here"
)]
fn build_runner(page_html: &str, selectors: &[String]) -> String {
    let selectors_json = serde_json::to_string(selectors).unwrap();
    let landmarks_block = "<script id=\"landmarks\" type=\"application/json\"></script>";
    let measure_script = format!(
        r#"<script>
(function () {{
  var selectors = {selectors_json};
  var results = [];
  selectors.forEach(function (sel, index) {{
    var node = document.querySelector(sel);
    if (node) {{
      var rect = node.getBoundingClientRect();
      results.push({{selector: sel, index: index, rect: [rect.x, rect.y, rect.width, rect.height]}});
    }} else {{
      results.push({{selector: sel, index: index, rect: null}});
    }}
  }});
  document.getElementById("landmarks").textContent = JSON.stringify(results);
}})();
</script>"#
    );

    // Insert the landmarks block right after the opening <body> tag.
    let with_landmarks = insert_after_body_open(page_html, landmarks_block);

    // Insert the measurement script just before </body>, or </html>, or at
    // the very end of the document.
    insert_before_close(&with_landmarks, &measure_script)
}

/// Finds the first `<body ...>` tag (case insensitive) and inserts `text`
/// right after its closing `>`. If there is no `<body>` tag, inserts after
/// the opening `<html ...>` tag, or at the very start of the document.
fn insert_after_body_open(html: &str, text: &str) -> String {
    let lower = html.to_lowercase();
    let body_start = match lower.find("<body") {
        Some(pos) => pos,
        None => match lower.find("<html") {
            Some(pos) => pos,
            None => return format!("{text}{html}"),
        },
    };
    let tag_end = html[body_start..].find('>').map(|i| body_start + i + 1);
    match tag_end {
        Some(end) => format!("{}{}{}", &html[..end], text, &html[end..]),
        None => format!("{text}{html}"),
    }
}

/// Inserts `text` just before `</body>` (case insensitive), or `</html>`,
/// or at the very end of the document if neither is found.
fn insert_before_close(html: &str, text: &str) -> String {
    let lower = html.to_lowercase();
    let close = lower.find("</body>").or_else(|| lower.find("</html>"));
    match close {
        Some(pos) => format!("{}{}{}", &html[..pos], text, &html[pos..]),
        None => format!("{html}{text}"),
    }
}

/// Turns a path into an absolute `file://` URL Chrome can load.
#[allow(
    clippy::expect_used,
    reason = "the current directory is always readable for a cargo-invoked tool"
)]
fn file_url(path: &Path) -> String {
    // The current directory is always readable for a cargo-invoked tool.
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .expect("current dir is readable")
            .join(path)
    };
    format!("file://{}", absolute.display())
}

/// Parses the landmark rects Chrome wrote into the runner's DOM from the
/// `--dump-dom` output. Each landmark is a `{selector, index, rect}` entry
/// in the `#landmarks` JSON block.
fn parse_rects(dom: &str, selectors: &[String]) -> Result<Vec<LandmarkRect>> {
    let start = dom
        .find("<script id=\"landmarks\"")
        .context("dump-dom output has no landmarks block")?;
    let content_start = dom[start..]
        .find('>')
        .map(|i| start + i + 1)
        .context("landmarks block is not closed")?;
    let end = dom[content_start..]
        .find("</script>")
        .map(|i| content_start + i)
        .context("landmarks block has no closing tag")?;
    let json = &dom[content_start..end];
    let entries: Vec<serde_json::Value> = serde_json::from_str(json)
        .with_context(|| format!("landmarks JSON is not valid: {json:?}"))?;
    let mut landmarks = Vec::new();
    for (position, entry) in entries.into_iter().enumerate() {
        let selector = entry
            .get("selector")
            .and_then(|v| v.as_str())
            .context("landmark entry has no selector")?;
        let index = entry
            .get("index")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(position);
        let rect = entry
            .get("rect")
            .and_then(|v| v.as_array())
            .map(|arr| {
                let mut out = [0.0; 4];
                for (i, item) in arr.iter().enumerate().take(4) {
                    out[i] = item.as_f64().unwrap_or(0.0);
                }
                out
            })
            .unwrap_or([0.0; 4]);
        landmarks.push(LandmarkRect {
            selector: selector.to_owned(),
            index,
            rect,
        });
    }
    ensure!(
        landmarks.len() == selectors.len(),
        "parsed {} landmarks but site lists {}",
        landmarks.len(),
        selectors.len()
    );
    Ok(landmarks)
}

/// Reads Chrome's version string with `--version`.
fn chrome_version(chrome: &str) -> Result<String> {
    let output = std::process::Command::new(chrome)
        .arg("--version")
        .output()
        .context("could not run `--version` on Chrome")?;
    let text = String::from_utf8_lossy(&output.stdout);
    let version = text
        .split_whitespace()
        .find(|word| word.starts_with(|c: char| c.is_ascii_digit()))
        .context("Chrome `--version` did not report a version")?;
    Ok(version.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(name: &str, selectors: &[&str]) -> Site {
        Site {
            name: name.to_owned(),
            path: PathBuf::from("index.html"),
            landmarks: selectors.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn build_runner_injects_landmarks_after_body_and_measurement_before_close() {
        let page = "<html><body><div id=\"header\">Header</div></body></html>";
        let selectors: &[String] = &["#header".to_owned(), ".figure".to_owned()];
        let runner = build_runner(page, selectors);
        assert!(runner.contains("<body>"));
        assert!(runner.contains("id=\"landmarks\""));
        // landmarks block comes right after <body>
        let body_pos = runner.find("<body>").unwrap();
        let landmarks_pos = runner.find("id=\"landmarks\"").unwrap();
        assert!(landmarks_pos > body_pos);
        assert!(runner.contains("getBoundingClientRect"));
        assert!(runner.contains("\"#header\""));
        assert!(runner.contains("\".figure\""));
        // measurement script is injected (it references document.querySelector)
        assert!(runner.contains("document.querySelector"));
        assert!(!runner.contains("iframe"));
        assert!(!runner.contains("contentDocument"));
    }

    #[test]
    fn parse_rects_reads_each_landmark_from_the_dump_dom_block() {
        let dom = "<html><body><script id=\"landmarks\" type=\"application/json\">\
[{\"selector\":\"#header\",\"index\":0,\"rect\":[10.0,20.0,300.0,50.0]},{\"selector\":\".figure\",\"index\":1,\"rect\":[0.0,100.0,200.0,300.0]}]\
</script></body></html>";
        let selectors: &[String] = &["#header".to_owned(), ".figure".to_owned()];
        let rects = parse_rects(dom, selectors).unwrap();
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].selector, "#header");
        assert_eq!(rects[0].index, 0);
        assert_eq!(rects[0].rect, [10.0, 20.0, 300.0, 50.0]);
        assert_eq!(rects[1].selector, ".figure");
        assert_eq!(rects[1].rect, [0.0, 100.0, 200.0, 300.0]);
    }

    #[test]
    fn parse_rects_reports_null_rect_for_an_unmatched_selector() {
        let dom = "<html><body><script id=\"landmarks\" type=\"application/json\">\
[{\"selector\":\"#missing\",\"index\":0,\"rect\":null}]\
</script></body></html>";
        let rects = parse_rects(dom, &["#missing".to_owned()]).unwrap();
        assert_eq!(rects[0].rect, [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn capture_without_chrome_exits_two() {
        // CHROME is not set in this environment, so capture must report exit 2
        // instead of launching anything.
        let sites = Sites {
            sites: vec![site("news-lookalike", &["#header"])],
        };
        let dir = PathBuf::from(".");
        // `capture` only checks CHROME before touching the file system.
        let result = sites.capture(&dir).unwrap();
        assert_eq!(result, ExitCode::from(2));
    }
}
