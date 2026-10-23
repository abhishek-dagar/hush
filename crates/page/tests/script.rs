//! AC-31 to AC-35: scripts load in the HTML standard's order, edit the DOM
//! through the slice 1 API, see lifecycle events and timers, keep wrapper
//! identity, and a runaway one cannot take the page down. Scripts report
//! what they saw into the `data-log` attribute of `<html>`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use std::time::{Duration, Instant};

use base::atoms::{LocalName, QualName};
use display_list::{PageId, SizeF};
use ipc::{Destination, DocumentKind, FetchId, NavigationId, ToBrowser};
use page::Page;

const NAV: NavigationId = NavigationId(1);
const URL: &str = "http://test/dir/page.html";
const VIEWPORT: SizeF = SizeF {
    width: 800.0,
    height: 600.0,
};

/// `log(x)` appends `x,` to `<html data-log>`.
const LOG: &str = "<script>function log(x) { var h = document.documentElement; \
     h.setAttribute('data-log', (h.getAttribute('data-log') || '') + x + ','); }</script>";

fn committed() -> Page {
    let mut page = Page::blank(PageId(1), VIEWPORT, 1.0, page::ua_stylesheet());
    page.navigate(NAV);
    assert!(page.commit_navigation(NAV, URL, Some("utf-8".into()), &DocumentKind::Document));
    page.end_task();
    page
}

fn feed(page: &mut Page, html: &str) {
    page.fetch_chunk(NAV, FetchId::DOCUMENT, html.as_bytes());
    page.end_task();
}

fn end(page: &mut Page) {
    page.fetch_done(NAV, FetchId::DOCUMENT);
    page.end_task();
}

/// A page that parsed all of `html`.
fn loaded(html: &str) -> Page {
    let mut page = committed();
    feed(&mut page, &format!("{LOG}{html}"));
    end(&mut page);
    page
}

fn script_fetches(page: &mut Page) -> Vec<(FetchId, String)> {
    page.take_fetch_messages()
        .into_iter()
        .filter_map(|message| match message {
            ToBrowser::Fetch {
                fetch,
                url,
                destination: Destination::Script,
                ..
            } => Some((fetch, url)),
            _ => None,
        })
        .collect()
}

fn serve(page: &mut Page, fetch: FetchId, status: u16, body: &str) {
    page.fetch_response(NAV, fetch, status, &[], "http://test/dir/x.js");
    page.fetch_chunk(NAV, fetch, body.as_bytes());
    page.fetch_done(NAV, fetch);
    page.end_task();
}

