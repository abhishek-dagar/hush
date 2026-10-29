//! Block layout: one block box's sizes and the stacking of its children
//! with collapsing margins (spec 0004, child 0004, Block formatting, AC-23).
//!
//! Widths follow CSS 2.2 section 10.3.3, heights section 10.6.3, and
//! vertical margins collapse as section 8.3.1 says. There are no floats,
//! and absolute boxes are laid out by their containing block afterwards
//! ([`crate::positioned`]), so where a block sits never changes how its
//! content lays out: margin collapsing is done entirely while placing
//! finished children. Each fragment carries the struts of the margins that collapse
//! through its top and bottom edges ([`BlockMargins`]), so a cached child
//! collapses the same way as a fresh one.

use std::rc::Rc;

use base::{Au, BoxId, Point, Rect, SideOffsets, Size, mat4_mul, mat4_translate};
use css::{BoxSizing, Clear, ComputedStyle, Length, WritingMode};
use dom::NodeId;

use crate::Fonts;
use crate::box_tree::{BoxKind, BoxTree, FloatSide, LayoutBox, NaturalSize};
use crate::exclusion::{Exclusion, ExclusionSpace};
use crate::fragment::{
    BlockMargins, BoxFragment, Fragment, MarginStrut, PendingAbsolute, StickyInfo, TextFragment,
};
use crate::inline::{Lines, font_descriptor, text_fragment};
use crate::{
    AvailableSize, ConstraintSpace, LayoutContext, border_widths, lay_out_box, positioned,
};

/// A box partway through layout: its children are being laid out.
pub(crate) struct InProgress {
    pub(crate) id: BoxId,
    node: Option<NodeId>,
    style: Rc<ComputedStyle>,
    pub(crate) space: ConstraintSpace,
    /// The box had no dirty flags: only the space changed, so intrinsic
    /// sizes cached on it still hold.
    pub(crate) was_clean: bool,
    writing_mode: WritingMode,
    border: SideOffsets,
    padding: SideOffsets,
    /// Used left and right margins, and the box's own top and bottom ones.
    margins: BlockMargins,
    own_top: Au,
    own_bottom: Au,
    is_scroll_container: bool,
    /// Whether the user may scroll this box on each axis (x, y).
    user_scrollable: [bool; 2],
    /// It centers its content in a taller content box: an `<input>` or
    /// `<button>` laid out as a block (spec 0006, AC-17).
    centers_content: bool,
    pub(crate) content_width: Au,
    /// The content height `height` sets, when it resolves.
    height: Option<Au>,
    min_height: Au,
    max_height: Option<Au>,
    /// Its bottom margin may collapse with its last child's.
    bottom_adjoining: bool,
    /// Its own top and bottom margins may collapse through it, if it turns
    /// out empty.
    may_collapse_through: bool,
    pub(crate) next_child: usize,
    /// The content height so far: where the next child's top margin starts.
    cursor: Au,
    /// The margins after the last placed child, not resolved yet because
    /// the next one may collapse with them.
    pending: MarginStrut,
    /// No child with content placed yet, and the margins in `pending` still
    /// adjoin this box's top margin: it starts no formatting context and
    /// has no top border or padding.
    at_top: bool,
    /// Children's top margins that collapsed into this box's top margin.
    absorbed_top: MarginStrut,
    /// The box's inline content made at least one line box.
    has_lines: bool,
    children: Vec<(Point, Rc<Fragment>)>,
    /// It is a BFC root: its `height: auto` grows to hold its floats.
    bfc_root: bool,
    /// The bottom of the lowest placed float, in BFC coordinates.
    float_bottom: Au,
    /// The first and last line's baselines from the border box top.
    baseline: Option<Au>,
    last_baseline: Option<Au>,
    marker: Option<String>,
    /// How far `position: relative` moves the box.
    relative_offset: Point,
    /// Sticky insets resolved against the containing block, for a
    /// `position: sticky` box (spec 0009, child 0003, AC-11).
    sticky_insets: Option<SideOffsets<Option<Au>>>,
    /// Absolute descendants waiting for their containing block, from the
    /// border box corner.
    absolutes: Vec<PendingAbsolute>,
    /// The floats placed in this BFC so far, as an exclusion space, from
    /// this box's content box top left.
    pub(crate) exclusions: ExclusionSpace,
    /// The floats placed in this BFC, as fragments to hand to the parent as
    /// `exclusions_out` and to paint (spec 0009, child 0001).
    pub(crate) floats: Vec<(Point, Rc<Fragment>)>,
}

/// A flex container's own sizes, which its items are laid out in.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FlexFrame {
    pub(crate) content_width: Au,
    /// The content height, when it is definite.
    pub(crate) height: Option<Au>,
    pub(crate) min_height: Au,
    pub(crate) max_height: Option<Au>,
}

/// A grid container's own sizes, which its items are laid out in.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GridFrame {
    pub(crate) content_width: Au,
    /// The content height, when it is definite.
    pub(crate) height: Option<Au>,
    #[allow(
        dead_code,
        reason = "grid layout does not clamp to min-height and max-height yet"
    )]
    pub(crate) min_height: Au,
    #[allow(
        dead_code,
        reason = "grid layout does not clamp to min-height and max-height yet"
    )]
    pub(crate) max_height: Option<Au>,
}

