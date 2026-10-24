//! Layout: the box tree built from the DOM, and the immutable fragment tree
//! layout produces from it (spec 0002, Layout; spec 0004, child 0004).
//!
//! Layout is incremental (AC-9): each box keeps its last fragment with the
//! constraint space it was laid out in, and a clean box given an equal
//! space returns that fragment without running. Dirty boxes are marked by
//! box rebuild and by style damage.
//!
//! Blocks follow CSS 2.2 flow layout: widths and heights from their
//! properties, collapsing vertical margins ([`block`]). A block's inline
//! content is broken into line boxes of shaped text, inline box pieces and
//! atomic inlines ([`inline`]). Flex containers lay out their items
//! ([`flex`]), and absolute boxes are laid out by their containing block
//! ([`positioned`]). Tree walks use explicit stacks, so a very deep page
//! cannot overflow the thread stack; only atomic inlines, flex containers
//! and absolute boxes nested inside each other recurse, and that is capped.

#![forbid(unsafe_code)]

mod block;
mod box_tree;
mod dump;
mod exclusion;
mod flex;
mod fragment;
mod hit_test;
mod inline;
mod intrinsic;
mod positioned;
pub mod stacking;
mod table;

use std::collections::HashMap;
use std::rc::Rc;

use base::{Au, BoxId, Point, SideOffsets, Size};
use css::{ComputedStyle, WritingMode};
use text::{FontContext, FontDescriptor, LineMetrics, ShapedRun, WebFontLayer};

use exclusion::ExclusionSpace;

use block::InProgress;
pub use box_tree::{BoxFlags, BoxKind, BoxTree, InlineItem, LayoutBox, NaturalSize};
pub use fragment::{
    BlockMargins, BoxFragment, Fragment, FragmentTree, InlineEdges, LineFragment, MarginStrut,
    PendingAbsolute, PlacedGlyph, TextFragment,
};
pub use hit_test::{HitTestResult, hit_test};

/// Space available along one axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AvailableSize {
    /// A known amount.
    Definite(Au),
    /// Lay out as narrow as the content allows.
    MinContent,
    /// Lay out as wide as the content wants.
    MaxContent,
}

impl AvailableSize {
    fn definite(self) -> Option<Au> {
        match self {
            Self::Definite(au) => Some(au),
            Self::MinContent | Self::MaxContent => None,
        }
    }
}

/// Everything a box's layout depends on from outside the box. Equal spaces
/// give equal results for a clean box, so it keys the layout cache.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConstraintSpace {
    /// Space along the inline (horizontal) axis.
    pub available_inline: AvailableSize,
    /// Space along the block (vertical) axis.
    pub available_block: AvailableSize,
    /// What percentages resolve against; `None` where it is not definite.
    pub percentage_basis: Size<Option<Au>>,
    /// The containing block's writing mode.
    pub writing_mode: WritingMode,
    /// A border box width the parent fixes, in place of the box's own
    /// width rules: a flex item's main or stretched cross size (spec 0006,
    /// Layout rules worth pinning).
    pub stretch_inline: Option<Au>,
    /// A border box height the parent fixes, counted as definite for the
    /// box's children's percentages (CSS Flexbox section 9.8).
    pub stretch_block: Option<Au>,
    /// The box's border box top left corner in its BFC's coordinates, or
    /// `None` while its margins have not settled (spec 0009, child 0001).
    pub bfc_offset: Option<Point>,
    /// The floats in this BFC that can reach this box, from its border box
    /// top left corner, or `None` when none can (spec 0009, child 0001).
    pub exclusions: Option<Rc<ExclusionSpace>>,
    /// How far above the box's hypothetical position `clear` moves it, or
    /// `None` when the box has no `clear` (spec 0009, child 0001).
    pub clearance: Option<Au>,
}

impl ConstraintSpace {
    /// The space for the root box: the viewport.
    pub fn viewport(viewport: Size) -> Self {
        Self {
            available_inline: AvailableSize::Definite(viewport.width),
            available_block: AvailableSize::Definite(viewport.height),
            percentage_basis: Size::new(Some(viewport.width), Some(viewport.height)),
            writing_mode: WritingMode::default(),
            stretch_inline: None,
            stretch_block: None,
            bfc_offset: None,
            exclusions: None,
            clearance: None,
        }
    }
}

/// A box's last layout result (spec 0002, Incremental layout).
#[derive(Debug, Clone)]
pub struct LayoutCache {
    /// The space it was laid out in.
    pub space: ConstraintSpace,
    /// What layout produced.
    pub fragment: Rc<Fragment>,
    /// The min and max content widths of the box's content, once an
    /// inline block shrinking to fit asked for them. Valid while the box
    /// stays clean.
    pub intrinsic: Option<(Au, Au)>,
}

/// What one layout pass did: the counters behind AC-9 and the `layout`
/// tracing span.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayoutStats {
    /// Boxes whose layout ran.
    pub laid_out: usize,
    /// Boxes that returned their cached fragment.
    pub cache_hits: usize,
}

