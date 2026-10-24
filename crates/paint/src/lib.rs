//! Paint: turns a fragment tree into a display list (spec 0002, Display list).
//!
//! Layout lengths are `Au`; this is where they become `f32` CSS px. Paint
//! emits backgrounds and borders (rounded by `border-radius`), groups a
//! stacking context with `opacity` below 1, clips content that overflows a
//! box whose `overflow` is not `visible` to its rounded padding box, and
//! gives each scroll container a scroll frame. Each text fragment becomes a
//! `Text` item, with an underline under it when its style asks for one
//! (spec 0004, child 0005), and each outline a `Border` item outside its
//! box (spec 0006, AC-8). Items follow the paint order
//! [`layout::stacking`] computes, the one hit testing uses too (spec 0006,
//! AC-6). The root's background, or `body`'s, covers the whole canvas
//! (AC-23). A decoded `<img>` draws its picture over its content box, and
//! a `background-image` draws once at the padding box's top left, cut to
//! the border box (AC-29).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::rc::Rc;

use base::{Au, Id, Point, Size};
use css::Position;
use display_list::{
    BlendMode, BorderStyle, ClipId, ClipNode, ColorF, DisplayItem, DisplayList, Epoch,
    GlyphInstance, ImageKey, ItemKind, PageId, PointF, RectF, ScrollId, ScrollOffset, SizeF,
    SpatialId, SpatialKind, sticky_offset,
};
use layout::stacking::{self, Placed, Step, Walker};
use layout::{BoxFragment, BoxTree, FragmentTree, InlineEdges, TextFragment};

/// A decoded picture a `background-image` can draw.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Picture {
    /// The key it was registered under with `AddImage`.
    pub key: ImageKey,
    /// Its natural size, in CSS px (one image pixel each).
    pub size: SizeF,
}

/// Everything paint needs besides the trees.
pub struct PaintContext<'a> {
    /// The page the list is for.
    pub page: PageId,
    /// The list's epoch.
    pub epoch: Epoch,
    /// The frame's id, packed into its scroll ids.
    pub frame: u32,
    /// The page's scroll table: each scroll container's offset and
    /// generation. A container missing from it is at the top left.
    pub scroll_offsets: &'a HashMap<ScrollId, ScrollOffset>,
    /// The decoded pictures by absolute URL, for `background-image`. A URL
    /// missing from it is still loading, or failed, and draws nothing.
    pub pictures: &'a HashMap<Rc<str>, Picture>,
    /// The focused text input's caret and selection, if any.
    pub edit: Option<EditPaint>,
    /// The roots of the overlay layer, by [`scroll_id`] (spec 0008,
    /// Decision): what a stacking context rooted at one paints goes into
    /// [`Layers::overlay`]. Empty for pages.
    pub overlay: &'a [ScrollId],
}

/// A painted document in two layers (spec 0008, Interface surface, Paint
/// and render): the overlay is drawn after the page, the base before it.
/// Both share the base's spatial nodes and clips, so either list's ids
/// stay valid.
#[derive(Debug, Clone, PartialEq)]
pub struct Layers {
    /// Everything outside an overlay root.
    pub base: DisplayList,
    /// What the overlay roots paint; no items when there are none.
    pub overlay: DisplayList,
}

/// A text input's caret and selection, drawn with its text (spec 0008,
/// AC-14). Offsets are bytes in its value, on grapheme boundaries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EditPaint {
    /// The input, by its [`scroll_id`]: its text runs carry its node, and
    /// its text scrolls in its own scroll frame.
    pub field: ScrollId,
    /// Where the caret sits; `None` hides it.
    pub caret: Option<usize>,
    /// The selected bytes, `start..end`; empty for no selection.
    pub selection: (usize, usize),
    /// The value's length in bytes.
    pub len: usize,
    /// What selected text sits on.
    pub highlight: ColorF,
}

/// The scroll id of the scroll container `node` in frame `frame`
/// (spec 0002, Value sourcing: Paint). Generic over the arena marker so
/// paint does not need to name `dom`'s node type.
pub fn scroll_id<M>(frame: u32, node: Id<M>) -> ScrollId {
    ScrollId::new(frame, node.index(), node.generation().get())
}

/// Where a fragment draws: its spatial node and clip.
type Space = (SpatialId, Option<ClipId>);

/// Pushes each box's clip and scroll frame for its content as the paint
/// order walk reaches it.
struct PaintWalker<'a, 'b> {
    list: &'b mut DisplayList,
    boxes: &'a BoxTree,
    context: &'a PaintContext<'a>,
}

impl<'a> Walker<'a> for PaintWalker<'a, '_> {
    type State = Space;