/// A flex container's laid out items, from its content box origin.
#[derive(Debug, Default)]
pub(crate) struct FlexItems {
    pub(crate) children: Vec<(Point, Rc<Fragment>)>,
    /// The content height the items need.
    pub(crate) height: Au,
    pub(crate) baseline: Option<Au>,
    pub(crate) last_baseline: Option<Au>,
    /// Absolute children and the absolute descendants the items hand on.
    pub(crate) absolutes: Vec<PendingAbsolute>,
}

/// A grid container's laid out items, from its content box origin.
#[derive(Debug, Default)]
pub(crate) struct GridItems {
    pub(crate) children: Vec<(Point, Rc<Fragment>)>,
    /// The content height the items need.
    pub(crate) height: Au,
    pub(crate) baseline: Option<Au>,
    pub(crate) last_baseline: Option<Au>,
    /// Absolute children and the absolute descendants the items hand on.
    pub(crate) absolutes: Vec<PendingAbsolute>,
}

/// Resolves a length against an optional basis: `None` when it is a
/// percentage and the basis is not definite.
pub(crate) fn resolve(length: Option<Length>, basis: Option<Au>) -> Option<Au> {
    match length? {
        Length::Au(au) => Some(au),
        percent @ Length::Percent(_) => basis.map(|b| percent.resolve(Some(b))),
        Length::Calc(expr) => Some(expr.resolve(basis)),
    }
}

/// `value` clamped by `max` and then `min`, so `min` wins (CSS 2.2 section
/// 10.4).
pub(crate) fn clamp(value: Au, min: Au, max: Option<Au>) -> Au {
    max.map_or(value, |max| value.min(max)).max(min)
}