/// How deep atomic inlines, flex containers and absolute boxes may nest
/// inside each other. Each level lays out its content with a nested call,
/// so the cap bounds the recursion. Deeper atomic inlines are left out of
/// their line, deeper absolute boxes are not laid out, and a deeper flex
/// container stacks its items like a block.
const MAX_NESTING: usize = 64;

/// The page's fonts and its document's web faces, which every shaping call
/// of a pass uses together (spec 0006, Font scope).
pub(crate) struct Fonts<'a> {
    context: &'a mut FontContext,
    web: &'a WebFontLayer,
}

impl Fonts<'_> {
    /// Shapes `text` (see [`FontContext::shape`]).
    pub(crate) fn shape(
        &mut self,
        text: &str,
        font: &FontDescriptor<'_>,
        size: Au,
    ) -> Vec<ShapedRun> {
        self.context.shape(text, font, size, self.web)
    }

    /// The first available font's metrics (see [`FontContext::metrics`]).
    pub(crate) fn metrics(&mut self, font: &FontDescriptor<'_>, size: Au) -> Option<LineMetrics> {
        self.context.metrics(font, size, self.web)
    }
}

/// What one layout pass carries through the tree.
pub(crate) struct LayoutContext<'a> {
    pub(crate) fonts: Fonts<'a>,
    pub(crate) stats: LayoutStats,
    /// Intrinsic sizes computed this pass, until their box stores them.
    pub(crate) intrinsic: HashMap<BoxId, (Au, Au)>,
    /// How many atomic inlines and flex containers the current box is
    /// nested in.
    depth: usize,
}

/// The used border widths. The computed width is already zero for a
/// `none` or `hidden` style.
pub fn border_widths(style: &ComputedStyle) -> SideOffsets {
    let border = &style.border;
    SideOffsets {
        top: border.top.width,
        right: border.right.width,
        bottom: border.bottom.width,
        left: border.left.width,
    }
}

/// The box's `border-radius` in CSS px, x then y for the top left, top
/// right, bottom right and bottom left corners of a border box `size`,
/// before overlapping corners shrink to fit; `None` when every corner is
/// square. A piece of an inline box split across lines rounds only the
/// corners on the edges it holds.
pub fn corner_radii(
    style: &ComputedStyle,
    size: Size,
    edges: Option<InlineEdges>,
) -> Option<[f32; 8]> {
    let radius = &style.border.radius;
    let (width, height) = (size.width.to_f32_px(), size.height.to_f32_px());
    let corner = |length: css::Length, kept: bool| match (length, kept) {
        (_, false) => (0.0, 0.0),
        (css::Length::Au(au), true) => (au.to_f32_px(), au.to_f32_px()),
        (css::Length::Percent(percent), true) => {
            (width * percent / 100.0, height * percent / 100.0)
        }
    };
    let (start, end) = edges.map_or((true, true), |e| (e.start, e.end));
    let [tl, tr, br, bl] = [
        corner(radius.top_left, start),
        corner(radius.top_right, end),
        corner(radius.bottom_right, end),
        corner(radius.bottom_left, start),
    ];
    let radii = [tl.0, tl.1, tr.0, tr.1, br.0, br.1, bl.0, bl.1];
    radii.iter().any(|r| *r > 0.0).then_some(radii)
}

/// The padding box radii inside a border box with `outer` radii: each
/// shrinks by the border width on its side (CSS Backgrounds 3, section
/// 5.2), and never below zero.
pub fn inner_radii(outer: [f32; 8], border: SideOffsets) -> [f32; 8] {
    let (top, right, bottom, left) = (
        border.top.to_f32_px(),
        border.right.to_f32_px(),
        border.bottom.to_f32_px(),
        border.left.to_f32_px(),
    );
    let less = [left, top, right, top, right, bottom, left, bottom];
    let mut inner = outer;
    for (radius, less) in inner.iter_mut().zip(less) {
        *radius = (*radius - less).max(0.0);
    }
    inner
}

/// The cached fragment for `layout_box`, if it is clean and was laid out in
/// an equal `space`.
fn cached(layout_box: &LayoutBox, space: &ConstraintSpace) -> Option<Rc<Fragment>> {
    let dirty = BoxFlags::NEEDS_LAYOUT | BoxFlags::CHILD_NEEDS_LAYOUT;
    if layout_box.flags.intersects(dirty) {
        return None;
    }
    let cache = layout_box.cache.as_ref()?;
    (cache.space == *space).then(|| Rc::clone(&cache.fragment))
}

/// What to do with the next box: reuse its fragment, or lay it out.
enum Step {
    Reuse(Rc<Fragment>),
    Run(Box<InProgress>),
}

