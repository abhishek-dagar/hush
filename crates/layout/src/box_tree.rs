//! The box tree: one document's boxes, built from the flat tree and rebuilt
//! one block container at a time (spec 0002, Box tree; spec 0004, child
//! 0004, Box tree building).
//!
//! Block level elements get block boxes. Text, inline elements, `<br>`,
//! images and inline blocks become the inline content of the nearest block:
//! a block holds either block children or inline content, never both, so a
//! run of inline content between blocks is wrapped in an anonymous block
//! (CSS 2.2 section 9.2.1.1). A block inside an inline element splits it:
//! the block is lifted out to the block around it, and the inline element
//! goes on in the next run of inline content.
//!
//! Inline elements get an inline box each, which the inline content marks
//! with start and end items: layout uses it for the element's edges on each
//! line, paint for its background and border. Inline blocks and images are
//! atomic inlines: boxes laid out on their own and set on a line whole.
//! Absolute boxes get boxes of their own too, but leave only a mark in the
//! content: an `Absolute` item among inline content, or a child of their
//! block when nothing inline is around them. An `<input>` shows its value,
//! or its placeholder, as the only inline content of its own box (spec
//! 0006, AC-17).

use std::collections::HashSet;
use std::ops::BitOr;
use std::rc::Rc;

use base::atoms::{LocalName, QualName};
use base::{Arena, Au, BoxId, BoxMarker, local_name, ns};
use css::{ComputedStyle, Display, Float, ListStyleType, Position, StyleDamage, WhiteSpace};
use dom::{Document, ImageState, NodeData, NodeId};

use crate::LayoutCache;
use crate::table::fix_tables;

/// What kind of box this is. More kinds (grid, tables) arrive with their
/// slices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxKind {
    /// A block that stacks its children vertically, or lays out its inline
    /// content in lines. An inline block is one too, set on a line whole.
    BlockContainer,
    /// A block level flex container: its children are flex items, laid out
    /// in a row or a column (spec 0006, AC-3). It never holds inline
    /// content; a run of text inside it becomes an anonymous flex item.
    FlexContainer,
    /// An inline element: it has no layout of its own, but its start and
    /// end items in the inline content give it edges on each line it
    /// spans.
    InlineBox,
    /// A replaced element (``), sized from its natural size.
    Replaced,
    /// A table wrapper: holds the table and its captions, takes the table's
    /// `position`, `float`, margins and `z-index` (spec 0009, child 0004,
    /// AC-14).
    TableWrapper,
    /// A table box: carries borders, padding and background, sizes its
    /// columns and rows (AC-14).
    Table,
    /// A table row group (`thead`, `tbody`, `tfoot`): a block of rows.
    TableRowGroup,
    /// A table header row group (`thead`).
    TableHeaderGroup,
    /// A table footer row group (`tfoot`).
    TableFooterGroup,
    /// A table row (`tr`): holds cells side by side.
    TableRow,
    /// A table column group (`colgroup`).
    TableColumnGroup,
    /// A table column (`col`).
    TableColumn,
    /// A table cell (`td`, `th`): a BFC root, sized at its column widths.
    TableCell,
    /// A table caption (`caption`): above or below the table by
    /// `caption-side` (AC-14).
    TableCaption,
}

/// Which side a float sits on (CSS 2.2 section 9.5; spec 0009, child 0001).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatSide {
    Left,
    Right,
}

/// One piece of a block's inline content.
#[derive(Debug, Clone, PartialEq)]
pub enum InlineItem {
    /// Text from one text node, with white space already processed.
    Text {
        /// The text node.
        node: NodeId,
        /// The text to lay out.
        text: String,
        /// The style it draws with: its parent element's.
        style: Rc<ComputedStyle>,
    },
    /// An inline box starts: its start edge (margin, border, padding).
    Start(BoxId),
    /// An inline box ends: its end edge.
    End(BoxId),
    /// An atomic inline (inline block or image), set on the line whole.
    Atomic(BoxId),
    /// An absolute box's place in the content: it takes no room, and only
    /// marks the box's static position (spec 0006, AC-5).
    Absolute(BoxId),
    /// A float's place in inline content: it takes no room on the line, and
    /// the float is placed where the marker is met (spec 0009, child 0001).
    Float(BoxId),
    /// A forced line break: a `<br>`, or a preserved newline in the text
    /// node given.
    Break(NodeId),
}

/// A replaced element's natural size. `None` on an axis when unknown.
/// It comes from the decoded picture and the `width` and
/// `height` attributes (spec 0004, child 0004, Replaced images).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NaturalSize {
    /// The natural width.
    pub width: Option<Au>,
    /// The natural height.
    pub height: Option<Au>,
}

/// Per box layout state bits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BoxFlags(u8);

impl BoxFlags {
    /// The box itself must be laid out again.
    pub const NEEDS_LAYOUT: Self = Self(1 << 0);
    /// Some box below this one must be laid out again.
    pub const CHILD_NEEDS_LAYOUT: Self = Self(1 << 1);

