//! Content side hit testing over the fragment tree (spec 0002, Hit testing,
//! AC-14). The browser process only picks which scroll frame a wheel event
//! moves; finding the DOM node under a point happens here.

use std::rc::Rc;

use base::{Au, Point, Rect, Size, mat4_identity, mat4_inverse, mat4_mul, mat4_translate};
use dom::NodeId;

use crate::stacking::{self, Placed, Step, Walker};
use crate::{BoxFragment, BoxTree, FragmentTree, border_widths, corner_radii, inner_radii};

/// Applies matrix `m` to `point` (with perspective divide), or `None` when
/// `w` is zero (the point is behind the viewer or the matrix is singular).
fn transform_point(m: &[f32; 16], point: Point) -> Option<Point> {
    let w = m[3] * point.x.to_f32_px() + m[7] * point.y.to_f32_px() + m[15];
    if w == 0.0 {
        return None;
    }
    let x = (m[0] * point.x.to_f32_px() + m[4] * point.y.to_f32_px() + m[12]) / w;
    let y = (m[1] * point.x.to_f32_px() + m[5] * point.y.to_f32_px() + m[13]) / w;
    Some(Point::new(Au::from_px(x), Au::from_px(y)))
}

/// The node under a point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HitTestResult {
    /// The deepest node whose box contains the point.
    pub node: NodeId,
    /// The point relative to that node's border box top left corner.
    pub point: Point,
    /// The topmost thing under the point is text whose `user-select` is not
    /// `none`, which `cursor: auto` shows as a text beam (spec 0006, AC-18).
    pub selectable_text: bool,
}

/// Finds the topmost node whose box contains `point` (page coordinates),
/// in paint order (spec 0006, AC-6), or `None` for a point outside the
/// viewport or over no box.
///
/// `scroll_offset` gives a scroll container node's current offset, from the
/// page's scroll table. Content inside a scroll container moves by it, and
/// a box that clips its overflow lets its content be hit only inside its
/// padding box, rounded by its `border-radius` (AC-7). The whole document
/// sits in the root viewport's scroll frame, which takes the root element's
/// node and its `overflow` (spec 0004, child 0005, Root scroll frame), as
/// paint draws it. Text, outlines and anonymous boxes are never the answer:
/// the hit goes to the nearest box with a node below them.
pub fn hit_test(
    tree: &FragmentTree,
    boxes: &BoxTree,
    point: Point,
    scroll_offset: impl Fn(NodeId) -> Point,
) -> Option<HitTestResult> {
    if !contains(Rect::new(Point::default(), tree.viewport), point) {
        return None;
    }
    let (origin, root) = tree.root.as_ref()?;
    let viewport = root
        .as_box()
        .and_then(|root| root.node)
        .map_or(Point::default(), &scroll_offset);
    let origin = Point::new(origin.x - viewport.x, origin.y - viewport.y);
    let mut walker = HitWalker {
        boxes,
        point,
        scroll_offset,
    };
    let root = Placed {
        origin,
        fragment: root,
        state: (None, None),
    };
    let steps = stacking::paint_order(boxes, root, &mut walker);
    // The last box in paint order that contains the point is on top. Text
    // painted above it is never the answer, but tells the cursor.
    let mut selectable_text = None;
    steps.iter().rev().find_map(|step| match step {
        Step::Box {
            fragment,
            origin,
            state,
        } => {
            let node = fragment.node?;
            let style = boxes.get(fragment.box_id).map(|b| &b.style);
            let (clip, _forward) = state;
            // `origin` is this box's border box corner in page space (layout
            // already applied scroll offsets). `m` maps box-local to
            // border-box-local (transformed about the origin); `tm` maps
            // box-local to page space. Undo it to test the page point against
            // the border box in box-local space. A box whose matrix cannot be
            // inverted (for example `scale(0)`) is never hit (spec 0010,
            // AC-21).
            let m = style
                .and_then(|s| s.transform.matrix())
                .unwrap_or(mat4_identity());
            let tm = mat4_mul(
                &m,
                &mat4_translate(origin.x.to_f32_px(), origin.y.to_f32_px(), 0.0),
            );
            // A box whose matrix cannot be inverted (for example `scale(0)`)
            // is never hit (spec 0010, AC-21).
            let local = mat4_inverse(&tm).and_then(|inv| transform_point(&inv, point))?;
            let border_box = Rect::new(Point::default(), fragment.size);
            // Keep the page-space clip check (the pre-transform behavior) so
            // scrolling and overflow clips keep working (spec 0009, AC-7).
            let inside =
                contains(border_box, local) && clip.as_ref().is_none_or(|c| c.contains(point));
            inside.then(|| HitTestResult {
                node,
                point: local,
                selectable_text: selectable_text.unwrap_or(false),
            })
        }
        Step::Text {
            fragment,
            origin,
            state,
        } if selectable_text.is_none() => {
            let (clip, forward) = state;
            // The text run sits at `origin` in parent space; measure the page
            // point from it in box-local space (spec 0010, AC-21).
            let m = fragment.style.transform.matrix().unwrap_or(mat4_identity());
            let tm = mat4_mul(
                &m,
                &mat4_translate(origin.x.to_f32_px(), origin.y.to_f32_px(), 0.0),
            );
            let local_box = forward
                .as_ref()
                .and_then(mat4_inverse)
                .and_then(|inv| transform_point(&inv, point))
                .unwrap_or(point);
            let text_box = mat4_inverse(&tm)
                .and_then(|inv| transform_point(&inv, *origin))
                .unwrap_or_default();
            let local = Point::new(local_box.x - text_box.x, local_box.y - text_box.y);
            let run = Rect::new(Point::default(), fragment.size);
            let p_in_parent = transform_point(&tm, local_box).unwrap_or(point);
            if contains(run, local) && clip.as_ref().is_none_or(|c| c.contains(p_in_parent)) {
                let user_select = fragment.style.inherited_box.user_select;
                selectable_text = Some(user_select != css::UserSelect::None);
            }
            None
        }
        _ => None,
    })
}