/// Starts box `id` in `space`: its cached fragment, or its sizes resolved
/// and its inline content (atomic inlines first) laid out in lines.
fn step(
    tree: &mut BoxTree,
    id: BoxId,
    space: ConstraintSpace,
    cx: &mut LayoutContext<'_>,
) -> Option<Step> {
    let layout_box = tree.get(id)?;
    if let Some(fragment) = cached(layout_box, &space) {
        cx.stats.cache_hits += 1;
        return Some(Step::Reuse(fragment));
    }
    cx.stats.laid_out += 1;
    // Inline blocks and floats shrink to fit (CSS 2.2 sections 10.3.9 and
    // 10.3.5; spec 0009, child 0001).
    let shrinks = (layout_box.inline_level || layout_box.is_float())
        && layout_box.kind == BoxKind::BlockContainer
        && layout_box.style.size.width.is_none_or(|width| {
            matches!(width, css::Length::Percent(_)) && space.percentage_basis.width.is_none()
        });
    let intrinsic = shrinks.then(|| intrinsic::content_sizes(tree, id, cx));
    let is_root = tree.root() == Some(id);
    let layout_box = tree.get(id)?;
    let is_flex_item = layout_box
        .parent
        .and_then(|parent| tree.get(parent))
        .is_some_and(|parent| parent.kind == BoxKind::FlexContainer);
    let is_flex = layout_box.kind == BoxKind::FlexContainer;
    let mut in_progress =
        InProgress::start(id, layout_box, space, is_root, is_flex_item, intrinsic);
    if !layout_box.inline.is_empty() {
        let atomics: Vec<BoxId> = layout_box
            .inline
            .iter()
            .filter_map(|item| match item {
                InlineItem::Atomic(atomic) => Some(*atomic),
                _ => None,
            })
            .collect();
        let child_space = in_progress.child_space();
        let mut fragments = HashMap::new();
        for atomic in atomics {
            if cx.depth >= MAX_NESTING {
                break;
            }
            cx.depth += 1;
            let fragment = lay_out_box(tree, atomic, child_space.clone(), cx);
            cx.depth -= 1;
            if let Some(fragment) = fragment {
                fragments.insert(atomic, fragment);
            }
        }
        let layout_box = tree.get(id)?;
        let lines = inline::lay_out_lines(
            tree,
            &layout_box.inline,
            &layout_box.style,
            in_progress.content_width,
            &in_progress.exclusions,
            &fragments,
            &mut cx.fonts,
        );
        in_progress.set_lines(lines);
    }
    if is_flex && cx.depth < MAX_NESTING {
        // Flex measures its items more than once, so it lays them out with
        // nested calls and the stack has no children left to visit.
        cx.depth += 1;
        flex::lay_out(tree, &mut in_progress, cx);
        cx.depth -= 1;
    }
    Some(Step::Run(Box::new(in_progress)))
}

/// Lays out the box tree for `viewport`: blocks in normal flow with
/// collapsing margins, inline content in line boxes. Text is shaped with
/// `fonts` and the document's web faces in `web` (spec 0006, AC-10).
///
/// Clean boxes laid out in an equal constraint space return their cached
/// fragment; every box that runs stores its result and is clean afterwards.
pub fn layout(
    tree: &mut BoxTree,
    viewport: Size,
    fonts: &mut FontContext,
    web: &WebFontLayer,
) -> (FragmentTree, LayoutStats) {
    let mut cx = LayoutContext {
        fonts: Fonts {
            context: fonts,
            web,
        },
        stats: LayoutStats::default(),
        intrinsic: HashMap::new(),
        depth: 0,
    };
    let root = tree.root().and_then(|root| {
        let fragment = lay_out_box(tree, root, ConstraintSpace::viewport(viewport), &mut cx)?;
        // The root starts a formatting context, so nothing collapses
        // through its margins.
        let margins = fragment.as_box()?.margins;
        Some((Point::new(margins.left, margins.top.resolve()), fragment))
    });
    // Sizes computed for boxes that stayed cached are kept for next time.
    for (id, sizes) in cx.intrinsic.drain() {
        if let Some(cache) = tree.boxes.get_mut(id).and_then(|b| b.cache.as_mut()) {
            cache.intrinsic = Some(sizes);
        }
    }
    (FragmentTree { viewport, root }, cx.stats)
}