    /// No flags set.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Whether every flag in `other` is set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether any flag in `other` is set.
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

impl BitOr for BoxFlags {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// One box in the box tree.
#[derive(Debug)]
pub struct LayoutBox {
    /// The node that generated it; `None` for anonymous boxes.
    pub node: Option<NodeId>,
    /// What kind of box it is.
    pub kind: BoxKind,
    /// The float this box is placed on: `Some` when `float` is `left` or
    /// `right` and the box is not absolutely positioned (spec 0009, child
    /// 0001).
    pub float: Option<FloatSide>,
    /// The style it lays out and paints with.
    pub style: Rc<ComputedStyle>,
    /// The parent box: for an inline box or atomic inline, the block whose
    /// inline content holds it.
    pub parent: Option<BoxId>,
    /// Child boxes in order. Empty when the box has inline content.
    pub children: Vec<BoxId>,
    /// The box's inline content, laid out in lines. Empty when the box has
    /// block children.
    pub inline: Vec<InlineItem>,
    /// It sits on a line as an atomic inline (an inline block, or an image
    /// that is not `display: block`), so it shrinks to fit and its `auto`
    /// margins are zero.
    pub inline_level: bool,
    /// A list item's marker text (`•`, `3.`), from its `list-style-type`
    /// and its place among its siblings.
    pub marker: Option<String>,
    /// A replaced box's natural size.
    pub natural: NaturalSize,
    /// The picture a replaced box draws, by its display list image key,
    /// once decoded.
    pub image: Option<u32>,
    /// It is a text field (`<input>` that takes text), whose content width
    /// is 20 times the advance of `0` in its font (spec 0006, AC-17).
    pub text_field: bool,
    /// It is an `<input>` or `<button>`, whose content is centered in a
    /// content box taller than it, as Chrome's inner editor and button
    /// content box are (spec 0006, AC-17).
    pub centers_content: bool,
    /// Whether it or a box below it must be laid out again.
    pub flags: BoxFlags,
    /// The last layout result, reused while the box is clean and gets the
    /// same constraint space.
    pub cache: Option<LayoutCache>,
}

/// One document's boxes.
#[derive(Debug, Default)]
pub struct BoxTree {
    pub(crate) boxes: Arena<LayoutBox, BoxMarker>,
    root: Option<BoxId>,
    /// The box of the root's `<body>` child, whose background the canvas
    /// takes when the root has none (spec 0004, AC-23).
    body: Option<BoxId>,
}

/// A block's content, read from its flat children before boxes are made.
enum Segment {
    /// A run of inline content.
    Inline(Vec<InlineItem>),
    /// A block level element, its style, and whether it is a replaced
    /// element.
    Block(NodeId, Rc<ComputedStyle>, bool),
}

/// Inline content read from a block's children, with the atomic inlines
/// whose own content still has to be built.
struct Read {
    segments: Vec<Segment>,
    atomics: Vec<(BoxId, NodeId)>,
}

/// The style a displayed element generates a box with: `None` for an
/// element with no style (not styled yet) or with `display: none`.
fn box_style(doc: &Document, node: NodeId) -> Option<Rc<ComputedStyle>> {
    doc.node(node)
        .and_then(|n| n.as_element())
        .and_then(|element| element.style())
        .filter(|style| style.display_box.display != Display::None)
        .map(Rc::clone)
}

/// The HTML element name of `node`, if it is an HTML element.
fn html_name(doc: &Document, node: NodeId) -> Option<LocalName> {
    let element = doc.node(node)?.as_element()?;
    let name = element.name();
    (name.ns == ns!("http://www.w3.org/1999/xhtml")).then(|| name.local.clone())
}

fn attribute(doc: &Document, node: NodeId, name: LocalName) -> Option<String> {
    doc.node(node)?
        .as_element()?
        .attribute(&QualName::attribute(name))
        .map(str::to_owned)
}

impl BoxTree {
    /// Builds the boxes for every styled, displayed element in `doc`, walking
    /// the flat tree, and records each node's principal box on the node.
    pub fn build(doc: &mut Document) -> Self {
        let mut tree = Self::default();
        let Some(html) = doc.document_element() else {
            return tree;
        };
        let Some(style) = box_style(doc, html) else {
            clear_boxes(doc, html);
            return tree;
        };
        // The root element always makes a block level box, whatever its
        // display.
        let kind = container_kind(&style);
        let root = tree
            .boxes
            .insert(LayoutBox::new(Some(html), kind, style, None));
        tree.root = Some(root);
        let _ = doc.set_layout_box(html, Some(root));
        tree.fill(doc, root, html);
        // CSS Tables 3 section 3: add the anonymous boxes tables need (a
        // table wrapper, anonymous rows and cells around misplaced parts).
        fix_tables(&mut tree);
        tree
    }

