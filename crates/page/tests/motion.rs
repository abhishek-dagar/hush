//! Spec 0010, AC-19 to AC-29 and AC-38, through the real page: hover and
//! class changes start transitions, `@keyframes` animate any property, the
//! transform matrix is composed about its origin, and motion events reach
//! script. Frame times are milliseconds, as `BeginFrame` sends them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use base::atoms::{Atom, LocalName, QualName};
use display_list::{Binding, DisplayList, PageId, PointF, SizeF, SpatialKind};
use ipc::{DocumentKind, FetchId, InputEvent, NavigationId};
use page::Page;

const NAV: NavigationId = NavigationId(1);
const VIEWPORT: SizeF = SizeF {
    width: 800.0,
    height: 600.0,
};

/// A committed page that parsed `html` and settled at frame time 0.
fn load(html: &str) -> Page {
    let mut page = Page::blank(PageId(1), VIEWPORT, 1.0, page::ua_stylesheet());
    page.navigate(NAV);
    assert!(page.commit_navigation(
        NAV,
        "http://test/motion.html",
        Some("utf-8".into()),
        &DocumentKind::Document
    ));
    page.end_task();
    page.fetch_chunk(NAV, FetchId::DOCUMENT, html.as_bytes());
    page.end_task();
    page.fetch_done(NAV, FetchId::DOCUMENT);
    page.end_task();
    assert!(page.begin_frame(0.0).is_some(), "no first frame");
    for _ in 0..10 {
        if !page.take_frame_request() || page.begin_frame(0.0).is_none() {
            break;
        }
    }
    page
}

fn element(page: &Page, id: &str) -> dom::NodeId {
    page.main_frame()
        .document
        .element_by_id(&Atom::from(id))
        .unwrap_or_else(|| panic!("no #{id}"))
}

/// The border box of `#id` in page space, as drawn.
fn rect(page: &Page, id: &str) -> display_list::RectF {
    let node = element(page, id);
    page.element_boxes()
        .into_iter()
        .find_map(|(n, r)| (n == node).then_some(r))
        .unwrap_or_else(|| panic!("#{id} has no box"))
}

/// Every reference frame matrix in the list, in spatial tree order.
fn frames(list: &DisplayList) -> Vec<[f32; 16]> {
    list.spatial
        .iter()
        .filter_map(|node| match &node.kind {
            SpatialKind::ReferenceFrame { transform } => Some(match transform {
                Binding::Static(m) | Binding::Bound { value: m, .. } => *m,
            }),
            _ => None,
        })
        .collect()
}

/// The one reference frame matrix in `list`.
fn frame(list: &DisplayList) -> [f32; 16] {
    let all = frames(list);
    assert_eq!(all.len(), 1, "expected one reference frame, got {all:?}");
    all[0]
}

fn close(actual: f32, expected: f32, what: &str) {
    assert!(
        (actual - expected).abs() < 0.01,
        "{what}: {actual}, expected {expected}"
    );
}

fn hover(page: &mut Page, x: f32, y: f32, at: f64) -> Option<DisplayList> {
    page.input(InputEvent::PointerMove { x, y });
    page.begin_frame(at)
}

fn set_class(page: &mut Page, id: &str, class: &str) {
    let node = element(page, id);
    let name = QualName::attribute(LocalName::from("class"));
    page.main_frame_mut()
        .document
        .set_attribute(node, name, class)
        .unwrap();
}

const SCALE_ON_HOVER: &str = "<style>
    #a { position: absolute; left: 0; top: 0; width: 100px; height: 100px;
         transform: scale(1); transform-origin: 0 0; transition: transform 1s linear }
    #a:hover { transform: scale(2) }
    </style><div id=a></div>";