/// Lays out box `id` and everything below it in `space`, with a heap
/// stack of boxes in progress.
fn lay_out_box(
    tree: &mut BoxTree,
    id: BoxId,
    space: ConstraintSpace,
    cx: &mut LayoutContext<'_>,
) -> Option<Rc<Fragment>> {
    let space = space.clone();
    let mut stack = match step(tree, id, space.clone(), cx)? {
        Step::Reuse(fragment) => return Some(fragment),
        Step::Run(in_progress) => vec![in_progress],
    };
    loop {
        let top = stack.last_mut()?;
        let next = tree
            .get(top.id)
            .and_then(|b| b.children.get(top.next_child))
            .copied();
        if let Some(child_id) = next {
            top.next_child += 1;
            // An absolute box leaves only its static position in flow; its
            // containing block lays it out (spec 0006, AC-5). A fixed box
            // leaves its static position too, then hoists to the viewport's
            // containing block (spec 0009, child 0003, AC-10).
            if tree.get(child_id).is_some_and(LayoutBox::is_absolute) {
                top.place_absolute(child_id, false);
                continue;
            }
            if tree.get(child_id).is_some_and(LayoutBox::is_fixed) {
                top.place_absolute(child_id, true);
                continue;
            }
            // A float leaves only its placement in flow; it is laid out by
            // its block formatting context (spec 0009, child 0001).
            if tree.get(child_id).is_some_and(LayoutBox::is_float) {
                top.place_float(tree, child_id, cx);
                continue;
            }
            let Some(space) = tree.get(child_id).map(|child| top.block_child_space(child)) else {
                continue;
            };
            match step(tree, child_id, space, cx) {
                Some(Step::Reuse(fragment)) => top.place(tree, fragment),
                Some(Step::Run(in_progress)) => stack.push(in_progress),
                None => {}
            }
            continue;
        }

        let done = stack.pop()?;
        let space = done.space.clone();
        let (id, was_clean) = (done.id, done.was_clean);
        let mut fragment = done.finish(&mut cx.fonts);
        if !fragment.absolutes.is_empty()
            && let Some(cb) = positioned::containing_block(tree, id, &fragment, space.clone())
        {
            positioned::lay_out_absolutes(tree, &mut fragment, cb, cx);
        }
        // Fixed boxes hoist to the viewport's initial containing block, so
        // only the root lays them out; they add nothing to scrollable
        // overflow (spec 0009, child 0003, AC-10).
        if tree.root() == Some(id) && fragment.absolutes.iter().any(|absolute| absolute.fixed) {
            let cb = positioned::viewport_containing_block(&fragment.margins, &space);
            positioned::lay_out_fixed(tree, &mut fragment, cb, cx);
        }
        let fragment = Rc::new(Fragment::Box(fragment));
        if let Some(layout_box) = tree.boxes.get_mut(id) {
            let kept = layout_box.cache.as_ref().and_then(|c| c.intrinsic);
            let intrinsic = cx
                .intrinsic
                .remove(&id)
                .or(if was_clean { kept } else { None });
            layout_box.flags = BoxFlags::empty();
            layout_box.cache = Some(LayoutCache {
                space,
                fragment: Rc::clone(&fragment),
                intrinsic,
            });
        }
        match stack.last_mut() {
            Some(parent) => parent.place(tree, fragment),
            None => break Some(fragment),
        }
    }
}

#[cfg(test)]
mod tests {
    use base::atoms::QualName;
    use base::local_name;
    use css::{Clear, Display, Float, Length, Margin, Padding, Position};
    use dom::{Document, NodeId};

    use super::*;

    fn block(margin: f32, padding: f32, height: Option<f32>) -> Rc<ComputedStyle> {
        let mut style = ComputedStyle::default();
        Rc::make_mut(&mut style.display_box).display = Display::Block;
        style.margin = Rc::new(Margin::all(Length::px(margin)));
        style.padding = Rc::new(Padding::all(Length::px(padding)));
        Rc::make_mut(&mut style.size).height = height.map(Length::px);
        Rc::new(style)
    }

    fn add(doc: &mut Document, parent: NodeId, style: Rc<ComputedStyle>) -> NodeId {
        let id = doc.create_element(QualName::html(local_name!("div")));
        doc.append_child(parent, id).unwrap();
        doc.set_style(id, Some(style)).unwrap();
        id
    }

    fn px(px: f32) -> Au {
        Au::from_px(px)
    }

    #[test]
    fn blocks_stack_vertically_inside_padding_and_margins() {
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let html = add(&mut doc, root, block(10.0, 5.0, None));
        let a = add(&mut doc, html, block(0.0, 0.0, Some(20.0)));
        let b = add(&mut doc, html, block(4.0, 0.0, Some(30.0)));
        let hidden = add(&mut doc, html, {
            let mut style = (*block(0.0, 0.0, Some(99.0))).clone();
            Rc::make_mut(&mut style.display_box).display = Display::None;
            Rc::new(style)
        });

        let mut boxes = BoxTree::build(&mut doc);
        assert_eq!(boxes.len(), 3, "display: none makes no box");
        assert!(doc.node(hidden).unwrap().layout_box().is_none());

        let (tree, _) = layout(
            &mut boxes,
            Size::new(px(800.0), px(600.0)),
            &mut FontContext::new(),
            &text::WebFontLayer::default(),
        );
        let (origin, fragment) = tree.root.unwrap();
        let html_fragment = fragment.as_box().unwrap();
        assert_eq!(origin, Point::new(px(10.0), px(10.0)));
        assert_eq!(html_fragment.node, Some(html));
        // 800 - 2 * 10 margin; 20 + (4 + 30 + 4) children + 2 * 5 padding.
        assert_eq!(html_fragment.size, Size::new(px(780.0), px(68.0)));

        let positions: Vec<_> = html_fragment.children.iter().map(|(p, _)| *p).collect();
        assert_eq!(
            positions,
            [Point::new(px(5.0), px(5.0)), Point::new(px(9.0), px(29.0))]
        );
        let b_fragment = html_fragment.children[1].1.as_box().unwrap();
        assert_eq!(b_fragment.node, Some(b));
        assert_eq!(b_fragment.size, Size::new(px(762.0), px(30.0)));
        let _ = a;
    }