/// What the scripts logged, in order.
fn log(page: &Page) -> Vec<String> {
    let doc = &page.main_frame().document;
    let html = doc.document_element().unwrap();
    let value = doc
        .node(html)
        .and_then(|n| n.as_element())
        .and_then(|e| e.attribute(&QualName::attribute(LocalName::from("data-log"))))
        .unwrap_or_default()
        .to_owned();
    value
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn has_id(page: &Page, id: &str) -> bool {
    page.main_frame()
        .document
        .element_by_id(&id.into())
        .is_some()
}

#[test]
fn scripts_run_in_the_order_the_standard_sets() {
    // AC-31, AC-33
    let mut page = committed();
    feed(
        &mut page,
        &(LOG.to_owned()
            + "<head>
            <script>log('inline-head');
              document.addEventListener('DOMContentLoaded', function () { log('dcl:' + document.readyState); });
              addEventListener('load', function () { log('load:' + document.readyState); });</script>
            <script src=blocking.js></script>
            <script defer src=d1.js></script><script defer src=d2.js></script>
            <script async src=a.js></script>
            <script type=module>log('module')</script>
            <script type=text/x-template>log('template')</script>
            <script defer>log('inline-defer-runs-now')</script>
            </head><body><p id=after>after</p>
            <script>log('inline-body:' + !!document.getElementById('after'))</script>"),
    );
    let asked = script_fetches(&mut page);
    let id = |name: &str| {
        asked
            .iter()
            .find(|(_, url)| url.ends_with(name))
            .map(|(fetch, _)| *fetch)
            .unwrap_or_else(|| panic!("{name} not asked for: {asked:?}"))
    };
    // The parser waits at blocking.js: nothing after it is in the tree, so
    // only it was asked for.
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(!has_id(&page, "after"));
    assert_eq!(log(&page), ["inline-head"]);

    serve(&mut page, id("blocking.js"), 200, "log('blocking')");
    end(&mut page);
    let asked = script_fetches(&mut page);
    let id = |name: &str| {
        asked
            .iter()
            .find(|(_, url)| url.ends_with(name))
            .map(|(fetch, _)| *fetch)
            .unwrap()
    };
    assert_eq!(
        log(&page),
        [
            "inline-head",
            "blocking",
            "inline-defer-runs-now",
            "inline-body:true"
        ]
    );

    // The async script runs on arrival; the defers wait for each other.
    serve(&mut page, id("a.js"), 200, "log('async')");
    serve(&mut page, id("d2.js"), 200, "log('d2')");
    assert!(!log(&page).contains(&"d2".to_owned()), "d2 waits for d1");
    serve(&mut page, id("d1.js"), 200, "log('d1')");
    assert_eq!(
        log(&page)[4..],
        ["async", "d1", "d2", "dcl:interactive", "load:complete"]
    );
}

#[test]
fn a_failed_script_is_skipped_and_the_parser_goes_on() {
    // AC-31
    let mut page = committed();
    feed(
        &mut page,
        &format!("{LOG}<script src=gone.js></script><p id=after>"),
    );
    let (fetch, _) = script_fetches(&mut page)[0].clone();
    serve(&mut page, fetch, 404, "log('should not run')");
    end(&mut page);
    assert!(has_id(&page, "after"));
    assert!(log(&page).is_empty());
}

#[test]
fn scripts_edit_the_dom_through_the_slice_1_api() {
    // AC-32
    let mut page = loaded(
        "<body><ul id=list><li class=a>one</li><li class=b>two</li></ul><p id=p>old</p>
        <script>
          var list = document.querySelector('#list');
          var item = document.createElement('LI');
          item.textContent = 'three';
          item.classList.add('c', 'new');
          item.classList.toggle('new');
          list.appendChild(item);
          list.removeChild(list.querySelector('.a'));
          log(list.children.length + ':' + list.firstChild.textContent + ':' + item.className);
          log(document.querySelectorAll('li').length + ':' + item.parentNode.tagName);
          var p = document.getElementById('p');
          p.textContent = 'new text';
          p.style.color = 'red';
          p.style.marginTop = '4px';
          log(p.getAttribute('style') + '|' + p.style.color);
          log(p.nodeType + ':' + p.nodeName + ':' + document.body.contains(p) + ':' + (p instanceof Element));
          try { list.appendChild(list); } catch (e) { log(e.name); }
          try { list.removeChild(document.body); } catch (e) { log(e.name); }
          try { document.querySelector('p::nope'); } catch (e) { log(e.name + ':' + (e instanceof DOMException)); }
          log(String(document.querySelector('#nothing')));
        </script>",
    );
    assert_eq!(
        log(&page),
        [
            "2:two:c",
            "2:UL",
            "color: red; margin-top: 4px;|red",
            "1:P:true:true",
            "HierarchyRequestError",
            "NotFoundError",
            "SyntaxError:true",
            "null"
        ]
    );
    // The edits are in the document, and the next frame draws them.
    assert!(page.take_frame_request());
    assert!(page.begin_frame(0.0).is_some());
    let doc = &page.main_frame().document;
    let p = doc.element_by_id(&"p".into()).unwrap();
    let text: Vec<_> = doc
        .children(p)
        .filter_map(|c| match doc.node(c)?.data() {
            dom::NodeData::Text(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, ["new text"]);
}

#[test]
fn timers_fire_later_and_wrappers_keep_their_identity() {
    // AC-33, AC-34
    let mut page = loaded(
        "<body><p id=a>a</p><script>
          log(document.getElementById('a') === document.getElementById('a'));
          window.kept = document.createElement('div');
          kept.id = 'kept';
          setTimeout(function (x) {
            log('timer:' + x);
            document.body.appendChild(kept);
            Promise.resolve().then(function () { log('microtask'); });
          }, 10, 'arg');
          var gone = setTimeout(function () { log('cleared'); }, 0);
          clearTimeout(gone);
        </script>",
    );
    assert_eq!(log(&page)[0], "true");
    let due = page.next_timer().expect("a timer is pending");
    assert!(due > Instant::now() - Duration::from_secs(1));
    // Sweeps in between: the detached element lives, pinned by its wrapper.
    for _ in 0..3 {
        page.end_task();
    }
    page.run_timers(Instant::now() + Duration::from_millis(50));
    assert_eq!(log(&page)[1..], ["timer:arg", "microtask"]);
    assert!(has_id(&page, "kept"), "the held element survived the sweep");
    assert_eq!(page.next_timer(), None, "the cleared timer never fires");
}

#[test]
fn a_runaway_script_is_stopped_and_the_page_goes_on() {
    // AC-35
    let started = Instant::now();
    let mut page = loaded(
        "<body><script>setTimeout(function () { log('later'); }, 0); for (;;) {}</script>
        <p id=after>after</p>",
    );
    assert!(started.elapsed() >= script_quickjs::TASK_DEADLINE);
    assert!(has_id(&page, "after"), "parsing went on");
    page.run_timers(Instant::now() + Duration::from_millis(10));
    assert_eq!(log(&page), ["later"]);
}