/// A clip a box sets for its content: its padding box with rounded corners,
/// inside the clips around it.
#[derive(Debug)]
struct Clip {
    rect: Rect,
    /// Corner radii in CSS px, as [`corner_radii`] orders them, already
    /// shrunk to fit.
    radii: Option<[f32; 8]>,
    parent: Option<Rc<Clip>>,
}

impl Clip {
    fn contains(&self, point: Point) -> bool {
        let mut clip = Some(self);
        while let Some(current) = clip {
            if !rounded_contains(current.rect, current.radii, point) {
                return false;
            }
            clip = current.parent.as_deref();
        }
        true
    }
}

/// Tracks the clips and scroll offsets of a hit test walk.
struct HitWalker<'b, F> {
    boxes: &'b BoxTree,
    point: Point,
    scroll_offset: F,
}

impl<'a, F: Fn(NodeId) -> Point> Walker<'a> for HitWalker<'_, F> {
    /// The clip in force for the subtree, and the matrix mapping the space
    /// that children origins are expressed in back to page space (`None` is
    /// the identity).
    type State = (Option<Rc<Clip>>, Option<[f32; 16]>);

    fn children(
        &mut self,
        fragment: &'a BoxFragment,
        origin: Point,
        state: &Self::State,
    ) -> Option<(Point, Self::State)> {
        let style = self.boxes.get(fragment.box_id).map(|b| &b.style);
        let (clip, forward) = state;
        let mut child_clip = clip.clone();
        let mut child_origin = origin;
        // `overflow` does not apply to an inline box's pieces, and the root
        // element's is the viewport's.
        let is_root = self.boxes.root() == Some(fragment.box_id);
        if let Some(style) = style
            && fragment.inline_edges.is_none()
            && style.display_box.clips_overflow()
            && !is_root
        {
            let border = border_widths(style);
            let padding_box = Rect::new(
                Point::new(origin.x + border.left, origin.y + border.top),
                Size::new(
                    fragment.size.width - border.horizontal(),
                    fragment.size.height - border.vertical(),
                ),
            );
            let radii = corner_radii(style, fragment.size, None)
                .map(|outer| inner_radii(outer, border))
                .map(|inner| fit_radii(padding_box.size, inner));
            child_clip = Some(Rc::new(Clip {
                rect: padding_box,
                radii,
                parent: clip.clone(),
            }));
        }
        if fragment.is_scroll_container
            && !is_root
            && let Some(node) = fragment.node
        {
            let offset = (self.scroll_offset)(node);
            child_origin = Point::new(origin.x - offset.x, origin.y - offset.y);
        }
        // Content outside the clip cannot be hit, so skip it entirely. The
        // clip rect lives in the same page space as `origin` (layout already
        // applied scroll offsets), so compare the page point directly.
        if child_clip.as_ref().is_some_and(|c| !c.contains(self.point)) {
            return None;
        }
        // A transformed box maps its children's space through its matrix,
        // about its border box (spec 0010, AC-20, AC-21). `child_forward`
        // maps box-local space to page space: apply this box's matrix, then
        // the ancestor chain.
        let m = style
            .and_then(|s| s.transform.matrix())
            .unwrap_or(mat4_identity());
        // `m` maps box-local to border-box-local; translate by `origin` to
        // place it in parent space (`tm`), then the ancestor chain (`forward`)
        // reaches page space: `forward ∘ tm`.
        let tm = mat4_mul(
            &m,
            &mat4_translate(origin.x.to_f32_px(), origin.y.to_f32_px(), 0.0),
        );
        let child_forward = forward.map(|f| mat4_mul(&f, &tm)).or(Some(tm));
        Some((child_origin, (child_clip, child_forward)))
    }
}