    fn children(
        &mut self,
        fragment: &'a BoxFragment,
        origin: Point,
        &(spatial, clip): &Space,
    ) -> Option<(Point, Space)> {
        let Some(style) = self.boxes.get(fragment.box_id).map(|b| &b.style) else {
            return Some((origin, (spatial, clip)));
        };
        // Inline boxes and the root element don't get their own space; the
        // root's overflow is the viewport's.
        if fragment.inline_edges.is_some() || self.boxes.root() == Some(fragment.box_id) {
            return Some((origin, (spatial, clip)));
        }
        let border = layout::border_widths(style);
        let padding_origin = Point::new(origin.x + border.left, origin.y + border.top);
        let padding_size = Size::new(
            fragment.size.width - border.horizontal(),
            fragment.size.height - border.vertical(),
        );
        let port = RectF {
            origin: point_f(padding_origin),
            size: size_f(padding_size),
        };
        // The padding box, rounded inside the border (spec 0006, AC-7).
        let radii = layout::corner_radii(style, fragment.size, None)
            .map(|outer| layout::inner_radii(outer, border))
            .map(|inner| display_list::clamp_radii(port, inner));
        // A `position: sticky` box stays inside its nearest scroll frame:
        // paint pushes a sticky spatial node whose offset the render side
        // applies each frame (spec 0009, child 0003, AC-11).
        if style.display_box.position == Position::Sticky
            && let Some(sticky) = fragment.sticky
        {
            let sticky_rect = RectF {
                origin: point_f(origin),
                size: size_f(fragment.size),
            };
            let margins = [
                sticky.insets.top.map(|t| t.to_f32_px()),
                sticky.insets.right.map(|r| r.to_f32_px()),
                sticky.insets.bottom.map(|b| b.to_f32_px()),
                sticky.insets.left.map(|l| l.to_f32_px()),
            ];
            let cb = sticky.containing_block;
            let margin_box = Size::new(
                fragment.size.width + border.horizontal(),
                fragment.size.height + border.vertical(),
            );
            let limits = (
                (
                    0.0,
                    (cb.size.height.to_f32_px() - margin_box.height.to_f32_px()).max(0.0),
                ),
                (
                    0.0,
                    (cb.size.width.to_f32_px() - margin_box.width.to_f32_px()).max(0.0),
                ),
            );
            let frame_offset = self.nearest_scroll_offset(spatial);
            let offset = sticky_offset(sticky_rect, margins, limits, frame_offset);
            let sticky_spatial = self.list.push_spatial(
                spatial,
                SpatialKind::Sticky {
                    sticky_rect,
                    margins,
                    vertical_limits: limits.0,
                    horizontal_limits: limits.1,
                    offset,
                },
            );
            // Children draw inside the sticky box, shifted by the sticky
            // offset, and clipped by its padding box.
            let clip = Some(self.list.push_clip(ClipNode {
                parent: None,
                spatial: sticky_spatial,
                rect: port,
                radii,
            }));
            return Some((Point::default(), (sticky_spatial, clip)));
        }
        // `clip: rect()` clips absolute and fixed boxes (and their
        // descendants) to a rectangle measured from the border box's top
        // left corner (spec 0009, child 0003, AC-12).
        if (style.display_box.position == Position::Absolute
            || style.display_box.position == Position::Fixed)
            && let Some(clip_rect) = style.effects.clip
        {
            let left = clip_rect.left.unwrap_or(Au::ZERO);
            let top = clip_rect.top.unwrap_or(Au::ZERO);
            let right = clip_rect.right.unwrap_or(fragment.size.width);
            let bottom = clip_rect.bottom.unwrap_or(fragment.size.height);
            let rect = RectF {
                origin: point_f(origin.offset_by(Point::new(left, top))),
                size: size_f(Size::new(
                    (right - left).max(Au::ZERO),
                    (bottom - top).max(Au::ZERO),
                )),
            };
            let clip = Some(self.list.push_clip(ClipNode {
                parent: clip,
                spatial,
                rect,
                radii: None,
            }));
            return Some((origin, (spatial, clip)));
        }
        if !style.display_box.clips_overflow() {
            return Some((origin, (spatial, clip)));
        }
        let clip = Some(self.list.push_clip(ClipNode {
            parent: clip,
            spatial,
            rect: port,
            radii,
        }));
        if !fragment.is_scroll_container {
            return Some((origin, (spatial, clip)));
        }
        let Some(node) = fragment.node else {
            return Some((origin, (spatial, clip)));
        };
        let scroll_id = scroll_id(self.context.frame, node);
        let overflow = fragment.scrollable_overflow;
        let content = Size::new(
            (overflow.origin.x + overflow.size.width - border.left).max(padding_size.width),
            (overflow.origin.y + overflow.size.height - border.top).max(padding_size.height),
        );
        let spatial = self.list.push_spatial(
            spatial,
            SpatialKind::ScrollFrame {
                scroll_id,
                frame_rect: port,
                content_size: size_f(content),
                offset: self
                    .context
                    .scroll_offsets
                    .get(&scroll_id)
                    .copied()
                    .unwrap_or_default(),
                user_scrollable: fragment.user_scrollable,
            },
        );
        // Children are placed relative to the border box; inside the scroll
        // frame they are relative to the padding box.
        Some((Point::new(-border.left, -border.top), (spatial, clip)))
    }
}

/// The current offset of the nearest scroll frame ancestor of `spatial`,
/// walking up the spatial tree. The viewport's own scroll frame is the
/// fallback, so a sticky box at the page root sticks against the viewport
/// (spec 0009, child 0003, AC-11).
impl<'a, 'b> PaintWalker<'a, 'b> {
    fn nearest_scroll_offset(&self, spatial: SpatialId) -> PointF {
        let mut current = Some(spatial);
        for _ in 0..=self.list.spatial.len() {
            let Some(node) = current.and_then(|id| self.list.spatial.get(id.0 as usize)) else {
                break;
            };
            if let SpatialKind::ScrollFrame {
                scroll_id, offset, ..
            } = node.kind
            {
                return self
                    .context
                    .scroll_offsets
                    .get(&scroll_id)
                    .map(|o| PointF { x: o.x, y: o.y })
                    .unwrap_or(PointF {
                        x: offset.x,
                        y: offset.y,
                    });
            }
            current = node.parent;
        }
        PointF::default()
    }
}

/// Writes the steps of the paint order into the list.
struct Painter<'a> {
    list: DisplayList,
    /// The overlay layer's items (spec 0008).
    overlay: Vec<DisplayItem>,
    /// While inside an overlay root: how many stacking contexts were open
    /// around it.
    overlay_from: Option<usize>,
    boxes: &'a BoxTree,
    context: &'a PaintContext<'a>,
    /// The box whose background the canvas took.
    canvas: Option<base::BoxId>,
}

/// Paints `tree` (laid out from `boxes`) into a new list, in the paint
/// order of [`layout::stacking`]: CSS 2.2 Appendix E for what the engine
/// supports. A stacking context with `opacity` below 1 is wrapped in a
/// group.
///
/// Everything hangs off the root viewport's scroll frame, the first
/// spatial node after the root (spec 0004, child 0005, Root scroll frame).
pub fn paint(tree: &FragmentTree, boxes: &BoxTree, context: &PaintContext<'_>) -> DisplayList {
    paint_layers(tree, boxes, context).base
}

/// Paints `tree` like [`paint`], putting what the stacking contexts rooted
/// at `context.overlay` paint into their own list.
pub fn paint_layers(tree: &FragmentTree, boxes: &BoxTree, context: &PaintContext<'_>) -> Layers {
    let mut list = DisplayList::new(context.page, context.epoch, size_f(tree.viewport));
    let viewport = viewport_frame(tree, context, &mut list);
    let canvas = canvas_background(tree, boxes, context.pictures, viewport, &mut list);
    let Some((origin, root)) = &tree.root else {
        let overlay = list.clone();
        return Layers {
            base: list,
            overlay,
        };
    };
    let mut walker = PaintWalker {
        list: &mut list,
        boxes,
        context,
    };
    let root = Placed {
        origin: *origin,
        fragment: root,
        state: (viewport, None),
    };
    let steps = stacking::paint_order(boxes, root, &mut walker);
    let mut painter = Painter {
        list,
        overlay: Vec::new(),
        overlay_from: None,
        boxes,
        context,
        canvas,
    };
    // Whether each open stacking context pushed a group.
    let mut groups: Vec<bool> = Vec::new();
    for step in steps {
        match step {
            Step::Enter {
                fragment,
                state: (spatial, clip),
                ..
            } => {
                let root = fragment
                    .node
                    .is_some_and(|node| context.overlay.contains(&scroll_id(context.frame, node)));
                if root && painter.overlay_from.is_none() {
                    painter.overlay_from = Some(groups.len());
                }
                let opacity = painter
                    .style(fragment)
                    .map_or(1.0, |style| style.effects.opacity);
                let group = opacity < 1.0;
                if group {
                    painter.push(
                        spatial,
                        clip,
                        ItemKind::PushStackingContext {
                            opacity: opacity.max(0.0),
                            blend_mode: BlendMode::Normal,
                        },
                    );
                }
                groups.push(group);
            }
            Step::Leave => {
                if groups.pop() == Some(true) {
                    painter.push(SpatialId::ROOT, None, ItemKind::PopStackingContext);
                }
                if painter.overlay_from == Some(groups.len()) {
                    painter.overlay_from = None;
                }
            }
            Step::Box {
                fragment,
                origin,
                state,
            } => painter.decorations(fragment, origin, state),
            Step::Text {
                fragment,
                origin,
                state: (spatial, clip),
            } => {
                let edit = context
                    .edit
                    .filter(|edit| scroll_id(context.frame, fragment.node) == edit.field);
                if let Some(kind) = edit.and_then(|edit| selection_item(fragment, origin, &edit)) {
                    painter.push(spatial, clip, kind);
                }
                for kind in text_items(fragment, origin) {
                    painter.push(spatial, clip, kind);
                }
                if let Some(kind) = edit.and_then(|edit| caret_item(fragment, origin, &edit)) {
                    painter.push(spatial, clip, kind);
                }
            }
            Step::Outline {
                fragment,
                origin,
                state: (spatial, clip),
            } => {
                if let Some(kind) = painter
                    .style(fragment)
                    .and_then(|style| outline_item(style, fragment, origin))
                {
                    painter.push(spatial, clip, kind);
                }
            }
        }
    }
    let mut overlay = painter.list.clone();
    overlay.items = painter.overlay;
    Layers {
        base: painter.list,
        overlay,
    }
}