    /// Rebuilds the boxes below each node in `dirty` (nodes with
    /// `NEEDS_BOX_REBUILD`, from `Document::take_box_rebuild_roots`), and
    /// returns how many boxes were built.
    ///
    /// Each region is rebuilt from the nearest block container at or above
    /// the dirty node, because inline content belongs to its block. Boxes
    /// below that container are dropped and built again; the container keeps
    /// its id, takes its node's current style, and is marked for layout. A
    /// dirty node with no box above it (the document itself) rebuilds the
    /// whole tree.
    pub fn rebuild(&mut self, doc: &mut Document, dirty: &[NodeId]) -> usize {
        let mut containers: Vec<BoxId> = Vec::new();
        for &node in dirty {
            match self.container_for(doc, node) {
                Some(container) if !containers.contains(&container) => containers.push(container),
                Some(_) => {}
                None => {
                    *self = Self::build(doc);
                    return self.len();
                }
            }
        }

        let mut built = 0;
        for container in containers {
            // An earlier container in the list may have contained this one,
            // and its rebuild already freed and replaced it.
            let Some(layout_box) = self.boxes.get_mut(container) else {
                continue;
            };
            let mut old = std::mem::take(&mut layout_box.children);
            old.extend(inline_boxes(&std::mem::take(&mut layout_box.inline)));
            let Some(node) = layout_box.node else {
                continue;
            };
            let style = doc
                .node(node)
                .and_then(|n| n.as_element())
                .and_then(|element| element.style())
                .map(Rc::clone);
            if let Some(style) = style {
                layout_box.style = style;
            }
            self.free_subtrees(old);
            built += self.fill(doc, container, node);
            self.mark_needs_layout(container);
        }
        built
    }

    /// Marks `id` for layout and each box above it as having a dirty child.
    pub fn mark_needs_layout(&mut self, id: BoxId) {
        let Some(layout_box) = self.boxes.get_mut(id) else {
            return;
        };
        layout_box.flags.insert(BoxFlags::NEEDS_LAYOUT);
        let mut current = layout_box.parent;
        while let Some(ancestor) = current.and_then(|a| self.boxes.get_mut(a)) {
            // Layout clears flags from the top down, so a flagged ancestor
            // means every box above it is flagged too.
            if ancestor.flags.contains(BoxFlags::CHILD_NEEDS_LAYOUT) {
                break;
            }
            ancestor.flags.insert(BoxFlags::CHILD_NEEDS_LAYOUT);
            current = ancestor.parent;
        }
    }

    /// Marks every box for layout, so all of the document's text is shaped
    /// again, as when a web font arrives (spec 0006, Relayout on font
    /// load).
    pub fn mark_all_needs_layout(&mut self) {
        let ids: Vec<BoxId> = self.boxes.iter().map(|(id, _)| id).collect();
        for id in ids {
            if let Some(layout_box) = self.boxes.get_mut(id) {
                layout_box
                    .flags
                    .insert(BoxFlags::NEEDS_LAYOUT | BoxFlags::CHILD_NEEDS_LAYOUT);
            }
        }
    }

    /// Gives box `id` its node's new style after a change with `damage`.
    /// `Relayout` (or worse) also marks it for layout; `Repaint` only needs
    /// the next paint, which reads the box's style.
    pub fn restyle_box(&mut self, id: BoxId, style: Rc<ComputedStyle>, damage: StyleDamage) {
        let Some(layout_box) = self.boxes.get_mut(id) else {
            return;
        };
        layout_box.style = style;
        if damage >= StyleDamage::Relayout {
            self.mark_needs_layout(id);
        }
    }

    /// The box for the document element, if it has one.
    pub fn root(&self) -> Option<BoxId> {
        self.root
    }

    /// Sets the root box: used by the table fixup when the root table is
    /// wrapped in a table wrapper.
    pub(crate) fn set_root(&mut self, root: BoxId) {
        self.root = Some(root);
    }

    /// The box whose background paints the whole canvas (CSS 2.2 section
    /// 14.2): the root's, or `<body>`'s when the root's background is
    /// transparent with no image.
    pub fn canvas_background_box(&self) -> Option<BoxId> {
        let root = self.root?;
        let style = &self.get(root)?.style;
        let root_has_background =
            style.background.color.a > 0.0 || style.background.image.is_some();
        if root_has_background {
            return Some(root);
        }
        self.body
            .filter(|&body| self.get(body).is_some())
            .or(Some(root))
    }

    /// The box for `id`, or `None` if it was freed.
    pub fn get(&self, id: BoxId) -> Option<&LayoutBox> {
        self.boxes.get(id)
    }

    /// Every live box, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = &LayoutBox> {
        self.boxes.iter().map(|(_, layout_box)| layout_box)
    }

    /// How many boxes there are.
    pub fn len(&self) -> usize {
        self.boxes.len()
    }

    /// Whether there are no boxes.
    pub fn is_empty(&self) -> bool {
        self.boxes.is_empty()
    }

