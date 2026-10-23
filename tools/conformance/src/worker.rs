//! Worker mode: runs one test at a time over line JSON on stdin/stdout
//! (spec 0005, *The run, step by step* / Worker).

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use display_list::{PageId, SizeF};
use ipc::{DocumentKind, FetchId, NavigationId, ToBrowser};
use page::{FontContext, Page, ReadyState};
use render::Renderer;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::compare;
use crate::discover::{self, Kind, Meta};
use crate::serve;
use crate::status::Status;
use crate::subset::Subset;

use base::atoms::{LocalName, QualName};

const NAV: NavigationId = NavigationId(1);
const VIEWPORT: SizeF = SizeF {
    width: 800.0,
    height: 600.0,
};
const CANTARELL: &[u8] = include_bytes!("../fonts/Cantarell-Regular.ttf");

#[derive(Deserialize)]
struct Request {
    test: String,
}

#[derive(Serialize)]
struct Response {
    status: String,
    message: String,
    duration_ms: u64,
    images: Option<ImagePaths>,
    parsed_as: String,
}

#[derive(Serialize, Clone)]
struct ImagePaths {
    test: String,
    ref_: String,
    diff: String,
}

/// Runs the worker loop until stdin closes.
pub fn run(wpt_dir: &Path, no_render: bool, subset_path: &Path) -> Result<()> {
    let subset = Subset::load(subset_path)?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let stdin = std::io::stdin();
    let reader = BufReader::new(stdin);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                let resp = Response {
                    status: Status::Error.as_str().to_owned(),
                    message: format!("bad request: {e}"),
                    duration_ms: 0,
                    images: None,
                    parsed_as: "html".to_owned(),
                };
                writeln!(out, "{}", serde_json::to_string(&resp).expect("serialization should not fail"))?;
                out.flush()?;
                continue;
            }
        };
        let started = Instant::now();
        let (status, message, images, parsed_as) =
            run_test(wpt_dir, no_render, &subset, &req.test);
        let resp = Response {
            status: status.as_str().to_owned(),
            message,
            duration_ms: started.elapsed().as_millis() as u64,
            images,
            parsed_as,
        };
        writeln!(out, "{}", serde_json::to_string(&resp).expect("serialization should not fail"))?;
        out.flush()?;
    }
    Ok(())
}

fn run_test(
    wpt_dir: &Path,
    no_render: bool,
    subset: &Subset,
    path: &str,
) -> (Status, String, Option<ImagePaths>, String) {
    let _area = match subset.area_of(path) {
        Some(a) => a.name.clone(),
        None => {
            return (
                Status::Error,
                "test is not in any area".to_owned(),
                None,
                "html".to_owned(),
            )
        }
    };
    let _xhtml = path.ends_with(".xht") || path.ends_with(".xhtml");
    let parsed_as = "html".to_owned();
    let bytes = match std::fs::read(wpt_dir.join(path)) {
        Ok(b) => b,
        Err(e) => {
            return (Status::Error, format!("could not read test: {e}"), None, parsed_as);
        }
    };
    let meta = discover::parse_meta(&serve::url_for(path), &bytes);
    let referenced: std::collections::HashSet<String> = std::collections::HashSet::new();
    let kind = discover::classify(wpt_dir, path, &meta, &referenced);
    let kind = match kind {
        Some(k) => k,
        None => {
            return (
                Status::Skip, // not a test
                "reference or support file".to_owned(),
                None,
                parsed_as,
            )
        }
    };
    let kind = match kind {
        Kind::Reftest => Kind::Reftest,
        Kind::Crashtest => Kind::Crashtest,
        Kind::Skip(r) => {
            return (Status::Skip, r.as_str().to_owned(), None, parsed_as);
        }
    };
    let timeout = if meta.long_timeout {
        Duration::from_secs(60)
    } else {
        Duration::from_secs(10)
    };
    // crashtest
    if matches!(kind, Kind::Crashtest) {
        return run_crashtest(wpt_dir, no_render, path, &meta, timeout, parsed_as);
    }
    // reftest
    run_reftest(wpt_dir, no_render, path, &meta, timeout, parsed_as)
}