impl InProgress {
    /// Resolves the box's own sizes from its style and the space it gets.
    /// `intrinsic` is its content's min and max content widths, given for
    /// an inline block whose width shrinks to fit. A flex item starts its
    /// own formatting context, as the root does.
    pub(crate) fn start(
        id: BoxId,
        layout_box: &LayoutBox,
        space: ConstraintSpace,
        is_root: bool,
        is_flex_item: bool,
        intrinsic: Option<(Au, Au)>,
    ) -> Self {
        let style = &layout_box.style;
        // Margins and padding resolve percentages against the containing
        // block's width on every side.
        let basis_width = space.percentage_basis.width;
        let basis_height = space.percentage_basis.height;
        let padding = SideOffsets {
            top: style.padding.top.resolve(basis_width),
            right: style.padding.right.resolve(basis_width),
            bottom: style.padding.bottom.resolve(basis_width),
            left: style.padding.left.resolve(basis_width),
        };
        let border = border_widths(style);
        let edges_x = border.horizontal() + padding.horizontal();
        let edges_y = border.vertical() + padding.vertical();
        let border_box = style.size.box_sizing == BoxSizing::BorderBox;
        let content_x = |w: Au| {
            if border_box {
                (w - edges_x).max(Au::ZERO)
            } else {
                w
            }
        };
        let content_y = |h: Au| {
            if border_box {
                (h - edges_y).max(Au::ZERO)
            } else {
                h
            }
        };

        let size = &style.size;
        let width = resolve(size.width.clone(), basis_width).map(content_x);
        let min_width = resolve(size.min_width.clone(), basis_width).map_or(Au::ZERO, content_x);
        let max_width = resolve(size.max_width.clone(), basis_width).map(content_x);
        let height = resolve(size.height.clone(), basis_height).map(content_y);
        let min_height = resolve(size.min_height.clone(), basis_height).map_or(Au::ZERO, content_y);
        let max_height = resolve(size.max_height.clone(), basis_height).map(content_y);

        // Whether each dimension was `auto` (unresolved) before the fill /
        // shrink-to-fit rules run: `aspect-ratio` derives an automatic
        // dimension from the other (spec 0010, AC-7).
        let width_auto = width.is_none();
        let height_auto = height.is_none();

        let side = |length: Option<Length>| length.map(|l| l.resolve(basis_width));
        let margin_left = side(style.margin.left.clone());
        let margin_right = side(style.margin.right.clone());
        let own_top = side(style.margin.top.clone()).unwrap_or(Au::ZERO);
        let own_bottom = side(style.margin.bottom.clone()).unwrap_or(Au::ZERO);

        let available = space.available_inline.definite().unwrap_or(Au::ZERO);
        let replaced = layout_box.kind == BoxKind::Replaced;
        let inline_level = layout_box.inline_level;
        let (content_width, height) = if replaced {
            let (w, h) = replaced_size(
                layout_box.natural,
                (width, height),
                (min_width, max_width),
                (min_height, max_height),
            );
            (w, Some(h))
        } else if let Some(stretch) = space.stretch_inline {
            // The parent already applied the limits.
            ((stretch - edges_x).max(Au::ZERO), height)
        } else {
            let width = width.unwrap_or_else(|| {
                let fill = (available
                    - margin_left.unwrap_or(Au::ZERO)
                    - margin_right.unwrap_or(Au::ZERO)
                    - edges_x)
                    .max(Au::ZERO);
                match (
                    inline_level
                        || layout_box.float.is_some()
                        || layout_box.kind == BoxKind::TableWrapper,
                    intrinsic,
                ) {
                    // Shrink to fit (CSS 2.2 sections 10.3.9, 10.3.5 and
                    // 17.5.2).
                    (true, Some((min, max))) => fill.max(min).min(max),
                    _ => fill,
                }
            });
            (clamp(width, min_width, max_width), height)
        };
        let mut content_width = match (replaced, space.stretch_inline) {
            (true, Some(stretch)) => (stretch - edges_x).max(Au::ZERO),
            _ => content_width,
        };
        let mut height = space
            .stretch_block
            .map(|stretch| (stretch - edges_y).max(Au::ZERO))
            .or(height);

        // `aspect-ratio`: when exactly one dimension is automatic, derive it
        // from the explicit ratio (a replaced element's `auto` natural ratio
        // is already applied by `replaced_size`). Clamp by the min/max limits
        // so the ratio never forces a box past them.
        if width_auto ^ height_auto
            && let Some((rw, rh)) = style
                .size
                .aspect_ratio
                .ratio
                .filter(|&(w, h)| w > 0.0 && h > 0.0)
        {
            let scale = rw / rh;
            if width_auto {
                // Height is definite: width = height * ratio.
                let mut w = Au::from_px(height.unwrap_or(Au::ZERO).to_f32_px() * scale);
                w = clamp(w, min_width, max_width);
                content_width = w;
            } else {
                // Width is definite: height = width / ratio.
                let mut h = Au::from_px(content_width.to_f32_px() / scale);
                h = clamp(h, min_height, max_height);
                height = Some(h);
            }
        }

        let is_absolute = layout_box.is_absolute();
        let (left, right) =
            if inline_level || is_flex_item || is_absolute || layout_box.float.is_some() {
                // `auto` margins of an inline level box are zero; a flex item's
                // and an absolute box's are resolved by whatever places it. A
                // float does not center: auto horizontal margins resolve to zero
                // (CSS 2.2 section 10.3.5), its width stays definite.
                (
                    margin_left.unwrap_or(Au::ZERO),
                    margin_right.unwrap_or(Au::ZERO),
                )
            } else {
                used_margins(
                    available,
                    content_width + edges_x,
                    margin_left,
                    margin_right,
                )
            };

        let is_scroll_container = style.display_box.is_scroll_container();
        // `scroll` and `auto` are user scrollable; `hidden` and `clip` are
        // not (spec 0009, child 0002, AC-6 and AC-7).
        let user_scrollable = [
            matches!(
                style.display_box.overflow_x,
                css::Overflow::Scroll | css::Overflow::Auto
            ),
            matches!(
                style.display_box.overflow_y,
                css::Overflow::Scroll | css::Overflow::Auto
            ),
        ];
        let is_flex = layout_box.kind == BoxKind::FlexContainer;
        let starts_context = is_root
            || inline_level
            || is_scroll_container
            || style.display_box.display == css::Display::FlowRoot
            || replaced
            || is_flex
            || layout_box.kind == BoxKind::GridContainer
            || layout_box.kind == BoxKind::TableCell
            || layout_box.kind == BoxKind::TableCaption
            || is_flex_item
            || is_absolute
            || layout_box.float.is_some();
        let top_adjoining = !starts_context && border.top == Au::ZERO && padding.top == Au::ZERO;
        let bottom_open =
            !starts_context && border.bottom == Au::ZERO && padding.bottom == Au::ZERO;
        // The exclusion space this box passes to its children: a BFC root
        // starts fresh, otherwise it inherits its parent's floats, given
        // from its border box top and moved here to its content box corner
        // (spec 0009, child 0001). Computed before `space` is moved into the
        // struct.
        let exclusions = if starts_context {
            ExclusionSpace::empty()
        } else {
            space
                .exclusions
                .as_deref()
                .map(|e| {
                    e.translated(Point::new(
                        -(left + border.left + padding.left),
                        -(border.top + padding.top),
                    ))
                })
                .unwrap_or_default()
        };
        let relative_offset = positioned::relative_offset(style, space.percentage_basis);
        // A sticky box keeps its insets inside its nearest scroll frame's
        // scrollport; resolve them against the containing block's size
        // (spec 0009, child 0003, AC-11).
        let sticky_insets = if layout_box.is_sticky() {
            let insets = &style.position_offsets;
            let resolve_inset = |length: Option<Length>, basis: Option<Au>| resolve(length, basis);
            Some(SideOffsets {
                top: resolve_inset(insets.top.clone(), space.percentage_basis.height),
                right: resolve_inset(insets.right.clone(), space.percentage_basis.width),
                bottom: resolve_inset(insets.bottom.clone(), space.percentage_basis.height),
                left: resolve_inset(insets.left.clone(), space.percentage_basis.width),
            })
        } else {
            None
        };
        Self {
            id,
            node: layout_box.node,
            style: Rc::clone(style),
            space,
            was_clean: layout_box.flags == crate::BoxFlags::empty(),
            writing_mode: style.inherited_text.writing_mode,
            border,
            padding,
            margins: BlockMargins {
                left,
                right,
                ..BlockMargins::default()
            },
            own_top,
            own_bottom,
            is_scroll_container,
            user_scrollable,
            centers_content: layout_box.centers_content
                && layout_box.kind == BoxKind::BlockContainer,
            content_width,
            height,
            min_height,
            max_height,
            bottom_adjoining: bottom_open && height.is_none(),
            may_collapse_through: top_adjoining
                && bottom_open
                && height.is_none_or(|h| h == Au::ZERO)
                && min_height == Au::ZERO,
            next_child: 0,
            cursor: Au::ZERO,
            pending: MarginStrut::default(),
            at_top: top_adjoining,
            absorbed_top: MarginStrut::default(),
            has_lines: false,
            children: Vec::new(),
            baseline: None,
            last_baseline: None,
            marker: layout_box.marker.clone(),
            relative_offset,
            sticky_insets,
            absolutes: Vec::new(),
            exclusions,
            floats: Vec::new(),
            bfc_root: starts_context,
            float_bottom: Au::ZERO,
        }
    }