    #[test]
    fn a_cleared_block_sits_below_the_matching_float() {
        // Spec 0009, child 0001, AC-2: `clear: left` pushes the box below the
        // lowest left float, stopping margins collapsing through it.
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let html = add(&mut doc, root, block(0.0, 0.0, None));
        let body = add(&mut doc, html, block(0.0, 0.0, None));
        let mut floated = block(0.0, 0.0, Some(50.0));
        {
            let s = Rc::make_mut(&mut floated);
            Rc::make_mut(&mut s.display_box).float = Float::Left;
            Rc::make_mut(&mut s.size).width = Some(Length::px(100.0));
        }
        add(&mut doc, body, floated);
        let mut cleared = block(0.0, 0.0, Some(20.0));
        {
            let s = Rc::make_mut(&mut cleared);
            Rc::make_mut(&mut s.display_box).clear = Clear::Left;
        }
        add(&mut doc, body, cleared);

        let mut boxes = BoxTree::build(&mut doc);
        let (tree, _) = layout(
            &mut boxes,
            Size::new(px(800.0), px(600.0)),
            &mut FontContext::new(),
            &text::WebFontLayer::default(),
        );
        let (_, fragment) = tree.root.unwrap();
        let html_fragment = fragment.as_box().unwrap();
        let body_fragment = html_fragment.children[0].1.as_box().unwrap();
        let float_pos = body_fragment.children[0].0;
        let cleared_pos = body_fragment.children[1].0;
        assert_eq!(float_pos, Point::new(px(0.0), px(0.0)));
        assert_eq!(
            cleared_pos.y,
            px(50.0),
            "cleared block's top edge below the float"
        );
        assert_eq!(cleared_pos.x, px(0.0));
    }

    #[test]
    fn user_scrollable_is_per_axis_from_overflow() {
        // Spec 0009, child 0002, AC-6 and AC-7: `scroll`/`auto` are user
        // scrollable on that axis; `hidden` and `clip` are not.
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let html = add(&mut doc, root, block(0.0, 0.0, None));

        let mut scroll_xy = block(0.0, 0.0, Some(40.0));
        {
            let s = Rc::make_mut(&mut scroll_xy);
            Rc::make_mut(&mut s.display_box).overflow_x = css::Overflow::Scroll;
            Rc::make_mut(&mut s.display_box).overflow_y = css::Overflow::Auto;
        }
        add(&mut doc, html, scroll_xy);

        let mut hidden = block(0.0, 0.0, Some(40.0));
        {
            let s = Rc::make_mut(&mut hidden);
            Rc::make_mut(&mut s.display_box).overflow_x = css::Overflow::Hidden;
            Rc::make_mut(&mut s.display_box).overflow_y = css::Overflow::Clip;
        }
        add(&mut doc, html, hidden);

        let mut boxes = BoxTree::build(&mut doc);
        let (tree, _) = layout(
            &mut boxes,
            Size::new(px(800.0), px(600.0)),
            &mut FontContext::new(),
            &text::WebFontLayer::default(),
        );
        let (_, fragment) = tree.root.unwrap();
        let html_fragment = fragment.as_box().unwrap();
        assert_eq!(
            html_fragment.children[0]
                .1
                .as_box()
                .unwrap()
                .user_scrollable,
            [true, true]
        );
        assert_eq!(
            html_fragment.children[1]
                .1
                .as_box()
                .unwrap()
                .user_scrollable,
            [false, false]
        );
    }

    fn styled(doc: &mut Document, parent: NodeId, name: &str, display: Display) -> NodeId {
        let id = doc.create_element(QualName::html(base::atoms::LocalName::from(name)));
        doc.append_child(parent, id).unwrap();
        let mut style = ComputedStyle::default();
        Rc::make_mut(&mut style.display_box).display = display;
        doc.set_style(id, Some(Rc::new(style))).unwrap();
        id
    }

    fn text(doc: &mut Document, parent: NodeId, text: &str) -> NodeId {
        let id = doc.create_text(text);
        doc.append_child(parent, id).unwrap();
        id
    }

    #[test]
    fn inline_content_beside_blocks_gets_anonymous_blocks_and_collapses_white_space() {
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let body = styled(&mut doc, root, "body", Display::Block);
        text(&mut doc, body, "  one\n  ");
        let span = styled(&mut doc, body, "span", Display::Inline);
        text(&mut doc, span, "  two  ");
        // A block inside an inline is lifted out beside it.
        let inner = styled(&mut doc, span, "div", Display::Block);
        text(&mut doc, inner, "three");
        text(&mut doc, body, "\n  ");
        let p = styled(&mut doc, body, "p", Display::Block);
        text(&mut doc, p, " four ");
        text(&mut doc, body, "\n");

        let boxes = BoxTree::build(&mut doc);
        assert_eq!(
            boxes.dump(&doc),
            "block body\n  block (anonymous)\n    text body>#text[1] \"one \"\n    \
             start body>span\n    text body>span>#text \"two\"\n    end body>span\n  \
             block body>span>div\n    text body>span>div>#text \"three\"\n  \
             block body>p\n    text body>p>#text \"four\"\n"
        );
        assert!(
            doc.node(span).unwrap().layout_box().is_some(),
            "the span keeps its inline box"
        );
        assert!(doc.node(inner).unwrap().layout_box().is_some());
    }