fn run_crashtest(
    wpt_dir: &Path,
    no_render: bool,
    path: &str,
    _meta: &Meta,
    timeout: Duration,
    parsed_as: String,
) -> (Status, String, Option<ImagePaths>, String) {
    let started = Instant::now();
    let render = if no_render {
        None
    } else {
        match Renderer::new_headless(800, 600) {
            Ok(r) => Some(r),
            Err(e) => {
                return (Status::Error, format!("renderer: {e}"), None, parsed_as);
            }
        }
    };
    let mut page = Page::blank(PageId(1), VIEWPORT, 1.0, page::ua_stylesheet());
    let ahem = std::fs::read(wpt_dir.join("fonts/Ahem.ttf")).unwrap_or_default();
    page.use_fonts(FontContext::with_families(CANTARELL.to_vec(), [ahem]));
    let url = serve::url_for(path);
    let resp = serve::serve(wpt_dir, &url);
    let charset = serve::charset(&resp.headers);
    page.navigate(NAV);
    if !page.commit_navigation(NAV, &url, charset, &DocumentKind::Document) {
        return (Status::Error, "commit failed".to_owned(), None, parsed_as);
    }
    page.end_task();
    // feed body
    for chunk in resp.body.chunks(64) {
        page.fetch_chunk(NAV, FetchId::DOCUMENT, chunk);
        page.end_task();
    }
    page.fetch_done(NAV, FetchId::DOCUMENT);
    page.end_task();
    // settle
    match settle(&mut page, wpt_dir, timeout, started, false) {
        Ok(()) => {}
        Err(s) => return (s, "did not settle".to_owned(), None, parsed_as),
    }
    // draw one frame
    if !no_render {
        let mut renderer = match render {
            Some(r) => r,
            None => return (Status::Error, "renderer missing".to_owned(), None, parsed_as),
        };
        let list = page.begin_frame(0.0);
        let (adds, deletes) = page.take_resource_messages();
        apply_resources(&mut renderer, PageId(1), &adds);
        if renderer.render(list.as_ref(), 1.0, None, None).is_err() {
            return (Status::Fail, "render failed".to_owned(), None, parsed_as);
        }
        let _ = renderer.read_pixels();
        apply_deletes(&mut renderer, PageId(1), &deletes);
    } else {
        let _ = page.begin_frame(0.0);
    }
    (Status::Pass, "ok".to_owned(), None, parsed_as)
}