    /// Builds the content of `container`, the box of `node`, and of every
    /// block and inline block below it. Returns how many boxes were built.
    /// Uses a heap stack, so a deep page cannot overflow the thread stack.
    fn fill(&mut self, doc: &mut Document, container: BoxId, node: NodeId) -> usize {
        let mut built = 0;
        let mut work = vec![(container, node)];
        while let Some((container, node)) = work.pop() {
            let Some(container_style) = self.boxes.get(container).map(|b| Rc::clone(&b.style))
            else {
                continue;
            };
            if Some(container) == self.root {
                self.body = None;
            }
            let is_control = html_name(doc, node)
                .is_some_and(|n| n == local_name!("input") || n == local_name!("button"));
            if let Some(layout_box) = self.boxes.get_mut(container) {
                layout_box.centers_content = is_control;
            }
            if let Some((text, style, text_field)) = input_text(doc, node, &container_style) {
                clear_children(doc, node);
                if let Some(layout_box) = self.boxes.get_mut(container) {
                    layout_box.text_field = text_field;
                    layout_box.inline =
                        process_white_space(vec![InlineItem::Text { node, text, style }]);
                }
                continue;
            }
            let Read { segments, atomics } = self.read_segments(doc, node, container);
            built += atomics.len();
            let mut pending: Vec<(BoxId, NodeId)> = atomics;

            // A flex container's text is wrapped in anonymous flex items, so
            // it always takes the block path.
            let is_flex = container_style.display_box.display == Display::Flex;
            let only_inline = segments.iter().all(|s| matches!(s, Segment::Inline(_)));
            if only_inline && !is_flex {
                let items: Vec<InlineItem> = segments
                    .into_iter()
                    .flat_map(|segment| match segment {
                        Segment::Inline(items) => items,
                        Segment::Block(..) => Vec::new(),
                    })
                    .collect();
                if let Some(layout_box) = self.boxes.get_mut(container) {
                    layout_box.inline = process_white_space(items);
                }
                work.extend(pending.into_iter().rev());
                continue;
            }

            let mut counter = list_start(doc, node);
            let mut kept: HashSet<BoxId> = HashSet::new();
            let mut dropped: Vec<BoxId> = Vec::new();
            let mut blocks = Vec::new();
            for segment in segments {
                let (child_node, style, inline, kind) = match segment {
                    Segment::Block(child, style, replaced) => {
                        let kind = if replaced {
                            BoxKind::Replaced
                        } else {
                            container_kind(&style)
                        };
                        (Some(child), style, Vec::new(), kind)
                    }
                    Segment::Inline(items) => {
                        let items = process_white_space(items);
                        // White space between blocks makes no box. Absolute
                        // boxes in it stay, as block children of their own.
                        if !has_content(&items) {
                            for item in &items {
                                match *item {
                                    InlineItem::Absolute(id) => {
                                        if let Some(parent) = self.boxes.get_mut(container) {
                                            parent.children.push(id);
                                        }
                                    }
                                    InlineItem::Start(id) | InlineItem::Atomic(id) => {
                                        dropped.push(id);
                                    }
                                    _ => {}
                                }
                            }
                            continue;
                        }
                        kept.extend(inline_boxes(&items));
                        let style = anonymous_style(&container_style);
                        (None, style, items, BoxKind::BlockContainer)
                    }
                };
                let mut layout_box = LayoutBox::new(child_node, kind, style, Some(container));
                // A floated block level box is blockified already; a float
                // among inline content leaves a marker instead (below).
                if layout_box.float.is_none()
                    && layout_box.style.display_box.float != Float::None
                    && !layout_box.is_absolute()
                {
                    layout_box.float = Some(match layout_box.style.display_box.float {
                        Float::Left => FloatSide::Left,
                        Float::Right => FloatSide::Right,
                        Float::None => FloatSide::Left,
                    });
                }
                if let Some(child) = child_node {
                    if layout_box.style.display_box.display == Display::ListItem {
                        if let Some(value) = attribute(doc, child, local_name!("value"))
                            .and_then(|v| v.trim().parse::<i64>().ok())
                        {
                            counter = value;
                        }
                        layout_box.marker = marker_text(layout_box.style.list.style_type, counter);
                        counter = counter.saturating_add(1);
                    }
                    if kind == BoxKind::Replaced {
                        (layout_box.natural, layout_box.image) = replaced_content(doc, child);
                    }
                }
                let item_boxes = inline_boxes(&inline);
                layout_box.inline = inline;
                let id = self.boxes.insert(layout_box);
                // Inline boxes and atomics belong to the anonymous block
                // that now holds them, so their changes dirty it.
                for item_box in item_boxes {
                    if let Some(b) = self.boxes.get_mut(item_box) {
                        b.parent = Some(id);
                    }
                }
                if let Some(parent) = self.boxes.get_mut(container) {
                    parent.children.push(id);
                }
                built += 1;
                if let Some(child) = child_node {
                    let _ = doc.set_layout_box(child, Some(id));
                    if Some(container) == self.root
                        && html_name(doc, child) == Some(local_name!("body"))
                    {
                        self.body.get_or_insert(id);
                    }
                    if kind == BoxKind::Replaced {
                        clear_children(doc, child);
                    } else {
                        blocks.push((id, child));
                    }
                }
            }
            // An inline box reopened after a lifted block may sit in a kept
            // run as well as a dropped one.
            for id in dropped {
                if kept.contains(&id) {
                    continue;
                }
                if let Some(node) = self.boxes.remove(id).and_then(|b| b.node) {
                    let _ = doc.set_layout_box(node, None);
                }
            }
            // Reversed so the first block's content is built first.
            pending.extend(blocks);
            work.extend(pending.into_iter().rev());
        }
        built
    }