    /// Every text fragment in a block's lines, with its line's offset.
    fn line_texts(block: &BoxFragment) -> Vec<(Point, &TextFragment)> {
        let mut texts = Vec::new();
        for (at, child) in &block.children {
            let Fragment::Line(line) = &**child else {
                continue;
            };
            for (text_at, text) in &line.children {
                if let Fragment::Text(t) = &**text {
                    texts.push((at.offset_by(*text_at), t));
                }
            }
        }
        texts
    }

    #[test]
    fn text_wraps_into_lines_that_fit_the_block() {
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let body = styled(&mut doc, root, "body", Display::Block);
        let words = "lorem ipsum dolor sit amet consectetur adipiscing elit";
        let node = text(&mut doc, body, words);
        let mut boxes = BoxTree::build(&mut doc);
        let mut fonts = FontContext::new();
        let (tree, _) = layout(
            &mut boxes,
            Size::new(px(120.0), px(600.0)),
            &mut fonts,
            &text::WebFontLayer::default(),
        );
        let root = root_fragment(&tree);
        let body = root.as_box().unwrap();
        let lines: Vec<&LineFragment> = body
            .children
            .iter()
            .filter_map(|(_, f)| match &**f {
                Fragment::Line(line) => Some(line),
                _ => None,
            })
            .collect();
        if lines.is_empty() {
            // No system font on this machine: nothing to lay out.
            return;
        }
        assert!(lines.len() > 1, "{} wraps in 120px", words);
        let texts = line_texts(body);
        let joined: String = texts.iter().map(|(_, t)| t.text.as_str()).collect();
        assert_eq!(joined.trim_end(), words);
        let tops: Vec<Au> = body.children.iter().map(|(at, _)| at.y).collect();
        for (pair, line) in tops.windows(2).zip(&lines) {
            assert_eq!(pair[1] - pair[0], line.size.height, "lines stack");
        }
        assert!(texts.iter().all(|(_, t)| t.node == node));
        // A line's trailing space may hang past the edge; a space is under
        // half an em.
        assert!(
            texts
                .iter()
                .all(|(at, t)| at.x == Au::ZERO && t.size.width <= px(120.0 + 8.0))
        );
        assert_eq!(
            body.size.height,
            lines.iter().map(|l| l.size.height).sum::<Au>()
        );
        assert_eq!(body.baseline, Some(lines[0].baseline));
        // Each glyph run sits on its line's baseline.
        for (at, t) in &texts {
            let line_top = tops
                .iter()
                .rev()
                .find(|&&top| top <= at.y)
                .copied()
                .unwrap();
            let line = lines[tops.iter().position(|&top| top == line_top).unwrap()];
            assert_eq!(at.y + t.baseline, line_top + line.baseline);
        }
    }

    #[test]
    fn a_document_without_styles_lays_out_to_nothing() {
        let mut doc = Document::new("about:blank");
        let html = doc.create_element(QualName::html(local_name!("html")));
        doc.append_child(doc.root(), html).unwrap();
        let mut boxes = BoxTree::build(&mut doc);
        assert!(boxes.is_empty());
        assert_eq!(
            layout(
                &mut boxes,
                Size::default(),
                &mut FontContext::new(),
                &text::WebFontLayer::default()
            )
            .0
            .root,
            None
        );
    }

    #[test]
    fn deep_trees_do_not_overflow_the_stack() {
        let mut doc = Document::new("about:blank");
        let mut parent = doc.root();
        for _ in 0..5_000 {
            parent = add(&mut doc, parent, block(0.0, 0.0, None));
        }
        let mut boxes = BoxTree::build(&mut doc);
        let viewport = Size::new(px(100.0), px(100.0));
        assert!(
            layout(
                &mut boxes,
                viewport,
                &mut FontContext::new(),
                &text::WebFontLayer::default()
            )
            .0
            .root
            .is_some()
        );
        // The cached path walks the same depth.
        let leaf = doc.node(parent).unwrap().layout_box().unwrap();
        boxes.mark_needs_layout(leaf);
        assert_eq!(
            layout(
                &mut boxes,
                viewport,
                &mut FontContext::new(),
                &text::WebFontLayer::default()
            )
            .1
            .laid_out,
            5_000
        );
    }

    /// `html` > `body` > three cards, each 20px tall, the middle one with a
    /// child. Laid out once, so every box is clean and cached.
    struct Fixture {
        doc: Document,
        boxes: BoxTree,
        body: NodeId,
        cards: [NodeId; 3],
        inner: NodeId,
    }

    const VIEWPORT: Size = Size::new(Au(800 * 60), Au(600 * 60));