/// AC-23, AC-26: hovering runs a `transform` list transition, starting
/// from the old value on the frame the hover lands.
#[test]
fn hover_transitions_a_transform_list() {
    let mut page = load(SCALE_ON_HOVER);
    let first = hover(&mut page, 10.0, 10.0, 1000.0).expect("hover frame");
    close(frame(&first)[0], 1.0, "scale on the hover frame");
    let quarter = page.begin_frame(1250.0).expect("frame at 250ms");
    close(frame(&quarter)[0], 1.25, "scale at 250ms");
    let half = page.begin_frame(1500.0).expect("frame at 500ms");
    close(frame(&half)[0], 1.5, "scale at 500ms");
    let end = page.begin_frame(2000.0).expect("frame at 1s");
    close(frame(&end)[0], 2.0, "scale at the end");
    // Settled: nothing runs any more.
    let _ = page.begin_frame(2100.0);
    assert!(!page.take_frame_request(), "still asking for frames");
}

/// AC-23: the separate `scale` property shows its start value on the
/// frame its transition starts.
#[test]
fn a_scale_transition_starts_from_the_old_value() {
    let mut page = load(
        "<style>
        #a { position: absolute; left: 0; top: 0; width: 100px; height: 100px;
             scale: 1; transform-origin: 0 0; transition: scale 1s linear }
        #a:hover { scale: 2 }
        </style><div id=a></div>",
    );
    let first = hover(&mut page, 10.0, 10.0, 1000.0).expect("hover frame");
    close(frame(&first)[0], 1.0, "scale on the hover frame");
    let half = page.begin_frame(1500.0).expect("frame at 500ms");
    close(frame(&half)[0], 1.5, "scale at 500ms");
}

/// AC-23, AC-26: a class change transitions `width`, a length that
/// needs layout.
#[test]
fn a_class_change_transitions_width() {
    let mut page = load(
        "<style>
        #w { width: 100px; height: 10px; transition: width 1s linear }
        #w.wide { width: 200px }
        </style><div id=w></div>",
    );
    close(rect(&page, "w").size.width, 100.0, "width before");
    set_class(&mut page, "w", "wide");
    page.begin_frame(1000.0);
    close(
        rect(&page, "w").size.width,
        100.0,
        "width on the change frame",
    );
    page.begin_frame(1250.0);
    close(rect(&page, "w").size.width, 125.0, "width at 250ms");
    page.begin_frame(2000.0);
    close(rect(&page, "w").size.width, 200.0, "width at the end");
}

/// AC-26: a color transitions in sRGB, and a discrete property flips at
/// 50%.
#[test]
fn colors_interpolate_and_discrete_properties_flip_halfway() {
    let mut page = load(
        "<style>
        #c { width: 10px; height: 10px; background-color: rgb(0, 0, 0);
             text-align: left; transition: all 1s linear }
        #c.on { background-color: rgb(200, 100, 0); text-align: right }
        </style><div id=c></div>",
    );
    set_class(&mut page, "c", "on");
    page.begin_frame(1000.0);
    page.begin_frame(1250.0);
    let node = element(&page, "c");
    let style = |page: &Page| {
        page.main_frame()
            .document
            .node(node)
            .and_then(|n| n.as_element())
            .and_then(|e| e.style())
            .cloned()
            .unwrap()
    };
    let color = style(&page).background.color;
    close(color.r * 255.0, 50.0, "red at 250ms");
    close(color.g * 255.0, 25.0, "green at 250ms");
    assert_eq!(style(&page).inherited_text.text_align, css::TextAlign::Left);
    page.begin_frame(1600.0);
    assert_eq!(
        style(&page).inherited_text.text_align,
        css::TextAlign::Right
    );
}

/// AC-24, AC-26: `@keyframes` on `width` animates the laid out box.
#[test]
fn keyframes_animate_width() {
    let mut page = load(
        "<style>
        @keyframes grow { from { width: 100px } to { width: 200px } }
        #k { height: 10px; animation: grow 1s linear }
        </style><div id=k></div>",
    );
    page.begin_frame(250.0);
    close(rect(&page, "k").size.width, 125.0, "width at 250ms");
    page.begin_frame(500.0);
    close(rect(&page, "k").size.width, 150.0, "width at 500ms");
}