/// `radii` shrunk by one factor until adjacent corners fit each side of a
/// box `size` (CSS Backgrounds 3, section 5.5).
fn fit_radii(size: Size, radii: [f32; 8]) -> [f32; 8] {
    let [tl_x, tl_y, tr_x, tr_y, br_x, br_y, bl_x, bl_y] = radii;
    let (width, height) = (size.width.to_f32_px(), size.height.to_f32_px());
    let factor = [
        (width, tl_x + tr_x),
        (height, tr_y + br_y),
        (width, bl_x + br_x),
        (height, tl_y + bl_y),
    ]
    .into_iter()
    .filter(|&(_, sum)| sum > 0.0)
    .map(|(length, sum)| length / sum)
    .fold(1.0f32, f32::min);
    radii.map(|r| r * factor)
}

fn contains(rect: Rect, point: Point) -> bool {
    point.x >= rect.origin.x
        && point.y >= rect.origin.y
        && point.x < rect.origin.x + rect.size.width
        && point.y < rect.origin.y + rect.size.height
}

/// Whether `point` is inside `rect` with its corners rounded by `radii`.
fn rounded_contains(rect: Rect, radii: Option<[f32; 8]>, point: Point) -> bool {
    if !contains(rect, point) {
        return false;
    }
    let Some([tl_x, tl_y, tr_x, tr_y, br_x, br_y, bl_x, bl_y]) = radii else {
        return true;
    };
    let px = |au: Au| au.to_f32_px();
    let (x, y) = (px(point.x - rect.origin.x), px(point.y - rect.origin.y));
    let (width, height) = (px(rect.size.width), px(rect.size.height));
    // Each corner's ellipse, by its centre and radii, and whether the
    // point is in that corner's square.
    let corners = [
        (tl_x, tl_y, x < tl_x && y < tl_y),
        (tr_x, tr_y, x > width - tr_x && y < tr_y),
        (br_x, br_y, x > width - br_x && y > height - br_y),
        (bl_x, bl_y, x < bl_x && y > height - bl_y),
    ];
    let centres = [
        (tl_x, tl_y),
        (width - tr_x, tr_y),
        (width - br_x, height - br_y),
        (bl_x, height - bl_y),
    ];
    corners
        .into_iter()
        .zip(centres)
        .all(|((rx, ry, in_corner), (cx, cy))| {
            if !in_corner || rx <= 0.0 || ry <= 0.0 {
                return true;
            }
            let (dx, dy) = ((x - cx) / rx, (y - cy) / ry);
            dx * dx + dy * dy <= 1.0
        })
}

#[cfg(test)]
mod tests {
    use base::atoms::QualName;
    use base::{Au, local_name};
    use css::{ComputedStyle, Display, Length, Margin, Overflow};
    use dom::Document;

    use super::*;
    use crate::layout;

    fn px(px: f32) -> Au {
        Au::from_px(px)
    }

    fn at(x: f32, y: f32) -> Point {
        Point::new(px(x), px(y))
    }