impl<'a> Painter<'a> {
    /// The decoded picture of the box's `background-image`, if any.
    fn picture(&self, style: &css::ComputedStyle) -> Option<Picture> {
        let url = style.background.image.as_deref()?;
        self.context.pictures.get(url).copied()
    }

    fn style(&self, fragment: &BoxFragment) -> Option<&'a css::ComputedStyle> {
        self.boxes.get(fragment.box_id).map(|b| &*b.style)
    }

    fn push(&mut self, spatial: SpatialId, clip: Option<ClipId>, kind: ItemKind) {
        let item = DisplayItem {
            spatial,
            clip,
            kind,
        };
        match self.overlay_from {
            Some(_) => self.overlay.push(item),
            None => self.list.items.push(item),
        }
    }

    /// A box's background, background image, border and `<img>` picture,
    /// unless it is hidden.
    fn decorations(&mut self, fragment: &BoxFragment, origin: Point, (spatial, clip): Space) {
        let Some(style) = self.style(fragment) else {
            return;
        };
        if style.inherited_box.visibility != css::Visibility::Visible {
            return;
        }
        let border_box = RectF {
            origin: point_f(origin),
            size: size_f(fragment.size),
        };
        let radii = layout::corner_radii(style, fragment.size, fragment.inline_edges)
            .map(|radii| display_list::clamp_radii(border_box, radii));
        let border = layout::border_widths(style);
        // The canvas already took this box's background.
        if self.canvas != Some(fragment.box_id) {
            let background = style.background.color;
            if background.a > 0.0 {
                let kind = ItemKind::Rect {
                    rect: border_box,
                    color: color_f(background),
                    radii,
                };
                self.push(spatial, clip, kind);
            }
            if let Some(picture) = self.picture(style) {
                // At the padding box's top left, at its natural size, cut
                // to the border box (spec 0004, child 0005, Backgrounds).
                let cut = self.list.push_clip(ClipNode {
                    parent: clip,
                    spatial,
                    rect: border_box,
                    radii,
                });
                let at = Point::new(origin.x + border.left, origin.y + border.top);
                let kind = ItemKind::Image {
                    key: picture.key,
                    rect: RectF {
                        origin: point_f(at),
                        size: picture.size,
                    },
                };
                self.push(spatial, Some(cut), kind);
            }
        }
        if let Some(kind) = border_item(style, border_box, fragment.inline_edges, radii) {
            self.push(spatial, clip, kind);
        }
        // A decoded `<img>` fills its content box.
        if let Some(key) = self.boxes.get(fragment.box_id).and_then(|b| b.image) {
            let padding = fragment.padding;
            let at = Point::new(
                origin.x + border.left + padding.left,
                origin.y + border.top + padding.top,
            );
            let size = Size::new(
                fragment.size.width - border.horizontal() - padding.horizontal(),
                fragment.size.height - border.vertical() - padding.vertical(),
            );
            let kind = ItemKind::Image {
                key: ImageKey(key),
                rect: RectF {
                    origin: point_f(at),
                    size: size_f(size),
                },
            };
            self.push(spatial, clip, kind);
        }
    }
}

/// A box's outline as a `Border` item: its border box grown by the offset
/// plus the width on each side, its nonzero corner radii grown by the same
/// (a square corner stays square), drawn solid in the outline color (spec
/// 0006, AC-8). `None` when it draws nothing.
fn outline_item(
    style: &css::ComputedStyle,
    fragment: &BoxFragment,
    origin: Point,
) -> Option<ItemKind> {
    let outline = &style.outline;
    if outline.width <= Au::ZERO
        || outline.color.a <= 0.0
        || style.inherited_box.visibility != css::Visibility::Visible
    {
        return None;
    }
    let grow = outline.offset + outline.width;
    let rect = RectF {
        origin: point_f(Point::new(origin.x - grow, origin.y - grow)),
        size: SizeF {
            width: (fragment.size.width + grow * 2).to_f32_px().max(0.0),
            height: (fragment.size.height + grow * 2).to_f32_px().max(0.0),
        },
    };
    let grow_px = grow.to_f32_px();
    let radii = layout::corner_radii(style, fragment.size, fragment.inline_edges).map(|radii| {
        let grown = radii.map(|r| if r > 0.0 { (r + grow_px).max(0.0) } else { 0.0 });
        display_list::clamp_radii(rect, grown)
    });
    let width = outline.width.to_f32_px();
    Some(ItemKind::Border {
        rect,
        widths: [width; 4],
        colors: [color_f(outline.color); 4],
        styles: [BorderStyle::Solid; 4],
        radii,
    })
}

/// How far the document reaches: the viewport, and everything the root's
/// content covers.
fn document_extent(tree: &FragmentTree) -> Size {
    let mut extent = tree.viewport;
    if let Some((origin, root)) = &tree.root
        && let Some(root) = root.as_box()
    {
        let overflow = root.scrollable_overflow;
        extent.width = extent
            .width
            .max(origin.x + overflow.origin.x + overflow.size.width);
        extent.height = extent
            .height
            .max(origin.y + overflow.origin.y + overflow.size.height);
    }
    extent
}