    /// Reads `node`'s flat children into runs of inline content and block
    /// level elements, descending into inline elements, so their content
    /// joins the run and their blocks are lifted out. Makes the inline
    /// boxes and atomic inlines on the way. Clears the box of every node
    /// that gets none.
    fn read_segments(&mut self, doc: &mut Document, node: NodeId, container: BoxId) -> Read {
        enum Pending {
            Node(NodeId, Rc<ComputedStyle>),
            End(BoxId),
        }
        let Some(style) = self.boxes.get(container).map(|b| Rc::clone(&b.style)) else {
            return Read {
                segments: Vec::new(),
                atomics: Vec::new(),
            };
        };
        let mut segments = Vec::new();
        let mut atomics = Vec::new();
        let mut run: Vec<InlineItem> = Vec::new();
        // Inline boxes open around the current point, outermost first.
        let mut open: Vec<BoxId> = Vec::new();
        // A block was lifted out of the open inline boxes: they go on in
        // the next run.
        let mut reopen = false;
        let mut pending: Vec<Pending> = children_of(doc, node, &style)
            .into_iter()
            .map(|(child, style)| Pending::Node(child, style))
            .collect();

        let push = |run: &mut Vec<InlineItem>, open: &[BoxId], reopen: &mut bool, item| {
            if *reopen {
                run.extend(open.iter().map(|&id| InlineItem::Start(id)));
                *reopen = false;
            }
            run.push(item);
        };

        while let Some(next) = pending.pop() {
            let (child, parent_style) = match next {
                Pending::End(id) => {
                    open.pop();
                    if !reopen {
                        run.push(InlineItem::End(id));
                    }
                    continue;
                }
                Pending::Node(child, style) => (child, style),
            };
            let text = match doc.node(child).map(|n| n.data()) {
                Some(NodeData::Text(text)) => Some(text.clone()),
                Some(NodeData::Element(_)) => None,
                _ => {
                    let _ = doc.set_layout_box(child, None);
                    continue;
                }
            };
            if let Some(text) = text {
                let _ = doc.set_layout_box(child, None);
                let item = InlineItem::Text {
                    node: child,
                    text,
                    style: parent_style,
                };
                push(&mut run, &open, &mut reopen, item);
                continue;
            }
            let Some(child_style) = box_style(doc, child) else {
                clear_boxes(doc, child);
                continue;
            };
            let name = html_name(doc, child);
            let display = child_style.display_box.display;
            let block_level = matches!(display, Display::Block | Display::ListItem | Display::Flex);
            if name == Some(local_name!("br")) {
                clear_boxes(doc, child);
                push(&mut run, &open, &mut reopen, InlineItem::Break(child));
                continue;
            }
            let replaced = name == Some(local_name!("img"));
            if child_style.display_box.position == Position::Absolute {
                // Out of flow: a box of its own, and a mark where it would
                // have been (spec 0006, AC-5).
                let kind = if replaced {
                    BoxKind::Replaced
                } else {
                    container_kind(&child_style)
                };
                let mut layout_box =
                    LayoutBox::new(Some(child), kind, child_style, Some(container));
                if replaced {
                    (layout_box.natural, layout_box.image) = replaced_content(doc, child);
                }
                let id = self.boxes.insert(layout_box);
                let _ = doc.set_layout_box(child, Some(id));
                if replaced {
                    clear_children(doc, child);
                } else {
                    atomics.push((id, child));
                }
                push(&mut run, &open, &mut reopen, InlineItem::Absolute(id));
                continue;
            }
            // A broken image with `alt` text shows the text in its place;
            // without it, it stays an empty box its attributes size
            // (AC-29).
            let alt =
                (replaced && !block_level && doc.image_state(child) == Some(ImageState::Failed))
                    .then(|| attribute(doc, child, local_name!("alt")))
                    .flatten()
                    .filter(|alt| !alt.is_empty());
            if let Some(text) = alt {
                clear_boxes(doc, child);
                let item = InlineItem::Text {
                    node: child,
                    text,
                    style: child_style,
                };
                push(&mut run, &open, &mut reopen, item);
                continue;
            }
            if !block_level && (replaced || display == Display::InlineBlock) {
                // A floated inline level box becomes a BFC root box and leaves
                // a marker in the inline content where it appeared
                // (spec 0009, child 0001).
                let float_side = match child_style.display_box.float {
                    Float::Left => Some(FloatSide::Left),
                    Float::Right => Some(FloatSide::Right),
                    Float::None => None,
                };
                let floated = float_side.is_some();
                let kind = if replaced {
                    BoxKind::Replaced
                } else {
                    BoxKind::BlockContainer
                };
                let mut layout_box =
                    LayoutBox::new(Some(child), kind, child_style, Some(container));
                if floated {
                    layout_box.float = float_side;
                } else {
                    layout_box.inline_level = true;
                }
                if replaced {
                    (layout_box.natural, layout_box.image) = replaced_content(doc, child);
                }
                let id = self.boxes.insert(layout_box);
                let _ = doc.set_layout_box(child, Some(id));
                if replaced {
                    clear_children(doc, child);
                } else {
                    atomics.push((id, child));
                }
                if floated {
                    push(&mut run, &open, &mut reopen, InlineItem::Float(id));
                } else {
                    push(&mut run, &open, &mut reopen, InlineItem::Atomic(id));
                }
                continue;
            }
            if display == Display::Inline {
                let layout_box = LayoutBox::new(
                    Some(child),
                    BoxKind::InlineBox,
                    Rc::clone(&child_style),
                    Some(container),
                );
                let id = self.boxes.insert(layout_box);
                let _ = doc.set_layout_box(child, Some(id));
                push(&mut run, &open, &mut reopen, InlineItem::Start(id));
                open.push(id);
                pending.push(Pending::End(id));
                pending.extend(
                    children_of(doc, child, &child_style)
                        .into_iter()
                        .map(|(c, s)| Pending::Node(c, s)),
                );
                continue;
            }
            // A block: close the inline boxes around it, end the run, and
            // reopen them after it.
            if !run.is_empty() {
                if !reopen {
                    run.extend(open.iter().rev().map(|&id| InlineItem::End(id)));
                }
                segments.push(Segment::Inline(std::mem::take(&mut run)));
            }
            reopen = !open.is_empty();
            segments.push(Segment::Block(child, child_style, replaced));
        }
        if !run.is_empty() {
            segments.push(Segment::Inline(run));
        }
        Read { segments, atomics }
    }