    /// The space the box's children (and its inline content) get. Children
    /// inherit the parent's exclusion space and clearance, and their BFC
    /// offset is where they sit (spec 0009, child 0001).
    pub(crate) fn child_space(&self) -> ConstraintSpace {
        ConstraintSpace {
            available_inline: AvailableSize::Definite(self.content_width),
            available_block: self
                .height
                .map_or(AvailableSize::MaxContent, AvailableSize::Definite),
            percentage_basis: Size::new(Some(self.content_width), self.height),
            writing_mode: self.writing_mode,
            stretch_inline: None,
            stretch_block: None,
            bfc_offset: None,
            exclusions: Some(Rc::new(self.exclusions.clone())),
            clearance: self.space.clearance,
        }
    }

    /// The space for an in flow block child: `child_space` with the
    /// exclusion space moved to the child's border box top, where its top
    /// margin would put it. A margin that collapses through it from its
    /// own first child can still move it lower (spec 0009, child 0001).
    pub(crate) fn block_child_space(&self, child: &LayoutBox) -> ConstraintSpace {
        let mut space = self.child_space();
        let margin_top = child
            .style
            .margin
            .top
            .clone()
            .map_or(Au::ZERO, |l| l.resolve(Some(self.content_width)));
        let y = if self.at_top {
            self.cursor
        } else {
            let mut strut = self.pending;
            strut.push(margin_top);
            self.cursor + strut.resolve()
        };
        space.exclusions = Some(Rc::new(
            self.exclusions.translated(Point::new(Au::ZERO, -y)),
        ));
        if let Some((_, width)) = self.float_band(child, y) {
            space.available_inline = AvailableSize::Definite(width);
        }
        space
    }

    /// The free band beside the floats at block offset `y`, as its left edge
    /// and width, for a block child that starts its own formatting context:
    /// its border box must not overlap the floats (CSS 2.2 section 9.5).
    /// `None` for any other child, or with no float beside it.
    fn float_band(&self, child: &LayoutBox, y: Au) -> Option<(Au, Au)> {
        let starts_context = child.style.display_box.display == css::Display::FlowRoot
            || child.style.display_box.is_scroll_container()
            || matches!(
                child.kind,
                BoxKind::FlexContainer | BoxKind::GridContainer | BoxKind::TableWrapper
            );
        if !starts_context || child.float.is_some() || child.is_absolute() {
            return None;
        }
        let band = self
            .exclusions
            .opportunity(y, self.content_width, Au::ZERO)
            .rect;
        (band.size.width < self.content_width).then_some((band.origin.x, band.size.width))
    }

    /// The sizes a flex container lays its items out in.
    pub(crate) fn flex_frame(&self) -> FlexFrame {
        FlexFrame {
            content_width: self.content_width,
            height: self.height,
            min_height: self.min_height,
            max_height: self.max_height,
        }
    }

    /// Takes a flex container's laid out items as its content. They are
    /// all placed, so the layout stack visits no children.
    pub(crate) fn set_flex_items(&mut self, items: FlexItems, child_count: usize) {
        let origin = self.content_origin();
        self.children = items
            .children
            .into_iter()
            .map(|(at, fragment)| (origin.offset_by(at), fragment))
            .collect();
        self.baseline = items.baseline.map(|b| origin.y + b);
        self.last_baseline = items.last_baseline.map(|b| origin.y + b);
        self.cursor = items.height;
        self.next_child = child_count;
        self.absolutes
            .extend(items.absolutes.into_iter().map(|a| a.offset_by(origin)));
    }

    /// The sizes a grid container lays its items out in.
    pub(crate) fn grid_frame(&self) -> GridFrame {
        GridFrame {
            content_width: self.content_width,
            height: self.height,
            min_height: self.min_height,
            max_height: self.max_height,
        }
    }

    /// Takes a grid container's laid out items as its content. They are
    /// all placed, so the layout stack visits no children.
    pub(crate) fn set_grid_items(&mut self, items: GridItems, child_count: usize) {
        let origin = self.content_origin();
        self.children = items
            .children
            .into_iter()
            .map(|(at, fragment)| (origin.offset_by(at), fragment))
            .collect();
        self.baseline = items.baseline.map(|b| origin.y + b);
        self.last_baseline = items.last_baseline.map(|b| origin.y + b);
        self.cursor = items.height;
        self.next_child = child_count;
        self.absolutes
            .extend(items.absolutes.into_iter().map(|a| a.offset_by(origin)));
    }

    fn content_origin(&self) -> Point {
        Point::new(
            self.border.left + self.padding.left,
            self.border.top + self.padding.top,
        )
    }

    /// Takes the box's laid out lines as its content.
    pub(crate) fn set_lines(&mut self, lines: Lines) {
        let origin = self.content_origin();
        self.children = lines
            .fragments
            .into_iter()
            .map(|(at, fragment)| (origin.offset_by(at), fragment))
            .collect();
        self.baseline = lines.first_baseline.map(|b| origin.y + b);
        self.last_baseline = lines.last_baseline.map(|b| origin.y + b);
        self.cursor = lines.height;
        self.has_lines = lines.has_content;
        if self.has_lines {
            self.at_top = false;
        }
        self.absolutes
            .extend(lines.absolutes.into_iter().map(|a| a.offset_by(origin)));
    }