/// Pushes the root viewport's scroll frame: the viewport over the whole
/// document, at the offset in the page's scroll table. Its id is the root
/// element's, whose `overflow` is the viewport's. Without a root element,
/// the root space.
fn viewport_frame(
    tree: &FragmentTree,
    context: &PaintContext<'_>,
    list: &mut DisplayList,
) -> SpatialId {
    let Some(node) = tree
        .root
        .as_ref()
        .and_then(|(_, root)| root.as_box())
        .and_then(|root| root.node)
    else {
        return SpatialId::ROOT;
    };
    let scroll_id = scroll_id(context.frame, node);
    list.push_spatial(
        SpatialId::ROOT,
        SpatialKind::ScrollFrame {
            scroll_id,
            frame_rect: RectF {
                origin: PointF::default(),
                size: size_f(tree.viewport),
            },
            content_size: size_f(document_extent(tree)),
            offset: context
                .scroll_offsets
                .get(&scroll_id)
                .copied()
                .unwrap_or_default(),
            user_scrollable: [true, true],
        },
    )
}

/// Paints the canvas background: the root's background, or `body`'s when
/// the root has none, over the viewport and everything the root's content
/// covers, its image once at the canvas origin (CSS 2.2 section 14.2).
/// Returns the box it came from, whose own background is then not painted
/// again.
fn canvas_background(
    tree: &FragmentTree,
    boxes: &BoxTree,
    pictures: &HashMap<Rc<str>, Picture>,
    spatial: SpatialId,
    list: &mut DisplayList,
) -> Option<base::BoxId> {
    let source = boxes.canvas_background_box()?;
    let style = &boxes.get(source)?.style;
    let color = style.background.color;
    let picture = style
        .background
        .image
        .as_deref()
        .and_then(|url| pictures.get(url).copied());
    if (color.a <= 0.0 && picture.is_none())
        || style.inherited_box.visibility != css::Visibility::Visible
    {
        return None;
    }
    let extent = document_extent(tree);
    let canvas = |kind| DisplayItem {
        spatial,
        clip: None,
        kind,
    };
    if color.a > 0.0 {
        list.items.push(canvas(ItemKind::Rect {
            radii: None,
            rect: RectF {
                origin: PointF { x: 0.0, y: 0.0 },
                size: size_f(extent),
            },
            color: color_f(color),
        }));
    }
    if let Some(picture) = picture {
        list.items.push(canvas(ItemKind::Image {
            key: picture.key,
            rect: RectF {
                origin: PointF { x: 0.0, y: 0.0 },
                size: picture.size,
            },
        }));
    }
    Some(source)
}

/// The items for a run whose top left is at `origin`: its `Text` item, and
/// an underline when `text-decoration-line` asks for one, at the font's
/// underline position and thickness (AC-25). Nothing when it draws nothing
/// (hidden, transparent, no glyphs, or waiting for its web font inside the
/// block period, spec 0006 AC-11).
fn text_items(text: &TextFragment, origin: Point) -> Vec<ItemKind> {
    let style = &text.style;
    let color = style.inherited_text.color;
    if style.inherited_box.visibility != css::Visibility::Visible
        || color.a <= 0.0
        || text.glyphs.is_empty()
        || text.hidden_font
    {
        return Vec::new();
    }
    let baseline = origin.y + text.baseline;
    let mut items = vec![ItemKind::Text {
        font: text.font,
        size: text.font_size.to_f32_px(),
        color: color_f(color),
        glyphs: text
            .glyphs
            .iter()
            .map(|glyph| GlyphInstance {
                index: glyph.id,
                point: point_f(Point::new(origin.x + glyph.x, baseline + glyph.y)),
            })
            .collect(),
    }];
    if style.inherited_text.text_decoration_line == css::TextDecorationLine::Underline {
        let top = baseline - text.underline_offset;
        items.push(ItemKind::Rect {
            radii: None,
            rect: RectF {
                origin: point_f(Point::new(origin.x, top)),
                size: SizeF {
                    width: text.size.width.to_f32_px(),
                    // At least a device pixel's worth, so thin underlines
                    // never vanish.
                    height: text.underline_thickness.to_f32_px().max(1.0),
                },
            },
            color: color_f(color),
        });
    }
    items
}

/// Where byte `at` of the shaped text falls in `text`, from its left edge:
/// the first glyph of the cluster at or after it, or the run's end. `None`
/// when `at` is outside the run.
pub fn offset_x(text: &TextFragment, at: usize) -> Option<Au> {
    let start = text.text_start as usize;
    let end = start + text.text.len();
    if !(start..=end).contains(&at) {
        return None;
    }
    if at == end {
        return Some(text.size.width);
    }
    text.glyphs
        .iter()
        .filter(|glyph| glyph.cluster as usize >= at)
        .map(|glyph| glyph.x)
        .min()
        .or(Some(text.size.width))
}

/// The highlight behind the part of `text` the selection covers.
fn selection_item(text: &TextFragment, origin: Point, edit: &EditPaint) -> Option<ItemKind> {
    let start = text.text_start as usize;
    let end = start + text.text.len();
    let (from, to) = edit.selection;
    let (from, to) = (from.max(start), to.min(end));
    if from >= to || edit.len == 0 {
        return None;
    }
    let (left, right) = (offset_x(text, from)?, offset_x(text, to)?);
    Some(ItemKind::Rect {
        radii: None,
        rect: RectF {
            origin: point_f(Point::new(origin.x + left.min(right), origin.y)),
            size: SizeF {
                width: (right - left).to_f32_px().abs(),
                height: text.size.height.to_f32_px(),
            },
        },
        color: edit.highlight,
    })
}

/// The caret: a steady 1 CSS px bar in the text color, as tall as the
/// run, in the one run that holds it (spec 0008, AC-14).
fn caret_item(text: &TextFragment, origin: Point, edit: &EditPaint) -> Option<ItemKind> {
    let caret = edit.caret?;
    let start = text.text_start as usize;
    let end = start + text.text.len();
    // A caret between two runs belongs to the later one; one at the end
    // of the value to the last. An empty value shows its placeholder or a
    // space, and the caret sits at its start.
    let holds = (start..end).contains(&caret) || (caret == end && caret == edit.len);
    if !holds {
        return None;
    }
    let x = if edit.len == 0 {
        Au::ZERO
    } else {
        offset_x(text, caret)?
    };
    Some(ItemKind::Rect {
        radii: None,
        rect: RectF {
            origin: point_f(Point::new(origin.x + x, origin.y)),
            size: SizeF {
                width: 1.0,
                height: text.size.height.to_f32_px(),
            },
        },
        color: color_f(text.style.inherited_text.color),
    })
}