    /// The nearest block container box at or above `node` in the flat tree.
    fn container_for(&self, doc: &Document, node: NodeId) -> Option<BoxId> {
        let mut current = Some(node);
        while let Some(id) = current {
            let layout_box = doc
                .is_alive(id)
                .then(|| doc.node(id))
                .flatten()
                .and_then(|n| n.layout_box());
            if let Some(box_id) = layout_box
                && self.boxes.get(box_id).is_some_and(|b| {
                    matches!(b.kind, BoxKind::BlockContainer | BoxKind::FlexContainer)
                })
            {
                return Some(box_id);
            }
            current = doc.flat_parent(id);
        }
        None
    }

    /// Frees `roots` and every box below them, inline boxes and atomic
    /// inlines included.
    fn free_subtrees(&mut self, roots: Vec<BoxId>) {
        let mut stack = roots;
        while let Some(id) = stack.pop() {
            if let Some(layout_box) = self.boxes.remove(id) {
                stack.extend(layout_box.children);
                stack.extend(inline_boxes(&layout_box.inline));
            }
        }
    }
}

impl LayoutBox {
    /// Whether the box is absolutely positioned: out of flow, and laid out
    /// by its containing block (spec 0006, AC-5).
    pub fn is_absolute(&self) -> bool {
        self.style.display_box.position == Position::Absolute
    }

    /// Whether the box is fixed positioned: out of flow, laid out against
    /// the viewport and scrolled with it (spec 0009, child 0003, AC-10).
    pub fn is_fixed(&self) -> bool {
        self.style.display_box.position == Position::Fixed
    }

    /// Whether the box is sticky positioned: laid out in flow, then shifted
    /// to stay inside its nearest scroll frame while that frame scrolls
    /// (spec 0009, child 0003, AC-11).
    pub fn is_sticky(&self) -> bool {
        self.style.display_box.position == Position::Sticky
    }

    /// Whether the box is a float: out of flow, placed to the side by its
    /// block formatting context (spec 0009, child 0001).
    pub fn is_float(&self) -> bool {
        self.float.is_some()
    }