    /// Records an absolute child's static position: where the next block's
    /// top margin edge would be (spec 0006, Value sourcing, Static
    /// position). A fixed child does the same, but hoists to the viewport's
    /// containing block instead of its nearest positioned ancestor
    /// (spec 0009, child 0003, AC-10).
    pub(crate) fn place_absolute(&mut self, id: BoxId, fixed: bool) {
        let origin = self.content_origin();
        let y = if self.at_top {
            self.cursor
        } else {
            self.cursor + self.pending.resolve()
        };
        self.absolutes.push(PendingAbsolute {
            box_id: id,
            static_position: Point::new(origin.x, origin.y + y),
            fixed,
            containing_block: None,
        });
    }

    /// Adds a placed child, moved by its relative offset, and takes over
    /// the absolute descendants it hands on.
    fn push_child(&mut self, at: Point, fragment: Rc<Fragment>) {
        if let Some(child) = fragment.as_box() {
            let at = at.offset_by(child.relative_offset);
            self.absolutes
                .extend(child.absolutes.iter().map(|a| a.offset_by(at)));
            self.children.push((at, fragment));
        }
    }

    /// Places a finished child below the ones before it, collapsing its
    /// top margin with the margins before it. `clearance` shifts the box
    /// down past `clear` (CSS 2.2 section 9.5.2; spec 0009, child 0001).
    pub(crate) fn place(&mut self, tree: &BoxTree, fragment: Rc<Fragment>) {
        let Some(child) = fragment.as_box() else {
            return;
        };
        let origin = self.content_origin();
        let margins = child.margins;
        let x = origin.x + margins.left;
        self.pending.append(margins.top);
        if margins.collapses_through {
            // An empty block: its margins join the ones around it. It sits
            // where the next block's border edge would.
            let y = if self.at_top {
                self.cursor
            } else {
                self.cursor + self.pending.resolve()
            };
            self.pending.append(margins.bottom);
            self.take_floats(child, Point::new(margins.left, y));
            self.push_child(Point::new(x, origin.y + y), fragment);
            return;
        }
        let mut y = if self.at_top {
            // The margins so far collapse into this box's own top margin.
            self.absorbed_top.append(self.pending);
            self.at_top = false;
            self.cursor
        } else {
            self.cursor + self.pending.resolve()
        };
        // `clear`: push the box's top border edge below the matching floats
        // (CSS 2.2 section 9.5.2).
        if let Some(clear) = tree
            .get(child.box_id)
            .map(|b| b.style.display_box.clear)
            .filter(|clear| *clear != Clear::None)
        {
            let clearance = match clear {
                Clear::Left => self.exclusions.clearance(FloatSide::Left),
                Clear::Right => self.exclusions.clearance(FloatSide::Right),
                Clear::Both => Some(self.exclusions.clearance_both()),
                Clear::None => None,
            };
            if let Some(clearance) = clearance.filter(|&c| c > y) {
                y = clearance;
                // Clearance stops margins collapsing through this box.
                self.bottom_adjoining = false;
            }
        }
        if self.baseline.is_none() {
            self.baseline = child.baseline.map(|b| origin.y + y + b);
        }
        if let Some(last) = child.last_baseline {
            self.last_baseline = Some(origin.y + y + last);
        }
        // A child that starts its own formatting context sits in the band
        // beside the floats it was sized for.
        let x = tree
            .get(child.box_id)
            .and_then(|b| self.float_band(b, y))
            .map_or(x, |(left, _)| x + left);
        self.cursor = y + child.size.height;
        self.pending = margins.bottom;
        self.take_floats(child, Point::new(margins.left, y));
        self.push_child(Point::new(x, origin.y + y), fragment);
    }

    /// Adds the floats a block child placed inside it to this box's
    /// exclusion space, so later siblings flow around them. `at` is the
    /// child's border box corner from this box's content box. The child's
    /// space starts with this box's floats, in order, so the ones after
    /// them are its own (spec 0009, child 0001).
    fn take_floats(&mut self, child: &BoxFragment, at: Point) {
        let Some(out) = &child.exclusions_out else {
            return;
        };
        let known = self.exclusions.exclusions.len();
        for exclusion in out.exclusions.iter().skip(known) {
            self.exclusions = self.exclusions.add(Exclusion {
                rect: Rect::new(exclusion.rect.origin.offset_by(at), exclusion.rect.size),
                side: exclusion.side,
            });
        }
    }

    /// Places a floated child out of flow, beside the floats already placed,
    /// and records its exclusion so later lines are narrowed (spec 0009,
    /// child 0001). The float takes no room in flow.
    pub(crate) fn place_float(
        &mut self,
        tree: &mut BoxTree,
        child_id: BoxId,
        cx: &mut LayoutContext<'_>,
    ) -> Option<()> {
        let layout_box = tree.get(child_id)?;
        let float_side = layout_box.float?;
        let float_space = ConstraintSpace {
            available_inline: AvailableSize::Definite(self.content_width),
            available_block: AvailableSize::MaxContent,
            percentage_basis: Size::new(Some(self.content_width), None),
            writing_mode: self.writing_mode,
            stretch_inline: None,
            stretch_block: None,
            // A float is a BFC root: its own children never see the outer
            // floats.
            bfc_offset: None,
            exclusions: Some(Rc::new(ExclusionSpace::empty())),
            clearance: None,
        };
        let fragment = lay_out_box(tree, child_id, float_space, cx)?;
        let box_fragment = fragment.as_box()?;
        let margin_box_width =
            box_fragment.margins.left + box_fragment.size.width + box_fragment.margins.right;
        let margin_box_height = box_fragment.margins.top.resolve()
            + box_fragment.size.height
            + box_fragment.margins.bottom.resolve();
        let origin = self.content_origin();
        let opp = self
            .exclusions
            .opportunity(self.cursor, self.content_width, margin_box_width);
        // A left float sits at the left edge of the free band; a right float
        // sits at its right edge (CSS 2.2 section 9.5.1).
        let float_x = match float_side {
            FloatSide::Left => opp.rect.origin.x,
            FloatSide::Right => {
                (opp.rect.origin.x + opp.rect.size.width - margin_box_width).max(Au::ZERO)
            }
        };
        // The exclusion is the margin box; the fragment sits at its border
        // box, inset by the left and top margins.
        let at = origin.offset_by(Point::new(
            float_x + box_fragment.margins.left,
            opp.rect.origin.y + box_fragment.margins.top.resolve(),
        ));
        self.floats.push((at, Rc::clone(&fragment)));
        self.push_child(at, fragment);
        self.exclusions = self.exclusions.add(Exclusion {
            rect: Rect::new(
                Point::new(float_x, opp.rect.origin.y),
                Size::new(margin_box_width, margin_box_height),
            ),
            side: float_side,
        });
        self.float_bottom = self.float_bottom.max(opp.rect.origin.y + margin_box_height);
        Some(())
    }