    fn add(doc: &mut Document, parent: NodeId, height: Option<f32>, scrolls: bool) -> NodeId {
        let mut style = ComputedStyle::default();
        Rc::make_mut(&mut style.display_box).display = Display::Block;
        if scrolls {
            Rc::make_mut(&mut style.display_box).overflow_y = Overflow::Auto;
        }
        Rc::make_mut(&mut style.size).height = height.map(Length::px);
        style.margin = Rc::new(Margin::all(Length::ZERO));
        let id = doc.create_element(QualName::html(local_name!("div")));
        doc.append_child(parent, id).unwrap();
        doc.set_style(id, Some(Rc::new(style))).unwrap();
        id
    }

    /// `html` > a 100px scroller holding two 80px rows, then a 50px footer.
    struct Page {
        doc: Document,
        boxes: BoxTree,
        tree: FragmentTree,
        html: NodeId,
        scroller: NodeId,
        rows: [NodeId; 2],
        footer: NodeId,
    }

    fn page() -> Page {
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let html = add(&mut doc, root, None, false);
        let scroller = add(&mut doc, html, Some(100.0), true);
        let rows = [0, 1].map(|_| add(&mut doc, scroller, Some(80.0), false));
        let footer = add(&mut doc, html, Some(50.0), false);
        let mut boxes = BoxTree::build(&mut doc);
        let (tree, _) = layout(
            &mut boxes,
            Size::new(px(200.0), px(400.0)),
            &mut text::FontContext::new(),
            &text::WebFontLayer::default(),
        );
        Page {
            doc,
            boxes,
            tree,
            html,
            scroller,
            rows,
            footer,
        }
    }

    #[test]
    fn the_deepest_node_under_the_point_wins() {
        // AC-14
        let p = page();
        let unscrolled = |_: NodeId| Point::default();
        let hit = hit_test(&p.tree, &p.boxes, at(10.0, 90.0), unscrolled).unwrap();
        assert_eq!(hit.node, p.rows[1]);
        assert_eq!(hit.point, at(10.0, 10.0), "relative to the row");
        let footer = hit_test(&p.tree, &p.boxes, at(10.0, 120.0), unscrolled);
        assert_eq!(footer.map(|h| h.node), Some(p.footer));
        let below = hit_test(&p.tree, &p.boxes, at(10.0, 300.0), unscrolled);
        assert_eq!(below, None, "over no box");
        let _ = (&p.doc, p.html);
    }

    #[test]
    fn scroll_offsets_move_the_content_under_the_point() {
        // AC-14
        let p = page();
        let scroller = p.scroller;
        let scrolled = |node: NodeId| {
            if node == scroller {
                at(0.0, 60.0)
            } else {
                Point::default()
            }
        };
        let hit = hit_test(&p.tree, &p.boxes, at(10.0, 10.0), scrolled).unwrap();
        assert_eq!(hit.node, p.rows[0], "row 0 now spans -60 to 20");
        let hit = hit_test(&p.tree, &p.boxes, at(10.0, 30.0), scrolled).unwrap();
        assert_eq!(hit.node, p.rows[1]);
        // Row 1 hangs below the scroller, but that part is clipped away.
        let hit = hit_test(&p.tree, &p.boxes, at(10.0, 110.0), |_| Point::default());
        assert_eq!(hit.map(|h| h.node), Some(p.footer));
    }

    #[test]
    fn the_viewport_offset_moves_the_whole_document() {
        // AC-30: the root element's node names the viewport's frame.
        let p = page();
        let html = p.html;
        let scrolled = |node: NodeId| {
            if node == html {
                at(0.0, 100.0)
            } else {
                Point::default()
            }
        };
        // The footer starts at 100 in the document: at the top once the
        // viewport scrolled 100 down.
        let hit = hit_test(&p.tree, &p.boxes, at(10.0, 10.0), scrolled).unwrap();
        assert_eq!(hit.node, p.footer);
        assert_eq!(hit.point, at(10.0, 10.0));
    }

    #[test]
    fn points_outside_the_page_hit_nothing() {
        // AC-14
        let p = page();
        for point in [
            at(-1.0, 10.0),
            at(10.0, -1.0),
            at(200.0, 10.0),
            at(10.0, 400.0),
        ] {
            assert_eq!(
                hit_test(&p.tree, &p.boxes, point, |_| Point::default()),
                None
            );
        }
    }