    pub(crate) fn new(
        node: Option<NodeId>,
        kind: BoxKind,
        style: Rc<ComputedStyle>,
        parent: Option<BoxId>,
    ) -> Self {
        Self {
            node,
            kind,
            style,
            parent,
            children: Vec::new(),
            inline: Vec::new(),
            inline_level: false,
            marker: None,
            natural: NaturalSize::default(),
            image: None,
            text_field: false,
            centers_content: false,
            float: None,
            flags: BoxFlags::empty(),
            cache: None,
        }
    }
}

/// The inline boxes, atomic inlines and absolute boxes that `items` refer
/// to.
fn inline_boxes(items: &[InlineItem]) -> Vec<BoxId> {
    items
        .iter()
        .filter_map(|item| match item {
            InlineItem::Start(id)
            | InlineItem::Atomic(id)
            | InlineItem::Absolute(id)
            | InlineItem::Float(id) => Some(*id),
            _ => None,
        })
        .collect()
}

/// Whether inline content makes a line box: it has text, an atomic inline
/// or a forced break, not just empty inline elements.
fn has_content(items: &[InlineItem]) -> bool {
    items.iter().any(|item| {
        matches!(
            item,
            InlineItem::Text { .. } | InlineItem::Atomic(_) | InlineItem::Break(_)
        )
    })
}

/// What an `<input>` shows (spec 0006, AC-17): for a field that takes
/// text, its `value` (as dots for a password), else its `placeholder` in
/// the `::placeholder` style, else one space so the field keeps its line;
/// for a button its `value` or its default label. Returns the text, its
/// style and whether the input is a text field. `None` for any other node,
/// and for an input that shows no text (a checkbox).
fn input_text(
    doc: &Document,
    node: NodeId,
    style: &Rc<ComputedStyle>,
) -> Option<(String, Rc<ComputedStyle>, bool)> {
    if html_name(doc, node)? != local_name!("input") {
        return None;
    }
    let kind = attribute(doc, node, local_name!("type"))
        .unwrap_or_default()
        .to_ascii_lowercase();
    let value = attribute(doc, node, local_name!("value")).unwrap_or_default();
    let label = match kind.as_str() {
        "submit" => Some("Submit"),
        "reset" => Some("Reset"),
        "button" => Some(""),
        _ => None,
    };
    if let Some(label) = label {
        let text = if value.is_empty() {
            label.into()
        } else {
            value
        };
        return Some((text, Rc::clone(style), false));
    }
    if !doc.is_text_field(node) {
        return None;
    }
    // What the user typed, else the attribute, without line breaks (spec
    // 0008, AC-16).
    let value = doc.input_value(node).unwrap_or_default();
    if !value.is_empty() {
        let text = if kind == "password" {
            "•".repeat(value.chars().count())
        } else {
            value
        };
        return Some((text, Rc::clone(style), true));
    }
    let placeholder = attribute(doc, node, local_name!("placeholder"))
        .map(|p| p.replace(['\n', '\r'], ""))
        .filter(|p| !p.is_empty());
    let placeholder_style = doc
        .node(node)
        .and_then(|n| n.as_element())
        .and_then(|element| element.placeholder_style())
        .map(Rc::clone);
    Some(match (placeholder, placeholder_style) {
        (Some(text), Some(placeholder_style)) => (text, placeholder_style, true),
        (Some(text), None) => (text, Rc::clone(style), true),
        (None, _) => (" ".into(), Rc::clone(style), true),
    })
}

/// `node`'s flat children, last first, each paired with `style`.
fn children_of(
    doc: &Document,
    node: NodeId,
    style: &Rc<ComputedStyle>,
) -> Vec<(NodeId, Rc<ComputedStyle>)> {
    let mut children: Vec<_> = doc
        .flat_children(node)
        .map(|child| (child, Rc::clone(style)))
        .collect();
    children.reverse();
    children
}

/// Clears the box of `node` and every node below it: none of them has one.
fn clear_boxes(doc: &mut Document, node: NodeId) {
    let mut stack = vec![node];
    while let Some(id) = stack.pop() {
        let _ = doc.set_layout_box(id, None);
        stack.extend(doc.flat_children(id).collect::<Vec<_>>());
    }
}

/// Clears the boxes below `node`, keeping its own: a replaced element
/// shows none of its children.
fn clear_children(doc: &mut Document, node: NodeId) {
    for child in doc.flat_children(node).collect::<Vec<_>>() {
        clear_boxes(doc, child);
    }
}

/// The kind of box a block level, non replaced element makes.
fn container_kind(style: &ComputedStyle) -> BoxKind {
    match style.display_box.display {
        Display::Flex => BoxKind::FlexContainer,
        Display::Table | Display::InlineTable => BoxKind::Table,
        Display::TableRowGroup => BoxKind::TableRowGroup,
        Display::TableHeaderGroup => BoxKind::TableHeaderGroup,
        Display::TableFooterGroup => BoxKind::TableFooterGroup,
        Display::TableRow => BoxKind::TableRow,
        Display::TableColumn => BoxKind::TableColumn,
        Display::TableColumnGroup => BoxKind::TableColumnGroup,
        Display::TableCell => BoxKind::TableCell,
        Display::TableCaption => BoxKind::TableCaption,
        _ => BoxKind::BlockContainer,
    }
}

/// An anonymous block's style: everything inherited from its container,
/// everything else initial.
fn anonymous_style(container: &ComputedStyle) -> Rc<ComputedStyle> {
    let mut style = ComputedStyle::inherit_from(container);
    Rc::make_mut(&mut style.display_box).display = Display::Block;
    Rc::new(style)
}

/// Where a list's item numbers start: an `<ol start>`, else 1.
fn list_start(doc: &Document, node: NodeId) -> i64 {
    if html_name(doc, node) != Some(local_name!("ol")) {
        return 1;
    }
    attribute(doc, node, local_name!("start"))
        .and_then(|start| start.trim().parse().ok())
        .unwrap_or(1)
}

/// The marker a list item with `style` and number `ordinal` shows.
fn marker_text(style: ListStyleType, ordinal: i64) -> Option<String> {
    Some(match style {
        ListStyleType::Disc => "•".into(),
        ListStyleType::Circle => "◦".into(),
        ListStyleType::Square => "▪".into(),
        ListStyleType::Decimal => format!("{ordinal}."),
        ListStyleType::None => return None,
    })
}

/// A replaced element's natural size from its `width` and `height`
/// attributes, read as HTML dimension values (digits, then an optional
/// fraction; a percentage or anything unparsable is ignored).
/// A replaced element's natural size and the picture it draws (spec 0004,
/// child 0004, Replaced images). Its `width` and `height` attributes stand
/// in for their presentational hints: set, they win, and one alone keeps
/// the decoded picture's ratio. Until a picture decodes they are all there
/// is (else 0 by 0).
fn replaced_content(doc: &Document, node: NodeId) -> (NaturalSize, Option<u32>) {
    let attributes = natural_size(doc, node);
    let Some(ImageState::Ready { width, height, key }) = doc.image_state(node) else {
        return (attributes, None);
    };
    let (width, height) = (width.max(1) as f32, height.max(1) as f32);
    let (width, height) = match (attributes.width, attributes.height) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, w.scale_by(height / width)),
        (None, Some(h)) => (h.scale_by(width / height), h),
        (None, None) => (Au::from_px(width), Au::from_px(height)),
    };
    let natural = NaturalSize {
        width: Some(width),
        height: Some(height),
    };
    (natural, Some(key))
}