/// AC-24, AC-27: a paused animation holds its value and asks for no
/// frames.
#[test]
fn a_paused_animation_does_not_advance() {
    let mut page = load(
        "<style>
        @keyframes spin { from { transform: translateX(0px) } to { transform: translateX(100px) } }
        #p { position: absolute; left: 0; top: 0; width: 10px; height: 10px;
             animation: spin 1s linear infinite; animation-play-state: paused }
        </style><div id=p></div>",
    );
    let at = |page: &mut Page, t: f64| {
        page.invalidate();
        page.begin_frame(t).map(|list| frame(&list)[12])
    };
    let first = at(&mut page, 100.0);
    let later = at(&mut page, 600.0);
    assert_eq!(first, later, "a paused animation moved");
    let _ = page.begin_frame(700.0);
    assert!(
        !page.take_frame_request(),
        "a paused animation asks for frames"
    );
}

/// AC-29: transition and animation events reach listeners on the element,
/// in order, with their fields.
#[test]
fn motion_events_fire_on_the_element() {
    let mut page = load(
        "<style>
        #e { width: 10px; height: 10px; transition: width 0.5s linear }
        #e.wide { width: 20px }
        @keyframes blink { from { opacity: 0 } to { opacity: 1 } }
        #f { width: 10px; height: 10px; animation: blink 0.2s linear 2 }
        </style><div id=e></div><div id=f></div>
        <script>
        var log = [];
        var e = document.getElementById('e');
        var f = document.getElementById('f');
        ['transitionrun', 'transitionstart', 'transitionend', 'transitioncancel'].forEach(function (t) {
          e.addEventListener(t, function (ev) {
            log.push(ev.type + ':' + ev.propertyName + ':' + ev.elapsedTime + ':' + ev.pseudoElement
              + ':' + (ev instanceof TransitionEvent));
            document.documentElement.setAttribute('data-log', log.join(','));
          });
        });
        ['animationstart', 'animationiteration', 'animationend'].forEach(function (t) {
          f.addEventListener(t, function (ev) {
            log.push(ev.type + ':' + ev.animationName + ':' + ev.elapsedTime
              + ':' + (ev instanceof AnimationEvent));
            document.documentElement.setAttribute('data-log', log.join(','));
          });
        });
        </script>",
    );
    set_class(&mut page, "e", "wide");
    for t in [0.0, 100.0, 250.0, 450.0, 600.0] {
        page.invalidate();
        page.begin_frame(t);
        page.end_task();
    }
    let doc = &page.main_frame().document;
    let html = doc.document_element().unwrap();
    let log = doc
        .node(html)
        .and_then(|n| n.as_element())
        .and_then(|e| e.attribute(&QualName::attribute(LocalName::from("data-log"))))
        .unwrap_or_default()
        .to_owned();
    let entries: Vec<&str> = log.split(',').collect();
    let of = |prefix: &str| -> Vec<&str> {
        entries
            .iter()
            .copied()
            .filter(|e| e.starts_with(prefix))
            .collect()
    };
    assert_eq!(
        of("transition"),
        [
            "transitionrun:width:0::true",
            "transitionstart:width:0::true",
            "transitionend:width:0.5::true"
        ],
        "log: {log}"
    );
    assert_eq!(
        of("animation"),
        [
            "animationstart:blink:0:true",
            "animationiteration:blink:0.2:true",
            "animationend:blink:0.4:true"
        ],
        "log: {log}"
    );
}

/// AC-19, AC-20: `transform-origin` defaults to the center, so a rotated
/// box turns about it.
#[test]
fn rotation_is_about_the_center_by_default() {
    let mut page = load(
        "<style>#r { position: absolute; left: 100px; top: 100px; width: 100px;
         height: 100px; transform: rotate(45deg) }</style><div id=r></div>",
    );
    page.invalidate();
    let list = page.begin_frame(0.0).expect("frame");
    let m = frame(&list);
    close(m[12], 150.0, "tx");
    close(m[13], 79.29, "ty");
}