    #[test]
    fn a_scroll_container_covers_its_children_plus_end_padding() {
        let p = page();
        let (_, root) = p.tree.root.as_ref().unwrap();
        let html = root.as_box().unwrap();
        let scroller = html.children[0].1.as_box().unwrap();
        assert!(scroller.is_scroll_container);
        assert_eq!(scroller.size.height, px(100.0));
        assert_eq!(scroller.scrollable_overflow.size.height, px(160.0));
        // html is not a scroll container and only sees the scroller's box.
        assert_eq!(html.scrollable_overflow.size.height, px(150.0));
    }

    /// Adds a `div` under `parent` styled by `set` on top of a zero margin
    /// block.
    fn styled(doc: &mut Document, parent: NodeId, set: impl FnOnce(&mut ComputedStyle)) -> NodeId {
        let mut style = ComputedStyle::default();
        Rc::make_mut(&mut style.display_box).display = Display::Block;
        style.margin = Rc::new(Margin::all(Length::ZERO));
        set(&mut style);
        let id = doc.create_element(QualName::html(local_name!("div")));
        doc.append_child(parent, id).unwrap();
        doc.set_style(id, Some(Rc::new(style))).unwrap();
        id
    }

    fn size(style: &mut ComputedStyle, width: f32, height: f32) {
        let size = Rc::make_mut(&mut style.size);
        size.width = Some(Length::px(width));
        size.height = Some(Length::px(height));
    }

    fn hit(doc: &mut Document, point: Point) -> Option<NodeId> {
        let mut boxes = BoxTree::build(doc);
        let (tree, _) = layout(
            &mut boxes,
            Size::new(px(200.0), px(400.0)),
            &mut text::FontContext::new(),
            &text::WebFontLayer::default(),
        );
        hit_test(&tree, &boxes, point, |_| Point::default()).map(|h| h.node)
    }

    #[test]
    fn the_hit_follows_paint_order_by_z_index() {
        // Spec 0006, Critical test scenarios: Stacking (AC-6).
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let html = styled(&mut doc, root, |_| {});
        let popover = styled(&mut doc, html, |s| {
            size(s, 50.0, 50.0);
            Rc::make_mut(&mut s.display_box).position = css::Position::Absolute;
            Rc::make_mut(&mut s.effects).z_index = Some(1);
        });
        let later = styled(&mut doc, html, |s| size(s, 100.0, 50.0));
        let under = styled(&mut doc, later, |s| {
            size(s, 100.0, 20.0);
            Rc::make_mut(&mut s.display_box).position = css::Position::Relative;
            Rc::make_mut(&mut s.effects).z_index = Some(-1);
        });
        assert_eq!(
            hit(&mut doc, at(10.0, 10.0)),
            Some(popover),
            "over the later sibling"
        );
        assert_eq!(
            hit(&mut doc, at(60.0, 10.0)),
            Some(later),
            "a block over negative z"
        );
        let _ = under;
    }

    #[test]
    fn rounded_clips_cut_the_corners_and_outlines_are_never_hit() {
        // Spec 0006, AC-7 and AC-8.
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let html = styled(&mut doc, root, |_| {});
        let clipping = styled(&mut doc, html, |s| {
            size(s, 50.0, 50.0);
            Rc::make_mut(&mut s.display_box).overflow_x = Overflow::Hidden;
            Rc::make_mut(&mut s.display_box).overflow_y = Overflow::Hidden;
            Rc::make_mut(&mut s.border).radius = css::BorderRadius {
                top_left: Length::px(20.0),
                top_right: Length::px(20.0),
                bottom_right: Length::px(20.0),
                bottom_left: Length::px(20.0),
            };
            s.outline = Rc::new(css::Outline {
                width: px(5.0),
                style: css::OutlineStyle::Solid,
                color: css::Color::BLACK,
                offset: px(10.0),
            });
        });
        let content = styled(&mut doc, clipping, |s| size(s, 50.0, 50.0));
        assert_eq!(hit(&mut doc, at(25.0, 25.0)), Some(content));
        // In the corner, outside the 20px curve: only the clipping box's
        // own square border box is there.
        assert_eq!(hit(&mut doc, at(2.0, 2.0)), Some(clipping));
        // Inside the outline, below every box.
        assert_eq!(hit(&mut doc, at(25.0, 60.0)), None);
    }
}