/// The border item for a box, or `None` when no side draws anything. A
/// piece of an inline box split across lines draws its left side only if
/// it holds the box's start edge, and its right side only if it holds the
/// end edge.
fn border_item(
    style: &css::ComputedStyle,
    rect: RectF,
    edges: Option<InlineEdges>,
    radii: Option<[f32; 8]>,
) -> Option<ItemKind> {
    let border = &style.border;
    let cut = |side: &css::BorderSide, kept: bool| {
        if kept {
            *side
        } else {
            css::BorderSide {
                width: Au::ZERO,
                style: css::BorderStyle::None,
                ..*side
            }
        }
    };
    let (start, end) = edges.map_or((true, true), |e| (e.start, e.end));
    let (left, right) = (cut(&border.left, start), cut(&border.right, end));
    let sides = [&border.top, &right, &border.bottom, &left];
    let visible = sides.iter().any(|side| {
        side.width > Au::ZERO
            && side.color.a > 0.0
            && !matches!(
                side.style,
                css::BorderStyle::None | css::BorderStyle::Hidden
            )
    });
    visible.then(|| ItemKind::Border {
        rect,
        widths: sides.map(|side| side.width.to_f32_px()),
        colors: sides.map(|side| color_f(side.color)),
        styles: sides.map(|side| match side.style {
            css::BorderStyle::None => BorderStyle::None,
            css::BorderStyle::Hidden => BorderStyle::Hidden,
            css::BorderStyle::Solid => BorderStyle::Solid,
            css::BorderStyle::Dashed => BorderStyle::Dashed,
            css::BorderStyle::Dotted => BorderStyle::Dotted,
            css::BorderStyle::Double => BorderStyle::Double,
        }),
        radii,
    })
}

fn color_f(color: css::Color) -> ColorF {
    ColorF {
        r: color.r,
        g: color.g,
        b: color.b,
        a: color.a,
    }
}

fn point_f(point: Point) -> PointF {
    PointF {
        x: point.x.to_f32_px(),
        y: point.y.to_f32_px(),
    }
}

fn size_f(size: Size) -> SizeF {
    SizeF {
        width: size.width.to_f32_px(),
        height: size.height.to_f32_px(),
    }
}

#[cfg(test)]
mod tests {
    use base::atoms::QualName;
    use base::local_name;
    use css::{
        Background, BorderSide, Color, ComputedStyle, Display, Float, Length, Margin, Overflow,
        Position,
    };
    use dom::{Document, NodeId};

    use super::*;

    fn style(color: Color, margin: f32, height: Option<f32>) -> ComputedStyle {
        let mut style = ComputedStyle::default();
        Rc::make_mut(&mut style.display_box).display = Display::Block;
        style.background = Rc::new(Background { color, image: None });
        style.margin = Rc::new(Margin::all(Length::px(margin)));
        Rc::make_mut(&mut style.size).height = height.map(Length::px);
        style
    }

    fn add(doc: &mut Document, parent: NodeId, style: ComputedStyle) -> NodeId {
        let id = doc.create_element(QualName::html(local_name!("div")));
        doc.append_child(parent, id).unwrap();
        doc.set_style(id, Some(Rc::new(style))).unwrap();
        id
    }

