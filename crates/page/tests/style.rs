//! AC-18 to AC-22: sheets from every source cascade into computed styles,
//! checked on real parsed pages through the computed style dump.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use std::time::{Duration, Instant};

use base::atoms::{Atom, LocalName, QualName};
use display_list::{PageId, SizeF};
use dom::NodeFlags;
use ipc::{Destination, DocumentKind, FetchId, NavigationId, ToBrowser};
use page::Page;

const NAV: NavigationId = NavigationId(1);
const URL: &str = "http://test/dir/page.html";
const VIEWPORT: SizeF = SizeF {
    width: 800.0,
    height: 600.0,
};

/// A page that committed `html` and parsed it all, before its first frame.
/// Each message is its own task, as in the content process, so the parser
/// is still running when the body's task ends.
fn parsed(html: &str) -> Page {
    let mut page = Page::blank(PageId(1), VIEWPORT, 1.0, page::ua_stylesheet());
    page.navigate(NAV);
    assert!(page.commit_navigation(NAV, URL, Some("utf-8".into()), &DocumentKind::Document));
    page.end_task();
    page.fetch_chunk(NAV, FetchId::DOCUMENT, html.as_bytes());
    page.end_task();
    page.fetch_done(NAV, FetchId::DOCUMENT);
    page.end_task();
    page
}

/// [`parsed`], then the first frame.
fn load(html: &str) -> Page {
    let mut page = parsed(html);
    assert!(page.begin_frame(0.0).is_some(), "no first frame");
    page
}

/// The style dump of `node`.
fn style_of(page: &Page, node: dom::NodeId) -> String {
    let doc = &page.main_frame().document;
    doc.node(node)
        .and_then(|n| n.as_element())
        .and_then(|e| e.style())
        .map(|s| s.dump())
        .unwrap_or_else(|| "(unstyled)".into())
}

fn element(page: &Page, id: &str) -> dom::NodeId {
    page.main_frame()
        .document
        .element_by_id(&Atom::from(id))
        .unwrap_or_else(|| panic!("no #{id}"))
}

/// The style dump of the element with `id`.
fn style(page: &Page, id: &str) -> String {
    style_of(page, element(page, id))
}

/// One property's value in `id`'s dump, or `None` when it is initial.
fn value(page: &Page, id: &str, property: &str) -> Option<String> {
    let dump = style(page, id);
    dump.split("; ").find_map(|pair| {
        let (name, value) = pair.split_once(": ")?;
        (name == property).then(|| value.to_owned())
    })
}

fn color(page: &Page, id: &str) -> String {
    value(page, id, "color").unwrap_or_else(|| "#000000ff".into())
}

#[test]
fn origins_importance_specificity_and_order_decide_the_winner() {
    // AC-18
    let page = load(
        r#"<style>
            p { color: red }
            #id { color: green }
            .important { color: blue !important }
            p.order { color: red }
            p.order { color: purple }
            h1 { font-size: 10px }
        </style>
        <p id=id>id beats type</p>
        <p id=both class=important>important beats id</p>
        <p id=order class=order>later wins</p>
        <p id=attr style="color: orange">the attribute beats selectors</p>
        <p id=attr-vs-important class=important style="color: orange">author important beats the attribute</p>
        <p id=attr-important class=important style="color: olive !important">attribute important beats author important</p>
        <h1 id=ua>author beats UA</h1>
        <h2 id=ua-only>UA alone</h2>"#,
    );
    assert_eq!(color(&page, "id"), "#008000ff");
    assert_eq!(color(&page, "both"), "#0000ffff");
    assert_eq!(color(&page, "order"), "#800080ff");
    assert_eq!(color(&page, "attr"), "#ffa500ff");
    assert_eq!(color(&page, "attr-vs-important"), "#0000ffff");
    assert_eq!(color(&page, "attr-important"), "#808000ff");
    assert_eq!(value(&page, "ua", "font-size").as_deref(), Some("10px"));
    assert_eq!(
        value(&page, "ua-only", "font-size").as_deref(),
        Some("24px")
    );
    assert_eq!(
        value(&page, "ua-only", "font-weight").as_deref(),
        Some("700")
    );
}