    /// Finishes the box: its height, its margins as its parent sees them,
    /// its list marker, and its overflow. Absolute boxes it holds for its
    /// descendants are still waiting in the fragment.
    pub(crate) fn finish(mut self, fonts: &mut Fonts<'_>) -> BoxFragment {
        let mut top = MarginStrut::new(self.own_top);
        let mut bottom = MarginStrut::new(self.own_bottom);
        let mut collapses_through = false;
        let auto_height = if self.at_top {
            // Nothing with content inside: whatever margins the empty
            // children had adjoin this box's top margin.
            top.append(self.pending);
            if self.may_collapse_through && !self.has_lines {
                collapses_through = true;
            }
            self.cursor
        } else {
            top.append(self.absorbed_top);
            if self.bottom_adjoining {
                bottom = self.pending;
                bottom.push(self.own_bottom);
                self.cursor
            } else {
                self.cursor + self.pending.resolve()
            }
        };
        // A BFC root's `height: auto` grows to hold its floats
        // (CSS 2.2 section 10.6.7; spec 0009, child 0001).
        let auto_height = if self.bfc_root {
            // Floats its children placed count too.
            auto_height
                .max(self.float_bottom)
                .max(self.exclusions.clearance_both())
        } else {
            auto_height
        };
        let content_height = clamp(
            self.height.unwrap_or(auto_height),
            self.min_height,
            self.max_height,
        )
        .max(Au::ZERO);
        if self.centers_content && content_height > auto_height {
            self.center_content((content_height - auto_height).scale_by(0.5));
        }
        let size = Size::new(
            self.content_width + self.padding.horizontal() + self.border.horizontal(),
            content_height + self.padding.vertical() + self.border.vertical(),
        );
        self.margins.top = top;
        self.margins.bottom = bottom;
        self.margins.collapses_through = collapses_through;
        self.add_marker(fonts);
        let scrollable_overflow = self.scrollable_overflow(size);
        // A sticky box keeps its insets inside its nearest scroll frame's
        // scrollport; the containing block is the space it was laid out in
        // (spec 0009, child 0003, AC-11).
        let sticky = self.sticky_insets.map(|insets| {
            let cb = Rect::new(
                Point::default(),
                Size::new(
                    self.space.available_inline.definite().unwrap_or(size.width),
                    self.space.available_block.definite().unwrap_or(size.height),
                ),
            );
            StickyInfo {
                insets,
                containing_block: cb,
            }
        });
        BoxFragment {
            box_id: self.id,
            node: self.node,
            size,
            children: self.children,
            scrollable_overflow,
            baseline: self.baseline,
            last_baseline: self.last_baseline,
            is_scroll_container: self.is_scroll_container,
            user_scrollable: self.user_scrollable,
            margins: self.margins,
            padding: self.padding,
            inline_edges: None,
            auto_height,
            relative_offset: self.relative_offset,
            sticky,
            absolutes: self.absolutes,
            // A BFC root keeps its floats; any other box hands them on
            // from its border box corner.
            exclusions_out: (!self.bfc_root).then(|| {
                Rc::new(self.exclusions.translated(Point::new(
                    self.border.left + self.padding.left,
                    self.border.top + self.padding.top,
                )))
            }),
            collapsed_borders: None,
            background_layers: None,
        }
    }

    /// Moves the placed content, its baselines and its absolute
    /// descendants' static positions down by `shift`.
    fn center_content(&mut self, shift: Au) {
        let by = Point::new(Au::ZERO, shift);
        for (at, _) in &mut self.children {
            *at = at.offset_by(by);
        }
        self.baseline = self.baseline.map(|b| b + shift);
        self.last_baseline = self.last_baseline.map(|b| b + shift);
        for absolute in &mut self.absolutes {
            *absolute = absolute.offset_by(by);
        }
    }

    /// Sets a list item's marker outside its first line: to the left of the
    /// content box with a gap of half an em, on the first line's baseline
    /// (spec 0004, child 0004, List markers). The marker takes no space.
    fn add_marker(&mut self, fonts: &mut Fonts<'_>) {
        let (Some(marker), Some(node)) = (self.marker.take(), self.node) else {
            return;
        };
        let style = &self.style;
        let runs = fonts.shape(&marker, &font_descriptor(style), style.font.size);
        let width: Au = runs.iter().map(|run| run.width()).sum();
        let origin = self.content_origin();
        let ascent = runs.first().map_or(Au::ZERO, |run| run.metrics.ascent);
        let baseline = self.baseline.unwrap_or(origin.y + ascent);
        let mut x = origin.x - style.font.size.scale_by(0.5) - width;
        for run in &runs {
            let fragment: TextFragment =
                text_fragment(node, &marker, style, run, 0..run.glyphs.len());
            let at = Point::new(x, baseline - fragment.baseline);
            x += fragment.size.width;
            self.children.push((at, Rc::new(Fragment::Text(fragment))));
        }
    }