    fn fixture() -> Fixture {
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let html = add(&mut doc, root, block(0.0, 0.0, None));
        let body = add(&mut doc, html, block(8.0, 0.0, None));
        let cards = [0, 1, 2].map(|_| add(&mut doc, body, block(0.0, 2.0, Some(20.0))));
        let inner = add(&mut doc, cards[1], block(0.0, 0.0, Some(5.0)));
        doc.take_box_rebuild_roots();
        let mut boxes = BoxTree::build(&mut doc);
        let (_, stats) = layout(
            &mut boxes,
            VIEWPORT,
            &mut FontContext::new(),
            &text::WebFontLayer::default(),
        );
        assert_eq!(stats.laid_out, 6, "the first layout runs every box");
        Fixture {
            doc,
            boxes,
            body,
            cards,
            inner,
        }
    }

    fn box_of(f: &Fixture, node: NodeId) -> BoxId {
        f.doc.node(node).unwrap().layout_box().unwrap()
    }

    fn root_fragment(tree: &FragmentTree) -> Rc<Fragment> {
        Rc::clone(&tree.root.as_ref().unwrap().1)
    }

    #[test]
    fn a_clean_tree_in_the_same_space_reuses_its_fragment() {
        // AC-9
        let mut f = fixture();
        let card = box_of(&f, f.cards[0]);
        let before = Rc::clone(&f.boxes.get(card).unwrap().cache.as_ref().unwrap().fragment);
        let first = root_fragment(
            &layout(
                &mut f.boxes,
                VIEWPORT,
                &mut FontContext::new(),
                &text::WebFontLayer::default(),
            )
            .0,
        );

        let (tree, stats) = layout(
            &mut f.boxes,
            VIEWPORT,
            &mut FontContext::new(),
            &text::WebFontLayer::default(),
        );
        assert_eq!(
            stats,
            LayoutStats {
                laid_out: 0,
                cache_hits: 1
            }
        );
        assert!(Rc::ptr_eq(&root_fragment(&tree), &first));
        let after = &f.boxes.get(card).unwrap().cache.as_ref().unwrap().fragment;
        assert!(Rc::ptr_eq(&before, after));
    }

    #[test]
    fn one_dirty_leaf_lays_out_only_its_ancestor_path() {
        // AC-9
        let mut f = fixture();
        let old_card_0 = Rc::clone(
            &f.boxes
                .get(box_of(&f, f.cards[0]))
                .unwrap()
                .cache
                .as_ref()
                .unwrap()
                .fragment,
        );
        let inner = box_of(&f, f.inner);
        f.boxes.mark_needs_layout(inner);

        let (tree, stats) = layout(
            &mut f.boxes,
            VIEWPORT,
            &mut FontContext::new(),
            &text::WebFontLayer::default(),
        );
        // html, body, the middle card and its child run; cards 0 and 2 are reused.
        assert_eq!(
            stats,
            LayoutStats {
                laid_out: 4,
                cache_hits: 2
            }
        );
        let root = root_fragment(&tree);
        let html = root.as_box().unwrap();
        let body = html.children[0].1.as_box().unwrap();
        assert!(Rc::ptr_eq(&body.children[0].1, &old_card_0));
        assert!(
            layout(
                &mut f.boxes,
                VIEWPORT,
                &mut FontContext::new(),
                &text::WebFontLayer::default()
            )
            .1
            .laid_out
                == 0,
            "clean again"
        );
    }

    #[test]
    fn a_new_space_misses_the_cache() {
        let mut f = fixture();
        let narrower = Size::new(px(400.0), px(600.0));
        let (_, stats) = layout(
            &mut f.boxes,
            narrower,
            &mut FontContext::new(),
            &text::WebFontLayer::default(),
        );
        assert_eq!(stats.laid_out, 6, "every width changed");
        let shorter = Size::new(px(400.0), px(300.0));
        let (_, stats) = layout(
            &mut f.boxes,
            shorter,
            &mut FontContext::new(),
            &text::WebFontLayer::default(),
        );
        // Only the root sees the height; its children get the same space.
        assert_eq!(
            stats,
            LayoutStats {
                laid_out: 1,
                cache_hits: 1
            }
        );
    }

    #[test]
    fn relayout_damage_marks_the_box_and_repaint_damage_does_not() {
        let mut f = fixture();
        let card = box_of(&f, f.cards[2]);
        let mut taller = (*f.boxes.get(card).unwrap().style).clone();
        Rc::make_mut(&mut taller.size).height = Some(Length::px(90.0));
        let mut recolored = (*f.boxes.get(card).unwrap().style).clone();
        Rc::make_mut(&mut recolored.background).color = css::Color::BLACK;

        f.boxes
            .restyle_box(card, Rc::new(recolored), css::StyleDamage::Repaint);
        assert_eq!(
            layout(
                &mut f.boxes,
                VIEWPORT,
                &mut FontContext::new(),
                &text::WebFontLayer::default()
            )
            .1
            .laid_out,
            0
        );
        assert_eq!(
            f.boxes.get(card).unwrap().style.background.color,
            css::Color::BLACK,
            "paint reads the new color from the box"
        );

        f.boxes
            .restyle_box(card, Rc::new(taller), css::StyleDamage::Relayout);
        let (tree, stats) = layout(
            &mut f.boxes,
            VIEWPORT,
            &mut FontContext::new(),
            &text::WebFontLayer::default(),
        );
        assert_eq!(stats.laid_out, 3, "html, body and the card");
        let root = root_fragment(&tree);
        let html = root.as_box().unwrap();
        // Body margins around three cards of 20px plus 2px padding a side.
        assert_eq!(html.size.height, px(8.0 + 24.0 + 24.0 + 94.0 + 8.0));
    }