#[test]
fn the_ua_sheet_gives_html_its_default_look() {
    let page = load(
        r#"<body id=body><p id=p>text</p><ul><li id=li>item</li></ul>
        <pre id=pre>code</pre><a id=a href="/x">link</a><a id=no-href>anchor</a>
        <div id=hidden hidden>gone</div><b id=b>bold</b>"#,
    );
    assert_eq!(
        value(&page, "body", "margin").as_deref(),
        Some("8px 8px 8px 8px")
    );
    assert_eq!(
        value(&page, "p", "margin").as_deref(),
        Some("16px 0px 16px 0px")
    );
    assert_eq!(value(&page, "li", "display").as_deref(), Some("list-item"));
    assert_eq!(value(&page, "pre", "white-space").as_deref(), Some("pre"));
    assert_eq!(
        value(&page, "pre", "font-family").as_deref(),
        Some("monospace")
    );
    // AC-25: monospace text defaults to 13 px where other text gets 16 px.
    assert_eq!(value(&page, "pre", "font-size").as_deref(), Some("13px"));
    assert_eq!(color(&page, "a"), "#0000eeff");
    assert_eq!(
        value(&page, "a", "text-decoration-line").as_deref(),
        Some("underline")
    );
    assert_eq!(color(&page, "no-href"), "#000000ff", ":link needs an href");
    assert_eq!(value(&page, "hidden", "display").as_deref(), Some("none"));
    assert_eq!(value(&page, "b", "font-weight").as_deref(), Some("700"));
}

#[test]
fn every_selector_form_matches_and_unsupported_rules_drop_whole() {
    // AC-19
    let page = load(
        r#"<style>
            span { color: red }
            * + em { color: red }
            .c { color: rgb(0, 0, 1) }
            #i { color: rgb(0, 0, 2) }
            [data-a] { color: rgb(0, 0, 3) }
            [data-b="x y"] { color: rgb(0, 0, 4) }
            [data-c~=y] { color: rgb(0, 0, 5) }
            [lang|=en] { color: rgb(0, 0, 6) }
            [data-d^=ab] { color: rgb(0, 0, 7) }
            [data-e$=yz] { color: rgb(0, 0, 8) }
            [data-f*=mid] { color: rgb(0, 0, 9) }
            [data-g=CASE i] { color: rgb(0, 0, 10) }
            div.outer i { color: rgb(0, 0, 11) }
            div.outer > s { color: rgb(0, 0, 12) }
            h3 + q { color: rgb(0, 0, 13) }
            h3 ~ u { color: rgb(0, 0, 14) }
            ol > li:first-child { color: rgb(0, 0, 15) }
            ol > li:last-child { color: rgb(0, 0, 16) }
            li:not(.skip) { background-color: rgb(0, 0, 17) }
            :root { background-color: rgb(0, 0, 18) }
            a:visited { color: red }
            b:checked, b { color: red }
            b::before { color: red }
        </style>
        <span class=c id=class>.c</span>
        <span id=i>#i</span>
        <span id=attr data-a>[a]</span>
        <span id=eq data-b="x y">=</span>
        <span id=word data-c="x y z">~=</span>
        <span id=lang lang=en-GB>|=</span>
        <span id=prefix data-d=abc>^=</span>
        <span id=suffix data-e=xyz>$=</span>
        <span id=sub data-f=amidb>*=</span>
        <span id=case data-g=case>i</span>
        <div class=outer><p><i id=desc>desc</i><s id=not-child>s</s></p><s id=child>s</s></div>
        <h3>h</h3><q id=next>q</q><u id=later>u</u>
        <ol><li id=first>1</li><li id=middle class=skip>2</li><li id=last>3</li></ol>
        <b id=dropped>b</b>"#,
    );
    let blue = |n: u8| format!("#0000{n:02x}ff");
    for (id, n) in [
        ("class", 1),
        ("i", 2),
        ("attr", 3),
        ("eq", 4),
        ("word", 5),
        ("lang", 6),
        ("prefix", 7),
        ("suffix", 8),
        ("sub", 9),
        ("case", 10),
        ("desc", 11),
        ("child", 12),
        ("next", 13),
        ("later", 14),
        ("first", 15),
        ("last", 16),
    ] {
        assert_eq!(color(&page, id), blue(n), "#{id}");
    }
    assert_eq!(
        color(&page, "not-child"),
        "#000000ff",
        "s is not a child of div"
    );
    assert_eq!(color(&page, "middle"), "#000000ff");
    assert_eq!(value(&page, "first", "background-color"), Some(blue(17)));
    assert_eq!(
        value(&page, "middle", "background-color"),
        None,
        ":not(.skip)"
    );
    let root = page.main_frame().document.document_element().unwrap();
    let doc = &page.main_frame().document;
    let root_style = doc
        .node(root)
        .unwrap()
        .as_element()
        .unwrap()
        .style()
        .unwrap();
    assert!(
        root_style
            .dump()
            .contains(&format!("background-color: {}", blue(18)))
    );
    assert_eq!(
        color(&page, "dropped"),
        "#000000ff",
        "rules with :checked or ::before drop whole"
    );
}