    /// What the box and its children cover. A child that scrolls its own
    /// overflow counts with its border box only. A scroll container starts
    /// from its padding box and adds its end padding after the children,
    /// so its scroll range ends where the content plus padding ends.
    fn scrollable_overflow(&self, size: Size) -> Rect {
        let children = self.children.iter().map(|(at, child)| {
            let area = overflow_of(child);
            Rect::new(at.offset_by(area.origin), area.size)
        });
        let children = children.reduce(union);
        if !self.is_scroll_container {
            let border_box = Rect::new(Point::default(), size);
            return children.map_or(border_box, |c| union(border_box, c));
        }
        let padding_box = Rect::new(
            Point::new(self.border.left, self.border.top),
            Size::new(
                size.width - self.border.horizontal(),
                size.height - self.border.vertical(),
            ),
        );
        let Some(children) = children else {
            return padding_box;
        };
        let padded = Rect::new(
            children.origin,
            Size::new(
                children.size.width + self.padding.right,
                children.size.height + self.padding.bottom,
            ),
        );
        union(padding_box, padded)
    }
}

/// The area a fragment covers, from its own top left corner, as its
/// parent's overflow counts it.
pub(crate) fn overflow_of(fragment: &Fragment) -> Rect {
    match fragment {
        Fragment::Box(child) if !child.is_scroll_container => child.scrollable_overflow,
        Fragment::Line(line) => line
            .children
            .iter()
            .map(|(at, child)| {
                let area = overflow_of(child);
                Rect::new(at.offset_by(area.origin), area.size)
            })
            .fold(Rect::new(Point::default(), line.size), union),
        child => Rect::new(Point::default(), child.size()),
    }
}

/// Children still to visit for [`transformed_overflow`], the matrix from
/// their space to the starting box's (`None` while a plain offset) and that
/// offset.
type OverflowWork<'a> = (&'a [(Point, Rc<Fragment>)], Option<[f32; 16]>, Point);

/// What the transformed descendants of `fragment` cover, from its border
/// box corner, or `None` when none of them has a transform. Each one counts
/// with the bounding rectangle of its overflow mapped through its matrix,
/// perspective divided; a scroll container below adds only its own box.
/// Paint adds this to a scroll container's scrollable overflow, since a
/// transform change is paint damage only (spec 0010, AC-22).
pub fn transformed_overflow(fragment: &BoxFragment, boxes: &BoxTree) -> Option<Rect> {
    let mut covered: Option<Rect> = None;
    // Each entry: a fragment's children, and the matrix from their space to
    // `fragment`'s border box space (`None` while it is a plain offset).
    let mut stack: Vec<OverflowWork<'_>> = vec![(&fragment.children, None, Point::default())];
    while let Some((children, outer, offset)) = stack.pop() {
        for (at, child) in children {
            let at = offset.offset_by(*at);
            match &**child {
                Fragment::Box(child_box) => {
                    let matrix = boxes
                        .get(child_box.box_id)
                        .and_then(|b| b.style.transform.matrix(child_box.size))
                        .filter(|_| child_box.inline_edges.is_none());
                    let space = match (matrix, outer) {
                        (Some(m), _) => {
                            let placed = mat4_mul(
                                &m,
                                &mat4_translate(at.x.to_f32_px(), at.y.to_f32_px(), 0.0),
                            );
                            let to_outer = outer.map_or(placed, |o| mat4_mul(&placed, &o));
                            let area = if child_box.is_scroll_container {
                                Rect::new(Point::default(), child_box.size)
                            } else {
                                child_box.scrollable_overflow
                            };
                            if let Some(bounds) = mapped_bounds(&to_outer, area) {
                                covered = Some(covered.map_or(bounds, |c| union(c, bounds)));
                            }
                            Some(to_outer)
                        }
                        (None, Some(o)) => Some(mat4_mul(
                            &mat4_translate(at.x.to_f32_px(), at.y.to_f32_px(), 0.0),
                            &o,
                        )),
                        (None, None) => None,
                    };
                    if !child_box.is_scroll_container {
                        let origin = if space.is_some() {
                            Point::default()
                        } else {
                            at
                        };
                        stack.push((&child_box.children, space, origin));
                    }
                }
                Fragment::Line(line) => stack.push((&line.children, outer, at)),
                Fragment::Text(_) => {}
            }
        }
    }
    covered
}

/// The bounding rectangle of `rect` mapped through `m`, or `None` when a
/// corner falls behind the viewer.
fn mapped_bounds(m: &[f32; 16], rect: Rect) -> Option<Rect> {
    let (x0, y0) = (rect.origin.x.to_f32_px(), rect.origin.y.to_f32_px());
    let (x1, y1) = (
        x0 + rect.size.width.to_f32_px(),
        y0 + rect.size.height.to_f32_px(),
    );
    let mut bounds = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    for (x, y) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
        let w = m[3] * x + m[7] * y + m[15];
        if w <= 0.0 {
            return None;
        }
        let px = (m[0] * x + m[4] * y + m[12]) / w;
        let py = (m[1] * x + m[5] * y + m[13]) / w;
        bounds = [
            bounds[0].min(px),
            bounds[1].min(py),
            bounds[2].max(px),
            bounds[3].max(py),
        ];
    }
    Some(Rect::new(
        Point::new(Au::from_px(bounds[0]), Au::from_px(bounds[1])),
        Size::new(
            Au::from_px(bounds[2] - bounds[0]),
            Au::from_px(bounds[3] - bounds[1]),
        ),
    ))
}