/// AC-21: hit testing follows the drawn, rotated box.
#[test]
fn hit_testing_follows_the_rotation() {
    let page = load(
        "<style>#r { position: absolute; left: 100px; top: 100px; width: 100px;
         height: 100px; transform: rotate(45deg) }</style><div id=r></div>",
    );
    let r = element(&page, "r");
    let hit = |x, y| page.hit_test(PointF { x, y }).map(|(h, _)| h.node);
    assert_eq!(hit(150.0, 82.0), Some(r), "the drawn tip");
    assert_ne!(hit(103.0, 103.0), Some(r), "the corner the rotation moved");
}

/// AC-19: the separate properties apply translate, rotate, scale, then
/// `transform`, all about the origin.
#[test]
fn separate_properties_compose_in_order() {
    let mut page = load(
        "<style>#s { position: absolute; left: 600px; top: 100px; width: 100px;
         height: 100px; translate: 50px 0; rotate: 90deg; scale: 2;
         transform: translateX(10px) }</style><div id=s></div>",
    );
    page.invalidate();
    let m = frame(&page.begin_frame(0.0).expect("frame"));
    close(m[12], 800.0, "tx");
    close(m[13], 70.0, "ty");
}

/// AC-19: a parent's `perspective` projects the child's 3D rotation.
#[test]
fn parent_perspective_projects_the_child() {
    let mut page = load(
        "<style>#p { perspective: 100px; width: 100px; height: 100px }
         #c { width: 100px; height: 100px; transform: rotateY(60deg) }</style>
         <div id=p><div id=c></div></div>",
    );
    page.invalidate();
    let m = frame(&page.begin_frame(0.0).expect("frame"));
    // w = 1 - z / d, and rotateY(60deg) sends x to z = -sin(60deg) * x,
    // z to cos(60deg) * z.
    let tight = |actual: f32, expected: f32, what: &str| {
        assert!(
            (actual - expected).abs() < 1e-5,
            "{what}: {actual}, expected {expected}"
        );
    };
    tight(m[3], 60f32.to_radians().sin() / 100.0, "w per x");
    tight(m[11], -60f32.to_radians().cos() / 100.0, "w per z");
}

/// AC-22: a transformed child grows its scroll container's scrollable
/// overflow.
#[test]
fn transformed_bounds_count_toward_scrollable_overflow() {
    let mut page = load(
        "<style>#o { overflow: auto; width: 100px; height: 100px }
         #c { width: 50px; height: 50px; transform: scale(3); transform-origin: 0 0 }</style>
         <div id=o><div id=c></div></div>",
    );
    page.invalidate();
    let list = page.begin_frame(0.0).expect("frame");
    let size = list
        .spatial
        .iter()
        .find_map(|node| match &node.kind {
            SpatialKind::ScrollFrame {
                frame_rect,
                content_size,
                ..
            } if frame_rect.size.width == 100.0 => Some(*content_size),
            _ => None,
        })
        .expect("the scroller's frame");
    close(size.width, 150.0, "content width");
    close(size.height, 150.0, "content height");
}

/// AC-38: `@keyframes` with more than 1,000 keyframes is ignored.
#[test]
fn oversized_keyframes_are_ignored() {
    let mut body = String::new();
    for i in 0..1001 {
        let pct = i as f32 * 100.0 / 1000.0;
        body.push_str(&format!("{pct}% {{ transform: translateX({i}px) }} "));
    }
    let mut page = load(&format!(
        "<style>@keyframes big {{ {body} }}
         #b {{ position: absolute; left: 0; top: 0; width: 10px; height: 10px;
               animation: big 1s linear }}</style><div id=b></div>"
    ));
    page.invalidate();
    let list = page.begin_frame(500.0).expect("frame");
    assert!(frames(&list).is_empty(), "the rule ran");
}