fn run_reftest(
    wpt_dir: &Path,
    no_render: bool,
    path: &str,
    meta: &Meta,
    timeout: Duration,
    parsed_as: String,
) -> (Status, String, Option<ImagePaths>, String) {
    if no_render {
        return (Status::Skip, "no render".to_owned(), None, parsed_as);
    }
    // render test
    let test_render = match render_page(wpt_dir, path, timeout) {
        Ok(r) => r,
        Err(e) => return (Status::Error, e.to_string(), None, parsed_as),
    };
    if let Err(s) = test_render.status {
        return (s, test_render.message, None, parsed_as);
    }
    let test_pixels = match test_render.pixels {
        Some(p) => p,
        None => return (Status::Error, "pixels missing after render".to_owned(), None, parsed_as),
    };
    // references
    let mut images_for_failure: Option<ImagePaths> = None;
    let mut all_ok = true;
    let mut fail_msg = String::new();
    for (rel, ref_url) in &meta.refs {
        let ref_path = match serve::path_for(ref_url) {
            Some(p) => p,
            None => {
                return (Status::Error, "bad reference url".to_owned(), None, parsed_as);
            }
        };
        let ref_render = match render_page(wpt_dir, &ref_path, timeout) {
            Ok(r) => r,
            Err(e) => return (Status::Error, e.to_string(), None, parsed_as),
        };
        if let Err(s) = ref_render.status {
            return (s, ref_render.message, None, parsed_as);
        }
        let ref_pixels = match ref_render.pixels {
            Some(p) => p,
            None => return (Status::Error, "pixels missing after render".to_owned(), None, parsed_as),
        };
        // fuzzy
        let fuzzy = find_fuzzy(meta, ref_url, &ref_render.meta);
        let cmp = compare::compare(&test_pixels, &ref_pixels, fuzzy.as_ref());
        let equal = cmp.equal;
        let should_equal = matches!(rel, discover::Relation::Match);
        let ok = if should_equal { equal } else { !equal };
        if !ok {
            all_ok = false;
            fail_msg = cmp.message;
            // save images
            let dir = crate::report::conformance_dir().join("images").join(path);
            let test_p = dir.join("test.png");
            let ref_p = dir.join("ref.png");
            let diff_p = dir.join("diff.png");
            let _ = crate::report::save_png(&test_p, &test_pixels, 800, 600);
            let _ = crate::report::save_png(&ref_p, &ref_pixels, 800, 600);
            let diff = compare::diff_image(&test_pixels, &ref_pixels);
            let _ = crate::report::save_png(&diff_p, &diff, 800, 600);
            images_for_failure = Some(ImagePaths {
                test: test_p.display().to_string(),
                ref_: ref_p.display().to_string(),
                diff: diff_p.display().to_string(),
            });
        }
    }
    if all_ok {
        (Status::Pass, "ok".to_owned(), None, parsed_as)
    } else {
        (
            Status::Fail,
            fail_msg,
            images_for_failure,
            parsed_as,
        )
    }
}

struct RenderedPage {
    status: Result<(), Status>,
    message: String,
    pixels: Option<Vec<u8>>,
    meta: Meta,
}

fn render_page(wpt_dir: &Path, path: &str, timeout: Duration) -> Result<RenderedPage, String> {
    let bytes = std::fs::read(wpt_dir.join(path)).map_err(|e| format!("{e}"))?;
    let meta = discover::parse_meta(&serve::url_for(path), &bytes);
    let url = serve::url_for(path);
    let resp = serve::serve(wpt_dir, &url);
    let charset = serve::charset(&resp.headers);
    let mut page = Page::blank(PageId(1), VIEWPORT, 1.0, page::ua_stylesheet());
    let ahem = std::fs::read(wpt_dir.join("fonts/Ahem.ttf")).unwrap_or_default();
    page.use_fonts(FontContext::with_families(CANTARELL.to_vec(), [ahem]));
    page.navigate(NAV);
    if !page.commit_navigation(NAV, &url, charset, &DocumentKind::Document) {
        return Err("commit failed".to_owned());
    }
    page.end_task();
    for chunk in resp.body.chunks(64) {
        page.fetch_chunk(NAV, FetchId::DOCUMENT, chunk);
        page.end_task();
    }
    page.fetch_done(NAV, FetchId::DOCUMENT);
    page.end_task();
    let started = Instant::now();
    match settle(&mut page, wpt_dir, timeout, started, true) {
        Ok(()) => {}
        Err(s) => {
            return Ok(RenderedPage {
                status: Err(s),
                message: "did not settle".to_owned(),
                pixels: None,
                meta,
            })
        }
    }
    let mut renderer = Renderer::new_headless(800, 600).map_err(|e| format!("{e}"))?;
    let list = page.begin_frame(0.0);
    let (adds, deletes) = page.take_resource_messages();
    apply_resources(&mut renderer, PageId(1), &adds);
    if renderer.render(list.as_ref(), 1.0, None, None).is_err() {
        apply_deletes(&mut renderer, PageId(1), &deletes);
        return Ok(RenderedPage {
            status: Err(Status::Fail),
            message: "render failed".to_owned(),
            pixels: None,
            meta,
        });
    }
    let pixels = renderer.read_pixels().map_err(|e| format!("{e}"))?;
    apply_deletes(&mut renderer, PageId(1), &deletes);
    Ok(RenderedPage {
        status: Ok(()),
        message: "ok".to_owned(),
        pixels: Some(pixels),
        meta,
    })
}