/// The smallest rectangle covering both.
pub fn union(a: Rect, b: Rect) -> Rect {
    let left = a.origin.x.min(b.origin.x);
    let top = a.origin.y.min(b.origin.y);
    let right = (a.origin.x + a.size.width).max(b.origin.x + b.size.width);
    let bottom = (a.origin.y + a.size.height).max(b.origin.y + b.size.height);
    Rect::new(Point::new(left, top), Size::new(right - left, bottom - top))
}

/// The used left and right margins of a block level box whose border box
/// is `border_box` wide in a containing block `available` wide (CSS 2.2
/// section 10.3.3). `None` is `auto`. Both `auto` centers the box; one
/// `auto` takes what is left; none, and the right margin gives way.
fn used_margins(available: Au, border_box: Au, left: Option<Au>, right: Option<Au>) -> (Au, Au) {
    let fixed = left.unwrap_or(Au::ZERO) + right.unwrap_or(Au::ZERO);
    // A box wider than its containing block treats `auto` margins as zero.
    let (left, right) = if border_box + fixed > available {
        (
            Some(left.unwrap_or(Au::ZERO)),
            Some(right.unwrap_or(Au::ZERO)),
        )
    } else {
        (left, right)
    };
    let remaining = available - border_box;
    match (left, right) {
        (None, None) => {
            let half = Au(remaining.0 / 2);
            (half, remaining - half)
        }
        (None, Some(right)) => (remaining - right, right),
        (Some(left), _) => (left, remaining - left),
    }
}

/// A replaced box's content size (CSS 2.2 sections 10.3.2 and 10.6.2):
/// `width` and `height` where set, the other kept to the natural aspect
/// ratio, else the natural size, else zero; then the limits.
fn replaced_size(
    natural: NaturalSize,
    (width, height): (Option<Au>, Option<Au>),
    (min_width, max_width): (Au, Option<Au>),
    (min_height, max_height): (Au, Option<Au>),
) -> (Au, Au) {
    let ratio = match (natural.width, natural.height) {
        (Some(w), Some(h)) if w > Au::ZERO && h > Au::ZERO => Some(w.to_f32_px() / h.to_f32_px()),
        _ => None,
    };
    let natural_width = natural.width.unwrap_or(Au::ZERO);
    let natural_height = natural.height.unwrap_or(Au::ZERO);
    let (w, h) = match (width, height) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, ratio.map_or(natural_height, |r| w.scale_by(1.0 / r))),
        (None, Some(h)) => (ratio.map_or(natural_width, |r| h.scale_by(r)), h),
        (None, None) => (natural_width, natural_height),
    };
    (
        clamp(w, min_width, max_width),
        clamp(h, min_height, max_height),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(px: f32) -> Au {
        Au::from_px(px)
    }

    #[test]
    fn auto_margins_center_and_overflow_gives_way_on_the_right() {
        let available = px(100.0);
        assert_eq!(
            used_margins(available, px(60.0), None, None),
            (px(20.0), px(20.0))
        );
        assert_eq!(
            used_margins(available, px(60.0), Some(px(10.0)), None),
            (px(10.0), px(30.0))
        );
        assert_eq!(
            used_margins(available, px(60.0), None, Some(px(10.0))),
            (px(30.0), px(10.0))
        );
        // Over constrained: the right margin absorbs the difference.
        assert_eq!(
            used_margins(available, px(60.0), Some(px(10.0)), Some(px(10.0))),
            (px(10.0), px(30.0))
        );
        // Too wide: `auto` counts as zero.
        assert_eq!(
            used_margins(available, px(120.0), None, None),
            (Au::ZERO, px(-20.0))
        );
    }

    #[test]
    fn replaced_sizes_keep_the_natural_ratio() {
        let natural = NaturalSize {
            width: Some(px(200.0)),
            height: Some(px(100.0)),
        };
        let free = ((Au::ZERO, None), (Au::ZERO, None));
        let size = |w, h| replaced_size(natural, (w, h), free.0, free.1);
        assert_eq!(size(None, None), (px(200.0), px(100.0)));
        assert_eq!(size(Some(px(50.0)), None), (px(50.0), px(25.0)));
        assert_eq!(size(None, Some(px(50.0))), (px(100.0), px(50.0)));
        let unknown = replaced_size(NaturalSize::default(), (None, None), free.0, free.1);
        assert_eq!(unknown, (Au::ZERO, Au::ZERO));
        let limited = replaced_size(natural, (None, None), (Au::ZERO, Some(px(120.0))), free.1);
        assert_eq!(limited.0, px(120.0));
    }

    #[test]
    fn struts_take_the_largest_and_most_negative_margins() {
        let mut strut = MarginStrut::new(px(10.0));
        strut.push(px(20.0));
        assert_eq!(strut.resolve(), px(20.0));
        strut.push(px(-5.0));
        assert_eq!(strut.resolve(), px(15.0));
        strut.append(MarginStrut::new(px(-8.0)));
        assert_eq!(strut.resolve(), px(12.0));
    }
}