fn natural_size(doc: &Document, node: NodeId) -> NaturalSize {
    let dimension = |name| {
        let value = attribute(doc, node, name)?;
        let value = value.trim_start_matches([' ', '\t', '\n', '\x0C', '\r']);
        let end = value
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(value.len());
        if value[end..].starts_with('%') {
            return None;
        }
        let px: f32 = value.get(..end)?.parse().ok()?;
        (px.is_finite() && px >= 0.0).then(|| Au::from_px(px))
    };
    NaturalSize {
        width: dimension(local_name!("width")),
        height: dimension(local_name!("height")),
    }
}

/// How many columns a tab advances to: tab stops every 8 spaces (spec
/// 0004, child 0004, Whitespace).
const TAB_SIZE: usize = 8;

/// Processes white space across a block's inline content per each text's
/// `white-space` (spec 0004, child 0004, Whitespace table, AC-24):
///
/// - Collapsing values (`normal`, `nowrap`, `pre-line`) turn each run of
///   spaces and tabs (and newlines, unless kept) into one space, and drop a
///   collapsible space right after another one, even across items, and at
///   the start and end of each line.
/// - Values that keep newlines (`pre`, `pre-wrap`, `pre-line`) turn each
///   newline into a forced break.
/// - Preserving values (`pre`, `pre-wrap`) keep spaces, and expand tabs to
///   the next tab stop.
///
/// Text items left empty go.
fn process_white_space(items: Vec<InlineItem>) -> Vec<InlineItem> {
    let mut out: Vec<InlineItem> = Vec::with_capacity(items.len());
    // The last thing was a collapsible space, or the start of a line.
    let mut after_space = true;
    let mut column = 0usize;
    for item in items {
        let InlineItem::Text { node, text, style } = item else {
            match item {
                InlineItem::Break(_) => {
                    trim_trailing_space(&mut out);
                    after_space = true;
                    column = 0;
                }
                InlineItem::Atomic(_) => {
                    after_space = false;
                    column += 1;
                }
                _ => {}
            }
            out.push(item);
            continue;
        };
        let white_space = style.inherited_text.white_space;
        let collapse = matches!(
            white_space,
            WhiteSpace::Normal | WhiteSpace::Nowrap | WhiteSpace::PreLine
        );
        let keep_newlines = matches!(
            white_space,
            WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::PreLine
        );
        let mut current = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            let newline = c == '\n' || c == '\r';
            if newline && c == '\r' && chars.peek() == Some(&'\n') {
                // CR LF is one newline.
                continue;
            }
            if newline && keep_newlines {
                if collapse {
                    // `pre-line` drops the spaces before a newline.
                    while current.ends_with(' ') {
                        current.pop();
                    }
                }
                push_text(&mut out, node, &mut current, &style);
                trim_trailing_space(&mut out);
                out.push(InlineItem::Break(node));
                after_space = true;
                column = 0;
                continue;
            }
            let space = newline || matches!(c, ' ' | '\t' | '\x0C');
            if space && collapse {
                if !after_space {
                    current.push(' ');
                    after_space = true;
                    column += 1;
                }
            } else if c == '\t' {
                let spaces = TAB_SIZE - column % TAB_SIZE;
                current.extend(std::iter::repeat_n(' ', spaces));
                column += spaces;
                after_space = false;
            } else if space {
                current.push(' ');
                column += 1;
                after_space = false;
            } else {
                current.push(c);
                column += 1;
                after_space = false;
            }
        }
        push_text(&mut out, node, &mut current, &style);
    }
    trim_trailing_space(&mut out);
    out
}

fn push_text(
    out: &mut Vec<InlineItem>,
    node: NodeId,
    text: &mut String,
    style: &Rc<ComputedStyle>,
) {
    if text.is_empty() {
        return;
    }
    out.push(InlineItem::Text {
        node,
        text: std::mem::take(text),
        style: Rc::clone(style),
    });
}

/// Drops a collapsible space at the end of the content so far: a line
/// ends there. Inline box edges and absolute boxes in between are looked
/// through.
fn trim_trailing_space(items: &mut Vec<InlineItem>) {
    let last_text = items.iter_mut().rev().find(|item| {
        !matches!(
            item,
            InlineItem::Start(_) | InlineItem::End(_) | InlineItem::Absolute(_)
        )
    });
    if let Some(InlineItem::Text { text, style, .. }) = last_text
        && matches!(
            style.inherited_text.white_space,
            WhiteSpace::Normal | WhiteSpace::Nowrap | WhiteSpace::PreLine
        )
        && text.ends_with(' ')
    {
        text.pop();
    }
    items.retain(|item| !matches!(item, InlineItem::Text { text, .. } if text.is_empty()));
}