#[test]
fn media_queries_apply_and_follow_resizes() {
    // AC-19
    let mut page = load(
        r#"<style>
            @media screen and (min-width: 600px) { #m { color: green } }
            @media (max-width: 599px) { #m { color: blue } }
            @media print { #m { color: red } }
            @media not screen { #m { color: red } }
            #vw { width: 10vw }
        </style>
        <p id=m>m</p><div id=vw>vw</div>"#,
    );
    assert_eq!(color(&page, "m"), "#008000ff");
    assert_eq!(value(&page, "vw", "width").as_deref(), Some("80px"));

    page.resize(
        SizeF {
            width: 400.0,
            height: 600.0,
        },
        1.0,
    );
    assert!(page.take_frame_request());
    page.begin_frame(16.0).unwrap();
    assert_eq!(color(&page, "m"), "#0000ffff", "evaluated again on resize");
    assert_eq!(value(&page, "vw", "width").as_deref(), Some("40px"));
}

#[test]
fn properties_compute_with_units_inheritance_and_shorthands() {
    // AC-20
    let page = load(
        r#"<style>
            html { font-size: 20px }
            #box {
                font-size: 10px;
                margin: 1em 2rem 10% auto;
                padding: 2px 4px;
                border: 3px dashed;
                border-left: thin solid rgba(255, 0, 0, 50%);
                border-radius: 4px 0;
                width: 50vw; height: 10vh;
                min-width: 5px; max-width: none; max-height: 100%;
                box-sizing: border-box;
                color: hsl(120, 100%, 25%);
                background: url(img/a.png) #ff0 no-repeat center;
                opacity: 150%;
                line-height: 150%;
                text-align: center;
                font: italic bolder 12px/2 "Iowan Old Style", serif;
                visibility: hidden;
                list-style: square inside;
                white-space: pre-line;
                text-decoration: underline dotted;
                bogus-property: 1;
                width: banana;
            }
        </style>
        <div id=box><span id=child>child</span></div>"#,
    );
    let get = |property: &str| value(&page, "box", property);
    assert_eq!(
        get("margin").as_deref(),
        Some("12px 40px 10% auto"),
        "em uses the element's font size"
    );
    assert_eq!(get("padding").as_deref(), Some("2px 4px 2px 4px"));
    assert_eq!(
        get("border-top").as_deref(),
        Some("3px dashed #008000ff"),
        "currentcolor"
    );
    assert_eq!(get("border-left").as_deref(), Some("1px solid #ff000080"));
    assert_eq!(get("border-radius").as_deref(), Some("4px 0px 4px 0px"));
    assert_eq!(
        get("width").as_deref(),
        Some("400px"),
        "the invalid width is dropped alone"
    );
    assert_eq!(get("height").as_deref(), Some("60px"));
    assert_eq!(get("min-width").as_deref(), Some("5px"));
    assert_eq!(get("max-height").as_deref(), Some("100%"));
    assert_eq!(get("box-sizing").as_deref(), Some("border-box"));
    assert_eq!(get("color").as_deref(), Some("#008000ff"));
    assert_eq!(get("background-color").as_deref(), Some("#ffff00ff"));
    assert_eq!(
        get("background-image").as_deref(),
        Some("url(http://test/dir/img/a.png)"),
        "resolved against the document"
    );
    assert_eq!(get("opacity"), None, "150% clamps to the initial 1");
    assert_eq!(get("font-size").as_deref(), Some("12px"));
    assert_eq!(
        get("font-weight").as_deref(),
        Some("700"),
        "bolder than 400"
    );
    assert_eq!(get("font-style").as_deref(), Some("italic"));
    assert_eq!(
        get("font-family").as_deref(),
        Some("Iowan Old Style, serif")
    );
    assert_eq!(
        get("line-height").as_deref(),
        Some("2"),
        "the font shorthand resets it"
    );
    assert_eq!(get("visibility").as_deref(), Some("hidden"));
    assert_eq!(get("list-style-type").as_deref(), Some("square"));
    assert_eq!(get("list-style-position").as_deref(), Some("inside"));
    assert_eq!(get("white-space").as_deref(), Some("pre-line"));
    assert_eq!(get("text-decoration-line").as_deref(), Some("underline"));

    // Inherited properties pass down; reset ones do not.
    let child = |property: &str| value(&page, "child", property);
    assert_eq!(child("color").as_deref(), Some("#008000ff"));
    assert_eq!(child("font-size").as_deref(), Some("12px"));
    assert_eq!(child("visibility").as_deref(), Some("hidden"));
    assert_eq!(child("margin"), None);
    assert_eq!(child("border-top"), None);
}

#[test]
fn var_substitutes_and_cycles_or_missing_variables_unset_the_property() {
    // AC-20
    let page = load(
        r#"<style>
            :root { --brand: #c00; --gap: 4px; --pad: var(--gap) }
            #user { color: var(--brand); margin: var(--gap) 0; padding-left: var(--pad) }
            #fallback { color: var(--missing, rgb(0, 0, 255)) }
            #missing { color: var(--missing) }
            #cycle { --a: var(--b); --b: var(--a); color: var(--a) }
            #self { --s: var(--s); color: var(--s, red) }
            #invalid { --bad: 12px; color: var(--bad) }
            #inherit-parent { color: rgb(1, 2, 3) }
            #override { --brand: rgb(0, 128, 0) }
        </style>
        <p id=user>u</p><p id=fallback>f</p>
        <div id=inherit-parent><p id=missing>m</p><p id=cycle>c</p><p id=invalid>i</p></div>
        <p id=self>s</p>
        <div id=override><p id=inner style="color: var(--brand)">o</p></div>"#,
    );
    assert_eq!(color(&page, "user"), "#cc0000ff");
    assert_eq!(
        value(&page, "user", "margin").as_deref(),
        Some("4px 0px 4px 0px")
    );
    assert_eq!(
        value(&page, "user", "padding").as_deref(),
        Some("0px 0px 0px 4px"),
        "custom properties resolve their own var()"
    );
    assert_eq!(color(&page, "fallback"), "#0000ffff");
    // `unset` on an inherited property inherits.
    assert_eq!(color(&page, "missing"), "#010203ff");
    assert_eq!(color(&page, "cycle"), "#010203ff");
    assert_eq!(
        color(&page, "invalid"),
        "#010203ff",
        "a value that fails to parse is unset"
    );
    assert_eq!(color(&page, "self"), "#ff0000ff", "a self cycle falls back");
    assert_eq!(
        color(&page, "inner"),
        "#008000ff",
        "custom properties inherit"
    );
}

#[test]
fn a_class_change_restyles_only_what_a_rule_can_reach() {
    // AC-21
    // No ids, so identical rows can share one style.
    let rows = "<li>row</li>".repeat(20);
    let mut page = load(&format!(
        r#"<style>
            .on {{ color: red }}
            .wrap .inner {{ color: blue }}
        </style>
        <ul id=list>{rows}</ul>
        <div id=wrap><p id=inner class=inner>x</p><p>y</p></div>"#
    ));
    let first = page.last_style_stats();
    assert!(first.shared > 0, "identical rows share styles: {first:?}");

    let doc = &mut page.main_frame_mut().document;
    let class = QualName::attribute(LocalName::from("class"));
    let list = doc.element_by_id(&Atom::from("list")).unwrap();
    let row = doc.children(list).nth(3).unwrap();
    doc.set_attribute(row, class.clone(), "unused").unwrap();
    page.end_task();
    page.begin_frame(16.0);
    assert_eq!(
        page.last_style_stats().styled,
        0,
        "no rule mentions .unused"
    );

    let doc = &mut page.main_frame_mut().document;
    doc.set_attribute(row, class.clone(), "on").unwrap();
    page.end_task();
    page.begin_frame(32.0).unwrap();
    assert_eq!(page.last_style_stats().styled, 1, "only the row itself");
    assert!(style_of(&page, row).contains("color: #ff0000ff"));

    let doc = &mut page.main_frame_mut().document;
    let wrap = doc.element_by_id(&Atom::from("wrap")).unwrap();
    doc.set_attribute(wrap, class, "wrap").unwrap();
    page.end_task();
    page.begin_frame(48.0).unwrap();
    assert_eq!(
        page.last_style_stats().styled,
        3,
        ".wrap is an ancestor compound: the subtree restyles"
    );
    assert_eq!(color(&page, "inner"), "#0000ffff");
}

#[test]
fn sibling_selectors_mark_the_elements_they_read() {
    // AC-21
    let mut page = load(
        r#"<style>li:last-child { color: red } h2 + p { color: blue }</style>
        <ul id=list><li id=a>a</li><li id=b>b</li></ul><p id=plain>p</p>"#,
    );
    let flagged = |page: &Page, id: &str| {
        let node = element(page, id);
        let doc = &page.main_frame().document;
        doc.node(node)
            .unwrap()
            .flags()
            .contains(NodeFlags::AFFECTED_BY_SIBLING_SELECTORS)
    };
    assert!(flagged(&page, "a") && flagged(&page, "b"));
    assert!(flagged(&page, "plain"), "tested against `h2 + p`");
    assert_eq!(color(&page, "b"), "#ff0000ff");

    // Appending a new last item restyles the old one.
    let doc = &mut page.main_frame_mut().document;
    let list = doc.element_by_id(&Atom::from("list")).unwrap();
    let li = doc.create_element(QualName::html(LocalName::from("li")));
    doc.append_child(list, li).unwrap();
    page.end_task();
    page.begin_frame(16.0).unwrap();
    assert_eq!(
        color(&page, "b"),
        "#000000ff",
        "b is no longer the last child"
    );
}

/// The fetch the page asked for, as `(fetch, url)`.
fn fetches(page: &mut Page) -> Vec<(FetchId, String)> {
    page.take_fetch_messages()
        .into_iter()
        .filter_map(|message| match message {
            ToBrowser::Fetch {
                fetch,
                url,
                destination: Destination::Style,
                navigation,
                ..
            } => {
                assert_eq!(navigation, NAV);
                Some((fetch, url))
            }
            _ => None,
        })
        .collect()
}

fn serve(page: &mut Page, fetch: FetchId, status: u16, css: &[u8]) {
    let headers = vec![("content-type".into(), "text/css".into())];
    page.fetch_response(NAV, fetch, status, &headers, "http://test/dir/sheet.css");
    page.fetch_chunk(NAV, fetch, css);
    page.fetch_done(NAV, fetch);
    page.end_task();
}

#[test]
fn linked_sheets_load_through_the_browser_and_cascade_in_tree_order() {
    // AC-18, AC-22
    let mut page = parsed(
        r#"<head>
            <link rel=stylesheet href="first.css">
            <style>#p { color: green } #q { color: green }</style>
            <link rel="alternate stylesheet" href="alt.css">
            <link rel=icon href="icon.png">
        </head>
        <body><p id=p>p</p><p id=q>q</p>
        <link rel=stylesheet href="/late.css"></body>"#,
    );
    let asked = fetches(&mut page);
    assert_eq!(
        asked,
        [
            (FetchId(1), "http://test/dir/first.css".to_owned()),
            (FetchId(2), "http://test/late.css".to_owned()),
        ]
    );
    assert!(
        fetches(&mut page).is_empty(),
        "each sheet is asked for once"
    );

    // The head's sheet holds the first paint; the body's does not.
    assert!(page.take_frame_request());
    assert!(page.begin_frame(0.0).is_none(), "held for first.css");
    assert!(page.take_frame_request(), "and asks again next frame");

    // The late sheet arrives first; it is last in tree order and wins.
    serve(&mut page, FetchId(2), 200, b"#q { color: blue }");
    assert!(page.begin_frame(16.0).is_none(), "still held for first.css");
    serve(
        &mut page,
        FetchId(1),
        200,
        b"#p { color: red } #q { color: red }",
    );
    page.begin_frame(32.0)
        .expect("painted once the head's sheet loaded");
    assert_eq!(color(&page, "p"), "#008000ff", "the <style> after it wins");
    assert_eq!(color(&page, "q"), "#0000ffff");
}

#[test]
fn a_failed_or_hung_sheet_stops_holding_the_paint() {
    // AC-22
    let mut page = parsed(
        r#"<link rel=stylesheet href="a.css"><link rel=stylesheet href="b.css"><p id=p>p</p>"#,
    );
    let asked = fetches(&mut page);
    serve(&mut page, asked[0].0, 404, b"#p { color: red }");
    assert_eq!(style(&page, "p"), "(unstyled)", "no frame has run yet");
    let styles = page.styles().unwrap();
    assert!(styles.render_blocked(Instant::now()), "b.css still loading");
    assert!(
        !styles
            .render_blocked(Instant::now() + page::RENDER_BLOCKING_CAP + Duration::from_millis(1)),
        "never more than 3 s after the commit"
    );
    page.fetch_failed(NAV, asked[1].0, &ipc::FetchError::TimedOut);
    page.end_task();
    page.begin_frame(0.0).expect("both sheets are done with");
    assert_eq!(
        color(&page, "p"),
        "#000000ff",
        "the 404 body is not applied"
    );
}

#[test]
fn removing_a_loading_link_cancels_its_fetch() {
    let mut page = parsed(r#"<link id=l rel=stylesheet href="a.css"><p>p</p>"#);
    let asked = fetches(&mut page);
    let doc = &mut page.main_frame_mut().document;
    let link = doc.element_by_id(&Atom::from("l")).unwrap();
    let parent = doc.node(link).unwrap().parent().unwrap();
    doc.remove_child(parent, link).unwrap();
    page.end_task();
    let cancels: Vec<_> = page
        .take_fetch_messages()
        .into_iter()
        .filter(|m| matches!(m, ToBrowser::CancelFetch { fetch, .. } if *fetch == asked[0].0))
        .collect();
    assert_eq!(cancels.len(), 1);
    assert!(
        page.begin_frame(0.0).is_some(),
        "nothing holds the paint now"
    );
}

#[test]
fn editing_a_style_element_restyles_the_page() {
    let mut page = load(r#"<style id=s>p { color: red }</style><p id=p>p</p>"#);
    assert_eq!(color(&page, "p"), "#ff0000ff");
    let doc = &mut page.main_frame_mut().document;
    let style = doc.element_by_id(&Atom::from("s")).unwrap();
    let text = doc.node(style).unwrap().first_child().unwrap();
    doc.set_text(text, "p { color: blue }").unwrap();
    page.end_task();
    assert!(page.take_frame_request());
    page.begin_frame(16.0).unwrap();
    assert_eq!(color(&page, "p"), "#0000ffff");
}