fn settle(
    page: &mut Page,
    wpt_dir: &Path,
    timeout: Duration,
    started: Instant,
    _reftest: bool,
) -> Result<(), Status> {
    for _ in 0..10000 {
        if started.elapsed() > timeout {
            return Err(Status::Timeout);
        }
        let fetches: Vec<_> = page
            .take_fetch_messages()
            .into_iter()
            .filter_map(|m| match m {
                ToBrowser::Fetch { fetch, url, .. } => Some((fetch, url)),
                _ => None,
            })
            .collect();
        if !fetches.is_empty() {
            for (fetch, url) in fetches {
                let resp = serve::serve(wpt_dir, &url);
                let headers = resp.headers.clone();
                if resp.status == 200 {
                    page.fetch_response(NAV, fetch, 200, &headers, &url);
                    page.fetch_chunk(NAV, fetch, &resp.body);
                } else {
                    page.fetch_response(NAV, fetch, 404, &headers, &url);
                }
                page.fetch_done(NAV, fetch);
                page.end_task();
            }
            continue;
        }
        // check done
        let ready = page.ready_state();
        let root_has_wait = root_has_class(page, "reftest-wait") || root_has_class(page, "test-wait");
        if ready == ReadyState::Complete && !root_has_wait {
            // run due timers just in case
            if let Some(t) = page.next_timer() && t <= Instant::now() {
                page.run_timers(Instant::now());
                page.end_task();
                continue;
            }
            return Ok(());
        }
        // run due timers
        if let Some(t) = page.next_timer() {
            if t <= Instant::now() {
                page.run_timers(Instant::now());
                page.end_task();
                continue;
            }
            // sleep until timer or timeout
            let wait = t.saturating_duration_since(Instant::now());
            let wait = wait.min(Duration::from_millis(50));
            std::thread::sleep(wait);
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
        // also try begin_frame to trigger reftest-wait removal? The page removes class via script; timers handle it.
    }
    Err(Status::Timeout)
}

fn root_has_class(page: &Page, class: &str) -> bool {
    let frame = page.main_frame();
    let doc = &frame.document;
    let root = doc.root();
    let class_attr = QualName::attribute(LocalName::from("class"));
    if let Some(el) = doc.node(root).and_then(|n| n.as_element())
        && let Some(c) = el.attribute(&class_attr)
    {
        return c.split_whitespace().any(|t| t == class);
    }
    false
}

fn apply_resources(renderer: &mut Renderer, page: PageId, msgs: &[ToBrowser]) {
    for msg in msgs {
        match msg {
            ToBrowser::AddFont { key, data, index, variations, .. } => {
                renderer.add_font(page, *key, data, *index, variations);
            }
            ToBrowser::AddImage { key, size, pixels, .. } => {
                renderer.add_image(page, *key, (size.0, size.1), display_list::ImageFormat::Rgba8, pixels);
            }
            _ => {}
        }
    }
}

fn apply_deletes(renderer: &mut Renderer, page: PageId, msgs: &[ToBrowser]) {
    for msg in msgs {
        match msg {
            ToBrowser::DeleteFont { key, .. } => {
                renderer.delete_font(page, *key);
            }
            ToBrowser::DeleteImage { key, .. } => {
                renderer.delete_image(page, *key);
            }
            _ => {}
        }
    }
}

fn find_fuzzy(test_meta: &Meta, ref_url: &Url, ref_meta: &Meta) -> Option<compare::Fuzzy> {
    // test's fuzzy wins
    for f in &test_meta.fuzzy {
        if let Ok(parsed) = compare::Fuzzy::parse(f)
            && (parsed.ref_url.is_none() || parsed.ref_url.as_ref() == serve::path_for(ref_url).as_ref())
        {
            return Some(parsed);
        }
    }
    for f in &ref_meta.fuzzy {
        if let Ok(parsed) = compare::Fuzzy::parse(f) && parsed.ref_url.is_none() {
            return Some(parsed);
        }
    }
    None
}