    fn paint_doc(doc: &mut Document, scroll: &HashMap<ScrollId, ScrollOffset>) -> DisplayList {
        let mut boxes = BoxTree::build(doc);
        let (tree, _) = layout::layout(
            &mut boxes,
            Size::new(Au::from_px(200.0), Au::from_px(100.0)),
            &mut text::FontContext::new(),
            &text::WebFontLayer::default(),
        );
        let pictures = HashMap::new();
        let context = PaintContext {
            page: PageId(1),
            epoch: Epoch(3),
            frame: 0,
            scroll_offsets: scroll,
            pictures: &pictures,
            edit: None,
            overlay: &[],
        };
        paint(&tree, &boxes, &context)
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> RectF {
        RectF {
            origin: PointF { x, y },
            size: SizeF {
                width: w,
                height: h,
            },
        }
    }

    #[test]
    fn backgrounds_become_rects_at_absolute_positions_in_paint_order() {
        let mut doc = Document::new("about:blank");
        let red = Color::rgb8(255, 0, 0);
        let mut parent = doc.root();
        // html (transparent) > body (red, margin 8) > div (blue, height 50)
        for (color, margin, height) in [
            (Color::TRANSPARENT, 0.0, None),
            (red, 8.0, None),
            (Color::rgb8(0, 0, 255), 0.0, Some(50.0)),
        ] {
            parent = add(&mut doc, parent, style(color, margin, height));
        }
        let list = paint_doc(&mut doc, &HashMap::new());

        assert_eq!((list.page, list.epoch), (PageId(1), Epoch(3)));
        assert_eq!(
            list.viewport,
            SizeF {
                width: 200.0,
                height: 100.0
            }
        );
        let rects: Vec<_> = list
            .items
            .iter()
            .filter_map(|item| match item.kind {
                ItemKind::Rect { rect, color, .. } => Some((rect, color.r)),
                _ => None,
            })
            .collect();
        assert_eq!(
            rects,
            [
                (rect(8.0, 8.0, 184.0, 50.0), 1.0),
                (rect(8.0, 8.0, 184.0, 50.0), 0.0)
            ]
        );
        assert_eq!(list.spatial.len(), 2, "only the viewport's frame");
        assert!(list.clips.is_empty());
    }

    #[test]
    fn text_becomes_glyphs_on_its_baseline_in_its_color() {
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let mut body = style(Color::TRANSPARENT, 10.0, None);
        Rc::make_mut(&mut body.inherited_text).color = Color::rgb8(0, 0, 255);
        let body = add(&mut doc, root, body);
        let text = doc.create_text("Hi there");
        doc.append_child(body, text).unwrap();
        let list = paint_doc(&mut doc, &HashMap::new());

        let texts: Vec<_> = list
            .items
            .iter()
            .filter_map(|item| match &item.kind {
                ItemKind::Text {
                    size,
                    color,
                    glyphs,
                    ..
                } => Some((*size, *color, glyphs)),
                _ => None,
            })
            .collect();
        let Some((size, color, glyphs)) = texts.first() else {
            // No system font on this machine.
            return;
        };
        assert_eq!(texts.len(), 1);
        assert_eq!(*size, 16.0);
        assert_eq!(*color, color_f(Color::rgb8(0, 0, 255)));
        assert_eq!(glyphs.len(), 8);
        assert_eq!(glyphs[0].point.x, 10.0, "starts at the body's content edge");
        assert!(glyphs.windows(2).all(|g| g[1].point.x > g[0].point.x));
        assert!(glyphs.iter().all(|g| g.point.y == glyphs[0].point.y));
        assert!(glyphs[0].point.y > 10.0, "the baseline is below the top");
    }

    #[test]
    fn borders_and_opacity_wrap_the_box_they_belong_to() {
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let mut html = style(Color::TRANSPARENT, 0.0, None);
        Rc::make_mut(&mut html.effects).opacity = 0.5;
        let html = add(&mut doc, root, html);
        let mut boxed = style(Color::rgb8(0, 255, 0), 0.0, Some(20.0));
        let side = BorderSide {
            width: Au::from_px(2.0),
            style: css::BorderStyle::Solid,
            color: Color::BLACK,
        };
        boxed.border = Rc::new(css::Border {
            top: side,
            right: side,
            bottom: side,
            left: side,
            radius: Default::default(),
        });
        add(&mut doc, html, boxed);
        let list = paint_doc(&mut doc, &HashMap::new());

        let kinds: Vec<_> = list.items.iter().map(|i| &i.kind).collect();
        assert!(matches!(
            kinds[0],
            ItemKind::PushStackingContext { opacity, .. } if *opacity == 0.5
        ));
        // The border adds 2px a side to the 20px content height.
        assert_eq!(
            kinds[1],
            &ItemKind::Rect {
                rect: rect(0.0, 0.0, 200.0, 24.0),
                color: color_f(Color::rgb8(0, 255, 0)),
                radii: None,
            }
        );
        assert!(matches!(kinds[2], ItemKind::Border { widths, .. } if *widths == [2.0; 4]));
        assert_eq!(kinds[3], &ItemKind::PopStackingContext);
        assert_eq!(kinds.len(), 4);
    }

    #[test]
    fn a_scroll_container_gets_a_clip_and_a_scroll_frame_with_its_offset() {
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let html = add(&mut doc, root, style(Color::TRANSPARENT, 0.0, None));
        let mut scroller = style(Color::TRANSPARENT, 10.0, Some(50.0));
        Rc::make_mut(&mut scroller.display_box).overflow_y = Overflow::Scroll;
        let scroller = add(&mut doc, html, scroller);
        add(&mut doc, scroller, style(Color::BLACK, 0.0, Some(120.0)));

        let id = scroll_id(0, scroller);
        let offset = ScrollOffset {
            x: 0.0,
            y: 30.0,
            generation: 4,
        };
        let list = paint_doc(&mut doc, &HashMap::from([(id, offset)]));

        let port = rect(10.0, 10.0, 180.0, 50.0);
        // The viewport's frame comes first; the scroller's sits in it.
        assert_eq!(list.viewport_frame(), Some(SpatialId(1)));
        assert_eq!(list.spatial[2].parent, Some(SpatialId(1)));
        assert_eq!(
            list.spatial[2].kind,
            SpatialKind::ScrollFrame {
                scroll_id: id,
                frame_rect: port,
                content_size: SizeF {
                    width: 180.0,
                    height: 120.0
                },
                offset,
                user_scrollable: [false, true],
            }
        );
        assert_eq!(list.clips[0].rect, port);
        assert_eq!(list.clips[0].spatial, SpatialId(1));
        // The child is drawn in the scroll frame's space, from its origin.
        let child = list.items.last().unwrap();
        assert_eq!((child.spatial, child.clip), (SpatialId(2), Some(ClipId(0))));
        assert!(matches!(
            child.kind,
            ItemKind::Rect { rect: r, .. } if r == rect(0.0, 0.0, 180.0, 120.0)
        ));
        assert_eq!(
            display_list::validate(&list, PageId(1), &display_list::ResourceKeys::default()),
            Ok(())
        );
    }

    #[test]
    fn the_viewport_scrolls_the_whole_document_and_takes_the_root_overflow() {
        // A 300 px tall document in a 100 px viewport, with `overflow` on
        // the root element: one frame, the viewport's.
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let mut html = style(Color::TRANSPARENT, 0.0, None);
        Rc::make_mut(&mut html.display_box).overflow_y = Overflow::Scroll;
        let html = add(&mut doc, root, html);
        add(&mut doc, html, style(Color::BLACK, 0.0, Some(300.0)));
        let id = scroll_id(0, html);
        let offset = ScrollOffset {
            x: 0.0,
            y: 120.0,
            generation: 0,
        };
        let list = paint_doc(&mut doc, &HashMap::from([(id, offset)]));

        assert_eq!(list.spatial.len(), 2);
        assert_eq!(
            list.spatial[1].kind,
            SpatialKind::ScrollFrame {
                scroll_id: id,
                frame_rect: rect(0.0, 0.0, 200.0, 100.0),
                content_size: SizeF {
                    width: 200.0,
                    height: 300.0
                },
                offset,
                user_scrollable: [true, true],
            }
        );
        assert!(list.clips.is_empty(), "the root clips nothing itself");
        assert!(list.items.iter().all(|item| item.spatial == SpatialId(1)
            || matches!(item.kind, ItemKind::PopStackingContext)));
    }

    /// Each item's kind name and, for rects, its red channel.
    fn kinds(list: &DisplayList) -> Vec<String> {
        list.items
            .iter()
            .map(|item| match &item.kind {
                ItemKind::Rect { color, .. } => format!("rect {}", color.r),
                ItemKind::Border { .. } => "border".into(),
                ItemKind::Text { .. } => "text".into(),
                ItemKind::Image { .. } => "image".into(),
                ItemKind::PushStackingContext { .. } => "push".into(),
                ItemKind::PopStackingContext => "pop".into(),
            })
            .collect()
    }

    #[test]
    fn block_backgrounds_come_before_inline_content_and_opacity_contexts_last() {
        // html > [a (opacity 0.5, red bg), b (text), c (blue bg)]: the
        // blocks' backgrounds, then the text, then the opacity context.
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let html = add(&mut doc, root, style(Color::TRANSPARENT, 0.0, None));
        let mut faded = style(Color::rgb8(255, 0, 0), 0.0, Some(10.0));
        Rc::make_mut(&mut faded.effects).opacity = 0.5;
        add(&mut doc, html, faded);
        let b = add(&mut doc, html, style(Color::TRANSPARENT, 0.0, None));
        let text = doc.create_text("words");
        doc.append_child(b, text).unwrap();
        add(
            &mut doc,
            html,
            style(Color::rgb8(0, 0, 255), 0.0, Some(10.0)),
        );
        let list = paint_doc(&mut doc, &HashMap::new());

        let kinds = kinds(&list);
        let with_text = kinds.contains(&"text".to_string());
        let mut expected = vec!["rect 0"];
        // No system font on this machine draws no text.
        if with_text {
            expected.push("text");
        }
        expected.extend(["push", "rect 1", "pop"]);
        assert_eq!(kinds, expected);
    }

    #[test]
    fn a_float_paints_after_block_backgrounds_and_before_inline_content() {
        // Spec 0009, child 0001, AC-4: floats paint as their own layer after
        // in-flow block backgrounds and before inline content.
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let html = add(&mut doc, root, style(Color::TRANSPARENT, 0.0, None));
        let body = add(&mut doc, html, style(Color::TRANSPARENT, 0.0, None));
        let _block_a = add(
            &mut doc,
            body,
            style(Color::rgb8(255, 0, 0), 0.0, Some(20.0)),
        );
        let mut float_b = style(Color::rgb8(0, 255, 0), 0.0, Some(20.0));
        Rc::make_mut(&mut float_b.size).width = Some(Length::px(40.0));
        Rc::make_mut(&mut float_b.display_box).float = Float::Left;
        add(&mut doc, body, float_b);
        let text = doc.create_text("words");
        doc.append_child(body, text).unwrap();
        let list = paint_doc(&mut doc, &HashMap::new());

        let rects: Vec<_> = list
            .items
            .iter()
            .filter_map(|item| match item.kind {
                ItemKind::Rect { color, .. } => Some((color.r, color.g, color.b)),
                _ => None,
            })
            .collect();
        // block_a's red bg, then float_b's green bg (floats layer after blocks).
        let red = rects
            .iter()
            .position(|(r, g, b)| *r > 0.5 && *g < 0.1 && *b < 0.1);
        let green = rects
            .iter()
            .position(|(r, g, b)| *r < 0.1 && *g > 0.5 && *b < 0.1);
        assert!(
            red.is_some() && green.is_some() && red.unwrap() < green.unwrap(),
            "float paints after the in-flow block: {:?}",
            rects
        );
    }

    #[test]
    fn border_radius_rounds_the_background_and_border_and_shrinks_to_fit() {
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let html = add(&mut doc, root, style(Color::TRANSPARENT, 0.0, None));
        let mut rounded = style(Color::rgb8(0, 255, 0), 0.0, Some(20.0));
        let side = BorderSide {
            width: Au::from_px(2.0),
            style: css::BorderStyle::Solid,
            color: Color::BLACK,
        };
        rounded.border = Rc::new(css::Border {
            top: side,
            right: side,
            bottom: side,
            left: side,
            radius: css::BorderRadius {
                top_left: Length::px(6.0),
                // 100% of the 200 by 24 border box: 200 across, 24 down.
                top_right: Length::Percent(100.0),
                bottom_right: Length::px(0.0),
                bottom_left: Length::px(6.0),
            },
        });
        add(&mut doc, html, rounded);
        let list = paint_doc(&mut doc, &HashMap::new());

        let radii: Vec<_> = list
            .items
            .iter()
            .filter_map(|item| match &item.kind {
                ItemKind::Rect { radii, .. } | ItemKind::Border { radii, .. } => *radii,
                _ => None,
            })
            .collect();
        assert_eq!(radii.len(), 2, "the background and the border");
        assert_eq!(radii[0], radii[1]);
        // The top side needs 6 + 200 of its 200 px: everything scales by
        // 200 / 206.
        let factor = 200.0 / 206.0;
        let expected = [6.0, 6.0, 200.0, 24.0, 0.0, 0.0, 6.0, 6.0].map(|r| r * factor);
        assert!(
            radii[0]
                .iter()
                .zip(expected)
                .all(|(a, e)| (a - e).abs() < 1e-3),
            "{:?}",
            radii[0]
        );
    }

    /// `style` made positioned, with insets and a `z-index`.
    fn positioned(
        mut style: ComputedStyle,
        position: css::Position,
        insets: [Option<f32>; 4],
        z_index: Option<i32>,
    ) -> ComputedStyle {
        Rc::make_mut(&mut style.display_box).position = position;
        let [top, right, bottom, left] = insets.map(|inset| inset.map(Length::px));
        style.position_offsets = Rc::new(css::PositionOffsets {
            top,
            right,
            bottom,
            left,
        });
        Rc::make_mut(&mut style.effects).z_index = z_index;
        style
    }

    fn sized(mut style: ComputedStyle, width: f32) -> ComputedStyle {
        Rc::make_mut(&mut style.size).width = Some(Length::px(width));
        style
    }

    /// The red channel of each background rect, in 0 to 255.
    fn reds(list: &DisplayList) -> Vec<u8> {
        list.items
            .iter()
            .filter_map(|item| match item.kind {
                ItemKind::Rect { color, .. } => Some((color.r * 255.0).round() as u8),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn stacking_follows_appendix_e_with_z_index() {
        // Spec 0006, AC-6: negative z, then blocks, then positioned boxes
        // in tree order, then positive z, whatever the tree order.
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let html = add(&mut doc, root, style(Color::TRANSPARENT, 0.0, None));
        let red = |r: u8, height: f32| style(Color::rgb8(r, 0, 0), 0.0, Some(height));
        let popover = positioned(
            sized(red(10, 50.0), 50.0),
            css::Position::Absolute,
            [Some(0.0), None, None, Some(0.0)],
            Some(1),
        );
        add(&mut doc, html, popover);
        add(&mut doc, html, red(20, 50.0));
        let below = positioned(red(30, 20.0), css::Position::Relative, [None; 4], Some(-1));
        add(&mut doc, html, below);
        let auto = positioned(red(40, 10.0), css::Position::Relative, [None; 4], None);
        add(&mut doc, html, auto);
        // `z-index` on a box that is neither positioned nor a flex item is
        // ignored.
        let mut ignored = red(50, 10.0);
        Rc::make_mut(&mut ignored.effects).z_index = Some(5);
        add(&mut doc, html, ignored);
        let list = paint_doc(&mut doc, &HashMap::new());
        assert_eq!(reds(&list), [30, 20, 50, 40, 10]);
        assert!(
            !kinds(&list).contains(&"push".to_string()),
            "contexts without opacity need no group"
        );
    }

    #[test]
    fn outlines_grow_outside_the_border_box_and_paint_last() {
        // Spec 0006, Critical test scenarios: Outline (AC-8).
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let html = add(&mut doc, root, style(Color::TRANSPARENT, 0.0, None));
        let mut ringed = style(Color::rgb8(0, 255, 0), 0.0, Some(20.0));
        Rc::make_mut(&mut ringed.border).radius = css::BorderRadius {
            top_left: Length::px(6.0),
            top_right: Length::px(6.0),
            bottom_right: Length::px(6.0),
            bottom_left: Length::ZERO,
        };
        ringed.outline = Rc::new(css::Outline {
            width: Au::from_px(2.0),
            style: css::OutlineStyle::Solid,
            color: Color::rgb8(0, 0, 255),
            offset: Au::from_px(2.0),
        });
        add(&mut doc, html, ringed);
        add(
            &mut doc,
            html,
            style(Color::rgb8(255, 0, 0), 0.0, Some(10.0)),
        );
        let list = paint_doc(&mut doc, &HashMap::new());

        assert_eq!(kinds(&list), ["rect 0", "rect 1", "border"]);
        let Some(ItemKind::Border {
            rect: ring,
            widths,
            colors,
            radii,
            ..
        }) = list.items.last().map(|item| &item.kind)
        else {
            panic!("the outline is the last item");
        };
        assert_eq!(*ring, rect(-4.0, -4.0, 208.0, 28.0));
        assert_eq!(*widths, [2.0; 4]);
        assert_eq!(colors[0], color_f(Color::rgb8(0, 0, 255)));
        // Each rounded corner grows by offset plus width; the square one
        // stays square.
        assert_eq!(*radii, Some([10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 0.0, 0.0]));
    }

    #[test]
    fn clips_are_rounded_and_follow_the_containing_block() {
        // Spec 0006, AC-7: `x` is held by `r`, outside the clipping `o`,
        // so it is not clipped; `y` is held by the clipping `q` itself.
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let html = add(&mut doc, root, style(Color::TRANSPARENT, 0.0, None));
        let r = positioned(
            sized(style(Color::TRANSPARENT, 0.0, Some(100.0)), 100.0),
            css::Position::Relative,
            [None; 4],
            None,
        );
        let r = add(&mut doc, html, r);
        let mut clipping = sized(style(Color::TRANSPARENT, 0.0, Some(50.0)), 50.0);
        Rc::make_mut(&mut clipping.display_box).overflow_x = Overflow::Hidden;
        Rc::make_mut(&mut clipping.display_box).overflow_y = Overflow::Hidden;
        let side = BorderSide {
            width: Au::from_px(2.0),
            style: css::BorderStyle::Solid,
            color: Color::BLACK,
        };
        clipping.border = Rc::new(css::Border {
            top: side,
            right: side,
            bottom: side,
            left: side,
            radius: css::BorderRadius {
                top_left: Length::px(10.0),
                top_right: Length::px(10.0),
                bottom_right: Length::px(10.0),
                bottom_left: Length::px(10.0),
            },
        });
        let o = add(&mut doc, r, clipping.clone());
        let child = |r: u8| {
            positioned(
                sized(style(Color::rgb8(r, 0, 0), 0.0, Some(10.0)), 10.0),
                css::Position::Absolute,
                [None, None, None, Some(60.0)],
                None,
            )
        };
        add(&mut doc, o, child(100));
        let q = positioned(clipping, css::Position::Relative, [None; 4], None);
        let q = add(&mut doc, r, q);
        add(&mut doc, q, child(200));
        let list = paint_doc(&mut doc, &HashMap::new());

        assert_eq!(list.clips.len(), 2, "one clip each for `o` and `q`");
        assert_eq!(
            list.clips[0].radii,
            Some([8.0; 8]),
            "the padding box, rounded inside the 2px border"
        );
        let clip_of = |r: u8| {
            list.items.iter().find_map(|item| match item.kind {
                ItemKind::Rect { color, .. } if (color.r * 255.0).round() as u8 == r => {
                    Some(item.clip)
                }
                _ => None,
            })
        };
        assert_eq!(clip_of(100), Some(None), "not clipped by `o`");
        assert_eq!(clip_of(200), Some(Some(ClipId(1))), "clipped by `q`");
        assert_eq!(
            display_list::validate(&list, PageId(1), &display_list::ResourceKeys::default()),
            Ok(())
        );
    }

    #[test]
    fn a_sticky_box_pushes_a_sticky_node_that_shifts_with_scroll() {
        // Spec 0009, child 0003, AC-11: a sticky box shifts to stay inside
        // its scroll frame as it scrolls.
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let mut html_style = style(Color::TRANSPARENT, 0.0, None);
        Rc::make_mut(&mut html_style.size).height = Some(Length::px(2000.0));
        let html = add(&mut doc, root, html_style);
        let mut sticky = style(Color::rgb8(0, 255, 0), 0.0, Some(50.0));
        Rc::make_mut(&mut sticky.display_box).position = Position::Sticky;
        Rc::make_mut(&mut sticky.position_offsets).top = Some(Length::px(0.0));
        add(&mut doc, html, sticky);

        // Not scrolled: the sticky offset is zero.
        let unscrolled = paint_doc(&mut doc, &HashMap::new());
        let sticky_node_id = spatial_sticky(&unscrolled).expect("a sticky spatial node is pushed");
        assert_eq!(
            sticky_offset_of(&unscrolled, sticky_node_id),
            PointF { x: 0.0, y: 0.0 }
        );

        // Scrolled down 200 px: the sticky box shifts down to follow.
        let mut scrolled_map = HashMap::new();
        let scroll_id = scroll_id(0, html);
        scrolled_map.insert(
            scroll_id,
            ScrollOffset {
                x: 0.0,
                y: 200.0,
                generation: 0,
            },
        );
        let scrolled = paint_doc(&mut doc, &scrolled_map);
        let sticky_node_id = spatial_sticky(&scrolled).expect("a sticky spatial node");
        assert!(sticky_offset_of(&scrolled, sticky_node_id).y > 0.0);
    }

    #[test]
    fn clip_rect_clips_an_absolute_box_and_its_descendants() {
        // Spec 0009, child 0003, AC-12: `clip: rect(...)` clips absolute and
        // fixed boxes to the rectangle measured from the border box top left.
        use css::ClipRect;
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let html = add(&mut doc, root, style(Color::TRANSPARENT, 0.0, None));
        let mut abs = style(Color::rgb8(0, 255, 0), 0.0, Some(200.0));
        Rc::make_mut(&mut abs.display_box).position = Position::Absolute;
        let mut offsets = (*abs.position_offsets).clone();
        offsets.top = Some(Length::px(0.0));
        offsets.left = Some(Length::px(0.0));
        abs.position_offsets = Rc::new(offsets);
        Rc::make_mut(&mut abs.effects).clip = Some(ClipRect {
            top: Some(Au::from_px(10.0)),
            right: Some(Au::from_px(50.0)),
            bottom: Some(Au::from_px(60.0)),
            left: Some(Au::from_px(20.0)),
        });
        let abs_node = add(&mut doc, html, abs);
        add(
            &mut doc,
            abs_node,
            style(Color::rgb8(255, 0, 0), 0.0, Some(100.0)),
        );

        let list = paint_doc(&mut doc, &HashMap::new());
        let clip = list.clips.last().expect("a clip node is pushed");
        assert_eq!(
            clip.rect,
            RectF {
                origin: PointF { x: 20.0, y: 10.0 },
                size: SizeF {
                    width: 30.0,
                    height: 50.0
                }
            }
        );
        // The inner box draws inside the clip.
        let inner = list.items.iter().find(|item| item.clip == Some(ClipId(0)));
        assert!(inner.is_some(), "inner box is drawn inside the clip");
    }

    /// The spatial index of the first `SpatialKind::Sticky` node, if any.
    fn spatial_sticky(list: &DisplayList) -> Option<usize> {
        list.spatial
            .iter()
            .position(|n| matches!(n.kind, SpatialKind::Sticky { .. }))
    }

    /// The offset of the sticky node at `index`.
    fn sticky_offset_of(list: &DisplayList, index: usize) -> PointF {
        match &list.spatial[index].kind {
            SpatialKind::Sticky { offset, .. } => *offset,
            _ => PointF::default(),
        }
    }
}