    #[test]
    fn box_rebuild_starts_at_the_nearest_container_and_keeps_its_id() {
        let mut f = fixture();
        let body_box = box_of(&f, f.body);
        let middle = f.cards[1];
        let other_card = box_of(&f, f.cards[0]);

        // A new child in the middle card, and the old inner child hidden.
        let extra = add(&mut f.doc, middle, block(0.0, 0.0, Some(7.0)));
        let mut hidden = (*f.boxes.get(box_of(&f, f.inner)).unwrap().style).clone();
        Rc::make_mut(&mut hidden.display_box).display = Display::None;
        f.doc.set_style(f.inner, Some(Rc::new(hidden))).unwrap();

        let roots = f.doc.take_box_rebuild_roots();
        assert_eq!(roots, [middle]);
        let middle_box = box_of(&f, middle);
        let built = f.boxes.rebuild(&mut f.doc, &roots);
        assert_eq!(built, 1, "only the new child gets a box");
        assert_eq!(box_of(&f, middle), middle_box, "the container keeps its id");
        assert_eq!(box_of(&f, f.cards[0]), other_card, "siblings are untouched");
        assert!(f.doc.node(f.inner).unwrap().layout_box().is_none());
        assert_eq!(f.boxes.len(), 6);
        let children = &f.boxes.get(middle_box).unwrap().children;
        assert_eq!(children.len(), 1);
        assert_eq!(f.boxes.get(children[0]).unwrap().node, Some(extra));

        let (_, stats) = layout(
            &mut f.boxes,
            VIEWPORT,
            &mut FontContext::new(),
            &text::WebFontLayer::default(),
        );
        assert_eq!(
            stats,
            LayoutStats {
                laid_out: 4,
                cache_hits: 2
            }
        );
        assert!(f.boxes.get(body_box).unwrap().flags == BoxFlags::empty());
    }

    #[test]
    fn rebuilding_at_the_document_rebuilds_everything() {
        let mut f = fixture();
        let root = f.doc.root();
        let built = f.boxes.rebuild(&mut f.doc, &[root]);
        assert_eq!(built, 6);
        assert_eq!(
            layout(
                &mut f.boxes,
                VIEWPORT,
                &mut FontContext::new(),
                &text::WebFontLayer::default()
            )
            .1
            .laid_out,
            6
        );
    }

    #[test]
    fn removing_a_node_drops_its_boxes_at_the_next_rebuild() {
        let mut f = fixture();
        let middle = f.cards[1];
        f.doc.remove_child(f.body, middle).unwrap();
        let roots = f.doc.take_box_rebuild_roots();
        assert_eq!(roots, [f.body]);
        f.boxes.rebuild(&mut f.doc, &roots);
        assert_eq!(f.boxes.len(), 4, "the card and its child are gone");
        // Body's children were rebuilt as a whole, so the cards left behind
        // are fresh boxes with nothing cached.
        let (_, stats) = layout(
            &mut f.boxes,
            VIEWPORT,
            &mut FontContext::new(),
            &text::WebFontLayer::default(),
        );
        assert_eq!(
            stats,
            LayoutStats {
                laid_out: 4,
                cache_hits: 0
            }
        );
    }

    #[test]
    fn a_fixed_box_lays_out_against_the_viewport_and_stays_in_flow_spot() {
        // Spec 0009, child 0003, AC-10: `position: fixed` lays the box out
        // against the viewport and it stays in place while the page scrolls,
        // adding nothing to the viewport's scrollable overflow.
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let html = add(&mut doc, root, block(0.0, 0.0, None));
        let mut fixed = block(0.0, 0.0, Some(50.0));
        {
            let s = Rc::make_mut(&mut fixed);
            Rc::make_mut(&mut s.display_box).position = Position::Fixed;
            Rc::make_mut(&mut s.position_offsets).top = Some(Length::px(0.0));
            Rc::make_mut(&mut s.position_offsets).left = Some(Length::px(0.0));
            Rc::make_mut(&mut s.size).width = Some(Length::px(100.0));
        }
        add(&mut doc, html, fixed);

        let mut boxes = BoxTree::build(&mut doc);
        let (tree, _) = layout(
            &mut boxes,
            Size::new(px(800.0), px(600.0)),
            &mut FontContext::new(),
            &text::WebFontLayer::default(),
        );
        let (origin, fragment) = tree.root.unwrap();
        assert_eq!(origin, Point::default());
        let root_fragment = fragment.as_box().unwrap();
        let fixed_child = root_fragment
            .children
            .iter()
            .find(|(_, f)| {
                f.as_box()
                    .is_some_and(|b| b.size == Size::new(px(100.0), px(50.0)))
            })
            .expect("the fixed box is a child of the root fragment");
        assert_eq!(fixed_child.0, Point::default());
    }
}
