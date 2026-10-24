//! Inline layout: a block's inline content broken into line boxes (spec
//! 0004, child 0004, Inline formatting, AC-24).
//!
//! The content is cut into pieces no line may break inside: stretches of
//! glyphs up to a break opportunity, inline box edges, atomic inlines and
//! forced breaks. Lines are filled greedily. Each line is as tall as its
//! strut and every item on it, set on one shared baseline, and its content
//! is aligned by `text-align`. An inline box that spans several lines gets
//! one box fragment per line, holding the text inside it, with its start
//! edge only on the first and its end edge only on the last.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use base::{Au, BoxId, Point, Rect, SideOffsets, Size};
use css::{ComputedStyle, FontStyle, LineHeight, TextAlign, TextOverflow, WhiteSpace};
use dom::NodeId;
use text::{FontDescriptor, LineMetrics, ShapedRun, Slant};

use crate::block::{overflow_of, union};
use crate::box_tree::{BoxTree, InlineItem};
use crate::exclusion::ExclusionSpace;
use crate::fragment::{
    BoxFragment, Fragment, InlineEdges, LineFragment, PendingAbsolute, PlacedGlyph, TextFragment,
};
use crate::{Fonts, border_widths, positioned};

/// The fonts a style's text asks for.
pub(crate) fn font_descriptor(style: &ComputedStyle) -> FontDescriptor<'_> {
    FontDescriptor {
        families: &style.font.family,
        weight: style.font.weight,
        slant: match style.font.style {
            FontStyle::Normal => Slant::Normal,
            FontStyle::Italic => Slant::Italic,
            FontStyle::Oblique => Slant::Oblique,
        },
    }
}

/// Whether text in this mode may wrap at soft break opportunities.
fn wraps(white_space: WhiteSpace) -> bool {
    matches!(
        white_space,
        WhiteSpace::Normal | WhiteSpace::PreWrap | WhiteSpace::PreLine
    )
}

/// Whether spaces at the end of a line in this mode hang past its edge
/// instead of taking room.
fn spaces_hang(white_space: WhiteSpace) -> bool {
    white_space != WhiteSpace::Pre
}

/// What a piece is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PieceKind {
    /// Glyphs of one shaped run of one text item.
    Text {
        item: usize,
        run: usize,
        glyphs: Range<usize>,
    },
    /// An inline box's start edge.
    Start(BoxId),
    /// An inline box's end edge.
    End(BoxId),
    /// An atomic inline.
    Atomic(BoxId),
    /// An absolute box's static position, taking no room.
    Absolute(BoxId),
    /// A float's marker among inline content: it takes no room on the line;
    /// the float is placed through the exclusion space (spec 0009, child
    /// 0001).
    Float(BoxId),
    /// A forced line break.
    Break,
}

/// A stretch of inline content no line may break inside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Piece {
    pub(crate) kind: PieceKind,
    /// The whole advance, trailing spaces included.
    pub(crate) width: Au,
    /// The advance of the spaces at its end, which may hang past a line's
    /// end.
    pub(crate) trailing: Au,
    /// A line may break before it.
    pub(crate) breaks_before: bool,
    /// A line may break after it.
    pub(crate) breaks_after: bool,
}

impl Piece {
    fn new(kind: PieceKind, width: Au) -> Self {
        Self {
            kind,
            width,
            trailing: Au::ZERO,
            breaks_before: false,
            breaks_after: false,
        }
    }
}

/// Whether a line may break between `pieces[i - 1]` and `pieces[i]`.
fn opportunity(pieces: &[Piece], i: usize) -> bool {
    i > 0
        && (pieces.get(i - 1).is_some_and(|p| p.breaks_after)
            || pieces.get(i).is_some_and(|p| p.breaks_before))
}

/// Breaks `pieces` into lines no wider than `available`, greedily: each line
/// takes pieces until the next would overflow (not counting the spaces that
/// hang at its end), then breaks at the last opportunity. Content with no
/// opportunity overflows until the next one. Forced breaks end their line.
/// `line_top` advances by `line_height` per line, so the exclusion space
/// narrows each line beside the floats (spec 0009, child 0001).
/// Returns each line's range of pieces.
pub(crate) fn break_lines(
    pieces: &[Piece],
    available: Au,
    exclusions: &ExclusionSpace,
    line_height: Au,
) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    // The width of `pieces[start..i]`.
    let mut width = Au::ZERO;
    // The last opportunity on this line, and the width before it.
    let mut last: Option<(usize, Au)> = None;
    // The block offset of the current line, used to ask the exclusion space
    // how wide the line may be beside the floats.
    let mut line_top = Au::ZERO;
    for (i, piece) in pieces.iter().enumerate() {
        if i > start && opportunity(pieces, i) {
            let hang = pieces.get(i - 1).map_or(Au::ZERO, |p| p.trailing);
            let available_line = line_width(available, exclusions, line_top, piece.width);
            if width - hang > available_line {
                // Overflowing content with no earlier opportunity ends here.
                lines.push(start..i);
                start = i;
                width = Au::ZERO;
                last = None;
                line_top += line_height;
            } else {
                last = Some((i, width));
            }
        }
        if piece.kind == PieceKind::Break {
            lines.push(start..i + 1);
            start = i + 1;
            width = Au::ZERO;
            last = None;
            line_top += line_height;
            continue;
        }
        width += piece.width;
        let available_line = line_width(available, exclusions, line_top, piece.width);
        if width - piece.trailing > available_line
            && let Some((at, before)) = last.take()
        {
            lines.push(start..at);
            start = at;
            width -= before;
            line_top += line_height;
        }
    }
    if start < pieces.len() {
        lines.push(start..pieces.len());
    }
    lines
}

/// How wide a line whose top is `line_top` may be, beside the floats, when
/// `min_width` must fit: the exclusion space's band at `line_top`.
fn line_width(available: Au, exclusions: &ExclusionSpace, line_top: Au, min_width: Au) -> Au {
    exclusions
        .opportunity(line_top, available, min_width)
        .rect
        .size
        .width
}

/// A text item shaped, with its style.
struct ShapedText<'a> {
    node: NodeId,
    text: &'a str,
    style: &'a Rc<ComputedStyle>,
    runs: Vec<ShapedRun>,
}

fn shape_items<'a>(items: &'a [InlineItem], fonts: &mut Fonts<'_>) -> Vec<Option<ShapedText<'a>>> {
    items
        .iter()
        .map(|item| {
            let InlineItem::Text { node, text, style } = item else {
                return None;
            };
            let runs = fonts.shape(text, &font_descriptor(style), style.font.size);
            Some(ShapedText {
                node: *node,
                text,
                style,
                runs,
            })
        })
        .collect()
}

/// An inline box's margin, border and padding on each side, with
/// percentages against `basis` (zero without one).
fn edges(tree: &BoxTree, id: BoxId, basis: Option<Au>) -> (SideOffsets, SideOffsets, SideOffsets) {
    let Some(style) = tree.get(id).map(|b| &b.style) else {
        return Default::default();
    };
    let side = |l: Option<css::Length>| l.map_or(Au::ZERO, |l| l.resolve(basis));
    let margin = SideOffsets {
        top: side(style.margin.top),
        right: side(style.margin.right),
        bottom: side(style.margin.bottom),
        left: side(style.margin.left),
    };
    let padding = SideOffsets {
        top: style.padding.top.resolve(basis),
        right: style.padding.right.resolve(basis),
        bottom: style.padding.bottom.resolve(basis),
        left: style.padding.left.resolve(basis),
    };
    (margin, border_widths(style), padding)
}

/// Cuts the content into pieces. `atomic` gives an atomic inline's margin
/// box width (`None` when it was not laid out, which leaves it out).
fn build_pieces(
    tree: &BoxTree,
    items: &[InlineItem],
    shaped: &[Option<ShapedText<'_>>],
    basis: Option<Au>,
    atomic: impl Fn(BoxId) -> Option<Au>,
) -> Vec<Piece> {
    let mut pieces: Vec<Piece> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        match item {
            InlineItem::Text { .. } => {
                if let Some(Some(text)) = shaped.get(index) {
                    text_pieces(index, text, &mut pieces);
                }
            }
            InlineItem::Start(id) => {
                let (margin, border, padding) = edges(tree, *id, basis);
                let width = margin.left + border.left + padding.left;
                pieces.push(Piece::new(PieceKind::Start(*id), width));
            }
            InlineItem::End(id) => {
                let (margin, border, padding) = edges(tree, *id, basis);
                let width = margin.right + border.right + padding.right;
                pieces.push(Piece::new(PieceKind::End(*id), width));
            }
            InlineItem::Atomic(id) => {
                let Some(width) = atomic(*id) else {
                    continue;
                };
                // Atomic inlines break like ideographs: on both sides,
                // where the text around them may wrap.
                let wrap = tree
                    .get(*id)
                    .is_some_and(|b| wraps(b.style.inherited_text.white_space));
                let mut piece = Piece::new(PieceKind::Atomic(*id), width);
                piece.breaks_before = wrap;
                piece.breaks_after = wrap;
                pieces.push(piece);
            }
            InlineItem::Absolute(id) => pieces.push(Piece::new(PieceKind::Absolute(*id), Au::ZERO)),
            InlineItem::Float(id) => pieces.push(Piece::new(PieceKind::Float(*id), Au::ZERO)),
            InlineItem::Break(_) => pieces.push(Piece::new(PieceKind::Break, Au::ZERO)),
        }
    }
    // Inline box edges stick to the content they open and close: an
    // opportunity before a start edge or after an end edge moves past it.
    for i in 0..pieces.len() {
        if pieces[i].breaks_before {
            let mut j = i;
            while j > 0 && matches!(pieces[j - 1].kind, PieceKind::Start(_)) {
                j -= 1;
            }
            if j != i {
                pieces[i].breaks_before = false;
                pieces[j].breaks_before = true;
            }
        }
    }
    for i in (0..pieces.len()).rev() {
        if pieces[i].breaks_after {
            let mut j = i;
            while j + 1 < pieces.len() && matches!(pieces[j + 1].kind, PieceKind::End(_)) {
                j += 1;
            }
            if j != i {
                pieces[i].breaks_after = false;
                pieces[j].breaks_after = true;
            }
        }
    }
    pieces
}

/// Splits a text item's runs after each break opportunity: after a space
/// where the text may wrap (in `pre-wrap`, after the last of a run of
/// spaces), and after a zero width space.
fn text_pieces(item: usize, text: &ShapedText<'_>, pieces: &mut Vec<Piece>) {
    let white_space = text.style.inherited_text.white_space;
    let wrap = wraps(white_space);
    let hang = spaces_hang(white_space);
    for (run_index, run) in text.runs.iter().enumerate() {
        let mut start = 0;
        let mut width = Au::ZERO;
        let mut trailing = Au::ZERO;
        for (i, glyph) in run.glyphs.iter().enumerate() {
            width += glyph.advance;
            let at = glyph.cluster as usize;
            let mut rest = text.text.get(at..).unwrap_or_default().chars();
            let c = rest.next();
            let is_space = c == Some(' ');
            trailing = if is_space && hang {
                trailing + glyph.advance
            } else {
                Au::ZERO
            };
            // Only after the last glyph of a cluster.
            let cluster_ends = run
                .glyphs
                .get(i + 1)
                .is_none_or(|next| next.cluster != glyph.cluster);
            let space_breaks =
                is_space && (white_space != WhiteSpace::PreWrap || rest.next() != Some(' '));
            let breaks = wrap && cluster_ends && (space_breaks || c == Some('\u{200B}'));
            if breaks || i + 1 == run.glyphs.len() {
                pieces.push(Piece {
                    kind: PieceKind::Text {
                        item,
                        run: run_index,
                        glyphs: start..i + 1,
                    },
                    width,
                    trailing,
                    breaks_before: false,
                    breaks_after: breaks,
                });
                start = i + 1;
                width = Au::ZERO;
                trailing = Au::ZERO;
            }
        }
    }
}

/// A block's lines, laid out.
pub(crate) struct Lines {
    /// The line fragments, placed from the content box's top left.
    pub(crate) fragments: Vec<(Point, Rc<Fragment>)>,
    /// The first line's baseline from the content box's top.
    pub(crate) first_baseline: Option<Au>,
    /// The last line's baseline from the content box's top.
    pub(crate) last_baseline: Option<Au>,
    /// The height of all lines together.
    pub(crate) height: Au,
    /// At least one line box holds something. Lines of nothing but empty
    /// inline elements take no space and count as no lines (CSS 2.2
    /// section 9.4.2).
    pub(crate) has_content: bool,
    /// The absolute boxes in the content and the ones atomic inlines hand
    /// on, with static positions from the content box's top left.
    pub(crate) absolutes: Vec<PendingAbsolute>,
}

/// What one line needs from the block around it.
struct LineContext<'a, 'b, 'f> {
    tree: &'a BoxTree,
    shaped: &'a [Option<ShapedText<'b>>],
    atomics: &'a HashMap<BoxId, Rc<Fragment>>,
    available: Au,
    fonts: &'a mut Fonts<'f>,
    /// Each inline box's font ascent and descent (its content area) and its
    /// extents on the line, looked up once.
    box_metrics: HashMap<BoxId, (LineMetrics, (Au, Au))>,
}

impl LineContext<'_, '_, '_> {
    fn box_metrics(&mut self, id: BoxId) -> (LineMetrics, (Au, Au)) {
        if let Some(found) = self.box_metrics.get(&id) {
            return *found;
        }
        let found = self
            .tree
            .get(id)
            .and_then(|b| {
                let style = &b.style;
                let metrics = self
                    .fonts
                    .metrics(&font_descriptor(style), style.font.size)?;
                Some((metrics, extents(metrics, style)))
            })
            .unwrap_or_default();
        self.box_metrics.insert(id, found);
        found
    }
}

/// An atomic inline's ascent and descent around the baseline, margins
/// included: an inline block sits on its last line's baseline, and one
/// without lines (or that clips its overflow), like an image, on its
/// bottom margin edge (CSS 2.2 section 10.8.1).
fn atomic_extents(fragment: &BoxFragment) -> (Au, Au) {
    let top = fragment.margins.top.resolve();
    let total = top + fragment.size.height + fragment.margins.bottom.resolve();
    let ascent = match fragment.last_baseline {
        Some(baseline) if !fragment.is_scroll_container => top + baseline,
        _ => total,
    };
    (ascent, total - ascent)
}

fn atomic_width(fragment: &Fragment) -> Option<Au> {
    let fragment = fragment.as_box()?;
    Some(fragment.margins.left + fragment.size.width + fragment.margins.right)
}

/// Lays out `items` in lines `available` wide. `style` is the block's, for
/// the strut every line starts from, the alignment, and the ellipsis an
/// overflowing line ends in when it clips with `text-overflow: ellipsis`.
/// `atomics` holds the laid out atomic inlines.
pub(crate) fn lay_out_lines(
    tree: &BoxTree,
    items: &[InlineItem],
    style: &Rc<ComputedStyle>,
    available: Au,
    exclusions: &ExclusionSpace,
    atomics: &HashMap<BoxId, Rc<Fragment>>,
    fonts: &mut Fonts<'_>,
) -> Lines {
    let shaped = shape_items(items, fonts);
    let pieces = build_pieces(tree, items, &shaped, Some(available), |id| {
        atomics.get(&id).and_then(|f| atomic_width(f))
    });
    let strut = fonts
        .metrics(&font_descriptor(style), style.font.size)
        .map(|metrics| extents(metrics, style))
        .unwrap_or_default();
    // Each line starts one line-height below the last, so the exclusion
    // space narrows lines beside floats at the right block offset.
    let line_height = strut.0 + strut.1;
    let display_box = &style.display_box;
    let ellipsis = (display_box.text_overflow == TextOverflow::Ellipsis
        && display_box.clips_overflow())
    .then(|| Ellipsis::shape(style, fonts));
    let mut cx = LineContext {
        tree,
        shaped: &shaped,
        atomics,
        available,
        fonts,
        box_metrics: HashMap::new(),
    };

    let mut lines = Lines {
        fragments: Vec::new(),
        first_baseline: None,
        last_baseline: None,
        height: Au::ZERO,
        has_content: false,
        absolutes: Vec::new(),
    };
    // Inline boxes open at the start of the next line, outermost first.
    let mut open: Vec<BoxId> = Vec::new();
    for range in break_lines(&pieces, available, exclusions, line_height) {
        let line = pieces.get(range).unwrap_or_default();
        let has_content = line.iter().any(|piece| match &piece.kind {
            PieceKind::Text { .. }
            | PieceKind::Atomic(_)
            | PieceKind::Float(_)
            | PieceKind::Break => true,
            PieceKind::Start(_) | PieceKind::End(_) => piece.width > Au::ZERO,
            PieceKind::Absolute(_) => false,
        });
        if !has_content {
            for piece in line {
                match piece.kind {
                    PieceKind::Start(id) => open.push(id),
                    PieceKind::End(id) => close_open(&mut open, id),
                    PieceKind::Absolute(box_id) => lines.absolutes.push(PendingAbsolute {
                        box_id,
                        static_position: Point::new(Au::ZERO, lines.height),
                    }),
                    _ => {}
                }
            }
            continue;
        }
        lines.has_content = true;
        // An overflowing line that ends in an ellipsis keeps only what fits
        // beside it (AC-9).
        let cut = ellipsis
            .as_ref()
            .filter(|_| used_width(line) > available)
            .map(|ellipsis| cut_line(line, cx.shaped, (available - ellipsis.width).max(Au::ZERO)));
        let (line, ellipsis_node) = match &cut {
            Some((kept, node)) => (kept.as_slice(), *node),
            None => (line, None),
        };
        let shown = ellipsis.as_ref().zip(ellipsis_node);

        let (mut ascent, mut descent) = strut;
        let mut grow = |(a, d): (Au, Au)| {
            ascent = ascent.max(a);
            descent = descent.max(d);
        };
        for &id in &open {
            grow(cx.box_metrics(id).1);
        }
        for piece in line {
            match &piece.kind {
                PieceKind::Text { item, run, .. } => {
                    if let Some(Some(text)) = cx.shaped.get(*item)
                        && let Some(run) = text.runs.get(*run)
                    {
                        grow(extents(run.metrics, text.style));
                    }
                }
                PieceKind::Start(id) => grow(cx.box_metrics(*id).1),
                PieceKind::Atomic(id) => {
                    if let Some(fragment) = cx.atomics.get(id).and_then(|f| f.as_box()) {
                        grow(atomic_extents(fragment));
                    }
                }
                PieceKind::End(_) | PieceKind::Absolute(_) | PieceKind::Break => {}
                PieceKind::Float(id) => {
                    if let Some(fragment) = cx.atomics.get(id).and_then(|f| f.as_box()) {
                        grow(atomic_extents(fragment));
                    }
                }
            }
        }
        if let Some((ellipsis, _)) = shown {
            for run in &ellipsis.runs {
                grow(extents(run.metrics, style));
            }
        }

        let used = used_width(line) + shown.map_or(Au::ZERO, |(e, _)| e.width);
        let x0 = match style.inherited_text.text_align {
            TextAlign::Start | TextAlign::Left | TextAlign::Justify => Au::ZERO,
            TextAlign::End | TextAlign::Right => (available - used).max(Au::ZERO),
            TextAlign::Center => Au((available - used).max(Au::ZERO).0 / 2),
        };
        let top = lines.height;
        let mut absolutes = Vec::new();
        let mut line_fragment = build_line(
            &mut cx,
            line,
            &mut open,
            x0,
            ascent,
            descent,
            &mut absolutes,
        );
        if let Some((ellipsis, node)) = shown {
            let mut x = x0 + line.iter().map(|piece| piece.width).sum::<Au>();
            for run in &ellipsis.runs {
                let fragment = text_fragment(node, ellipsis.text, style, run, 0..run.glyphs.len());
                let at = Point::new(x, ascent - fragment.baseline);
                x += fragment.size.width;
                line_fragment
                    .children
                    .push((at, Rc::new(Fragment::Text(fragment))));
            }
        }
        lines.absolutes.extend(
            absolutes
                .into_iter()
                .map(|a| a.offset_by(Point::new(Au::ZERO, top))),
        );
        lines.first_baseline.get_or_insert(top + ascent);
        lines.last_baseline = Some(top + ascent);
        lines.fragments.push((
            Point::new(Au::ZERO, top),
            Rc::new(Fragment::Line(line_fragment)),
        ));
        lines.height += ascent + descent;
    }
    lines
}

/// The `…` an overflowing line ends in under `text-overflow: ellipsis`,
/// shaped in the block's style, or `...` when its fonts lack the glyph
/// (spec 0006, Value sourcing, Ellipsis).
struct Ellipsis {
    text: &'static str,
    runs: Vec<ShapedRun>,
    width: Au,
}

impl Ellipsis {
    fn shape(style: &ComputedStyle, fonts: &mut Fonts<'_>) -> Self {
        let descriptor = font_descriptor(style);
        let mut text = "\u{2026}";
        let mut runs = fonts.shape(text, &descriptor, style.font.size);
        // Glyph 0 is a font's missing glyph.
        let missing = runs
            .iter()
            .flat_map(|run| &run.glyphs)
            .any(|glyph| glyph.id == 0);
        if missing {
            text = "...";
            runs = fonts.shape(text, &descriptor, style.font.size);
        }
        let width = runs.iter().map(ShapedRun::width).sum();
        Self { text, runs, width }
    }
}

/// The pieces of an overflowing line that fit in `limit` before its
/// ellipsis (spec 0006, AC-9): the text up to the last whole glyph cluster
/// that fits, and atomic inlines only if they fit whole. After the cut only
/// inline box edges stay, so every box still closes. Also says which text
/// node the ellipsis follows, or `None` for a line without text before
/// its cut, which then shows no ellipsis.
fn cut_line(
    line: &[Piece],
    shaped: &[Option<ShapedText<'_>>],
    limit: Au,
) -> (Vec<Piece>, Option<NodeId>) {
    let mut kept = Vec::with_capacity(line.len());
    let mut x = Au::ZERO;
    let mut cut = false;
    let mut node = None;
    for piece in line {
        match &piece.kind {
            PieceKind::Start(_)
            | PieceKind::End(_)
            | PieceKind::Absolute(_)
            | PieceKind::Float(_) => {
                x += piece.width;
                kept.push(piece.clone());
            }
            _ if cut => {}
            PieceKind::Text { item, run, glyphs } => {
                let Some(Some(text)) = shaped.get(*item) else {
                    continue;
                };
                node = Some(text.node);
                if x + piece.width <= limit {
                    x += piece.width;
                    kept.push(piece.clone());
                    continue;
                }
                cut = true;
                let Some(run_glyphs) = text.runs.get(*run).map(|r| &r.glyphs) else {
                    continue;
                };
                // Whole clusters only: a cluster's glyphs share an index.
                let (mut end, mut width) = (glyphs.start, Au::ZERO);
                let mut i = glyphs.start;
                while i < glyphs.end {
                    let cluster = run_glyphs.get(i).map(|g| g.cluster);
                    let mut j = i;
                    let mut advance = Au::ZERO;
                    while j < glyphs.end && run_glyphs.get(j).map(|g| g.cluster) == cluster {
                        advance += run_glyphs.get(j).map_or(Au::ZERO, |g| g.advance);
                        j += 1;
                    }
                    if x + width + advance > limit {
                        break;
                    }
                    width += advance;
                    i = j;
                    end = j;
                }
                if end > glyphs.start {
                    x += width;
                    kept.push(Piece {
                        kind: PieceKind::Text {
                            item: *item,
                            run: *run,
                            glyphs: glyphs.start..end,
                        },
                        width,
                        trailing: Au::ZERO,
                        breaks_before: piece.breaks_before,
                        breaks_after: false,
                    });
                }
            }
            PieceKind::Atomic(_) => {
                if x + piece.width <= limit {
                    x += piece.width;
                    kept.push(piece.clone());
                } else {
                    cut = true;
                }
            }
            PieceKind::Break => kept.push(piece.clone()),
        }
    }
    (kept, node)
}

/// The width a line's content takes for alignment: everything but the
/// spaces hanging at its end.
fn used_width(line: &[Piece]) -> Au {
    let total: Au = line.iter().map(|p| p.width).sum();
    let hang = line
        .iter()
        .rev()
        .find(|p| {
            !matches!(
                p.kind,
                PieceKind::End(_) | PieceKind::Absolute(_) | PieceKind::Break
            )
        })
        .map_or(Au::ZERO, |p| p.trailing);
    total - hang
}

/// Removes the innermost `id` from the open inline boxes.
fn close_open(open: &mut Vec<BoxId>, id: BoxId) {
    if let Some(at) = open.iter().rposition(|&o| o == id) {
        open.truncate(at);
    }
}

/// An inline box being filled on the current line.
struct OpenBox {
    id: BoxId,
    /// Its border box's left edge on this line.
    start_x: Au,
    /// Its start edge is on this line.
    first: bool,
    /// Its content, at positions from the line's top left.
    children: Vec<(Point, Rc<Fragment>)>,
}

/// Builds one line box. `open` holds the inline boxes open at its start and
/// is left holding those open at its end. Absolute boxes on the line, and
/// those its atomic inlines hand on, go to `absolutes`, from the line's top
/// left.
fn build_line(
    cx: &mut LineContext<'_, '_, '_>,
    line: &[Piece],
    open: &mut Vec<BoxId>,
    x0: Au,
    ascent: Au,
    descent: Au,
    absolutes: &mut Vec<PendingAbsolute>,
) -> LineFragment {
    let baseline = ascent;
    let mut stack: Vec<OpenBox> = open
        .iter()
        .map(|&id| OpenBox {
            id,
            start_x: x0,
            first: false,
            children: Vec::new(),
        })
        .collect();
    let mut children: Vec<(Point, Rc<Fragment>)> = Vec::new();
    let mut x = x0;
    let mut i = 0;
    while let Some(piece) = line.get(i) {
        match &piece.kind {
            PieceKind::Text { item, run, glyphs } => {
                // One fragment for the run's consecutive pieces.
                let mut end = glyphs.end;
                let mut j = i + 1;
                while let Some(PieceKind::Text {
                    item: next_item,
                    run: next_run,
                    glyphs: next,
                }) = line.get(j).map(|p| &p.kind)
                {
                    if next_item != item || next_run != run {
                        break;
                    }
                    end = next.end;
                    j += 1;
                }
                i = j;
                let Some(Some(text)) = cx.shaped.get(*item) else {
                    continue;
                };
                let Some(shaped_run) = text.runs.get(*run) else {
                    continue;
                };
                let fragment = text_fragment(
                    text.node,
                    text.text,
                    text.style,
                    shaped_run,
                    glyphs.start..end,
                );
                let at = Point::new(x, baseline - fragment.baseline);
                x += fragment.size.width;
                push_child(&mut stack, &mut children, at, Fragment::Text(fragment));
                continue;
            }
            PieceKind::Start(id) => {
                let (margin, border, padding) = edges(cx.tree, *id, Some(cx.available));
                x += margin.left;
                stack.push(OpenBox {
                    id: *id,
                    start_x: x,
                    first: true,
                    children: Vec::new(),
                });
                x += border.left + padding.left;
                open.push(*id);
            }
            PieceKind::End(id) => {
                let (margin, border, padding) = edges(cx.tree, *id, Some(cx.available));
                x += border.right + padding.right;
                if let Some(at) = stack.iter().rposition(|o| o.id == *id) {
                    while stack.len() > at {
                        let Some(done) = stack.pop() else { break };
                        let ends = stack.len() == at;
                        close_box(cx, done, ends, x, baseline, &mut stack, &mut children);
                    }
                }
                x += margin.right;
                close_open(open, *id);
            }
            PieceKind::Atomic(id) => {
                if let Some(fragment) = cx.atomics.get(id)
                    && let Some(atomic) = fragment.as_box()
                {
                    let (atomic_ascent, _) = atomic_extents(atomic);
                    let at = Point::new(
                        x + atomic.margins.left,
                        baseline - atomic_ascent + atomic.margins.top.resolve(),
                    )
                    .offset_by(atomic.relative_offset);
                    absolutes.extend(atomic.absolutes.iter().map(|a| a.offset_by(at)));
                    let target = match stack.last_mut() {
                        Some(top) => &mut top.children,
                        None => &mut children,
                    };
                    target.push((at, Rc::clone(fragment)));
                }
                x += piece.width;
            }
            PieceKind::Absolute(box_id) => absolutes.push(PendingAbsolute {
                box_id: *box_id,
                static_position: Point::new(x, Au::ZERO),
            }),
            PieceKind::Float(_) => {}
            PieceKind::Break => {}
        }
        i += 1;
    }
    // Boxes still open go on in the next line.
    while let Some(done) = stack.pop() {
        close_box(cx, done, false, x, baseline, &mut stack, &mut children);
    }
    LineFragment {
        size: Size::new(cx.available.max(x), ascent + descent),
        baseline,
        children,
    }
}

fn push_child(
    stack: &mut [OpenBox],
    children: &mut Vec<(Point, Rc<Fragment>)>,
    at: Point,
    fragment: Fragment,
) {
    let target = match stack.last_mut() {
        Some(top) => &mut top.children,
        None => children,
    };
    target.push((at, Rc::new(fragment)));
}

/// Ends one line's piece of an inline box at `end_x`. Its border box
/// wraps the font's content area (ascent plus descent) and its vertical
/// padding and border, which do not change the line's height (CSS 2.2
/// section 10.6.1).
fn close_box(
    cx: &mut LineContext<'_, '_, '_>,
    done: OpenBox,
    ends: bool,
    end_x: Au,
    baseline: Au,
    stack: &mut [OpenBox],
    children: &mut Vec<(Point, Rc<Fragment>)>,
) {
    let (metrics, _) = cx.box_metrics(done.id);
    let (_, border, padding) = edges(cx.tree, done.id, Some(cx.available));
    let top = baseline - metrics.ascent - padding.top - border.top;
    let origin = Point::new(done.start_x, top);
    // A relative inline box moves with everything in it; the line does
    // not change.
    let shift = cx.tree.get(done.id).map_or(Point::default(), |b| {
        positioned::relative_offset(&b.style, Size::new(Some(cx.available), None))
    });
    let size = Size::new(
        (end_x - done.start_x).max(Au::ZERO),
        metrics.ascent + metrics.descent + padding.vertical() + border.vertical(),
    );
    let inner: Vec<(Point, Rc<Fragment>)> = done
        .children
        .into_iter()
        .map(|(at, child)| (Point::new(at.x - origin.x, at.y - origin.y), child))
        .collect();
    let scrollable_overflow = inner
        .iter()
        .map(|(at, child)| {
            let area = overflow_of(child);
            Rect::new(at.offset_by(area.origin), area.size)
        })
        .fold(Rect::new(Point::default(), size), union);
    let fragment = BoxFragment {
        box_id: done.id,
        node: cx.tree.get(done.id).and_then(|b| b.node),
        size,
        children: inner,
        scrollable_overflow,
        baseline: None,
        last_baseline: None,
        is_scroll_container: false,
        margins: Default::default(),
        padding,
        inline_edges: Some(InlineEdges {
            start: done.first,
            end: ends,
        }),
        auto_height: Au::ZERO,
        relative_offset: Point::default(),
        absolutes: Vec::new(),
        exclusions_out: None,
    };
    push_child(
        stack,
        children,
        origin.offset_by(shift),
        Fragment::Box(fragment),
    );
}

/// The min content and max content widths of inline content (spec 0004,
/// child 0004, Intrinsic sizes): the widest piece between break
/// opportunities, and the widest line between forced breaks. `atomics`
/// gives each atomic inline's min and max margin box widths.
pub(crate) fn intrinsic_sizes(
    tree: &BoxTree,
    items: &[InlineItem],
    atomics: &HashMap<BoxId, (Au, Au)>,
    fonts: &mut Fonts<'_>,
) -> (Au, Au) {
    let shaped = shape_items(items, fonts);
    let pieces = build_pieces(tree, items, &shaped, None, |id| {
        atomics.get(&id).map(|_| Au::ZERO)
    });
    let width = |piece: &Piece, max: bool| match piece.kind {
        PieceKind::Atomic(id) => atomics
            .get(&id)
            .map_or(Au::ZERO, |&(min, most)| if max { most } else { min }),
        _ => piece.width,
    };
    let mut min_content = Au::ZERO;
    let mut max_content = Au::ZERO;
    let mut stretch = Au::ZERO;
    let mut line = Au::ZERO;
    let mut last_trailing = Au::ZERO;
    for (i, piece) in pieces.iter().enumerate() {
        if opportunity(&pieces, i) {
            min_content = min_content.max(stretch - last_trailing);
            stretch = Au::ZERO;
        }
        if piece.kind == PieceKind::Break {
            min_content = min_content.max(stretch - last_trailing);
            max_content = max_content.max(line - last_trailing);
            stretch = Au::ZERO;
            line = Au::ZERO;
            last_trailing = Au::ZERO;
            continue;
        }
        stretch += width(piece, false);
        line += width(piece, true);
        if !matches!(piece.kind, PieceKind::End(_)) {
            last_trailing = piece.trailing;
        }
    }
    min_content = min_content.max(stretch - last_trailing);
    max_content = max_content.max(line - last_trailing);
    (min_content.max(Au::ZERO), max_content.max(min_content))
}

/// An inline box's ascent and descent around the baseline, with the
/// line height's leading split evenly above and below (CSS 2.2 section
/// 10.8.1).
fn extents(metrics: LineMetrics, style: &ComputedStyle) -> (Au, Au) {
    let content = metrics.ascent + metrics.descent;
    let line_height = match style.inherited_text.line_height {
        LineHeight::Normal => content + metrics.line_gap,
        LineHeight::Number(factor) => style.font.size.scale_by(factor),
        LineHeight::Length(length) => length.resolve(Some(style.font.size)),
    };
    let half_leading = (line_height - content).scale_by(0.5);
    (
        metrics.ascent + half_leading,
        line_height - metrics.ascent - half_leading,
    )
}

/// The fragment for `glyphs` of `run`, shaped from `text`: as wide as their
/// advances, as tall as the font's ascent plus descent, with the baseline
/// the ascent below its top.
pub(crate) fn text_fragment(
    node: NodeId,
    text: &str,
    style: &Rc<ComputedStyle>,
    run: &ShapedRun,
    glyphs: Range<usize>,
) -> TextFragment {
    let run_glyphs = run.glyphs.get(glyphs).unwrap_or_default();
    let mut placed = Vec::with_capacity(run_glyphs.len());
    let mut x = Au::ZERO;
    for glyph in run_glyphs {
        placed.push(PlacedGlyph {
            id: glyph.id,
            x: x + glyph.x_offset,
            y: -glyph.y_offset,
            cluster: glyph.cluster,
        });
        x += glyph.advance;
    }
    let text_start = run_glyphs.first().map_or(0, |g| g.cluster as usize);
    let text_end = run_glyphs
        .iter()
        .map(|glyph| {
            let at = glyph.cluster as usize;
            text.get(at..)
                .and_then(|rest| rest.chars().next())
                .map_or(at, |c| at + c.len_utf8())
        })
        .max()
        .unwrap_or(text_start);
    TextFragment {
        node,
        style: Rc::clone(style),
        font: run.font,
        font_size: run.size,
        size: Size::new(x, run.metrics.ascent + run.metrics.descent),
        baseline: run.metrics.ascent,
        underline_offset: run.metrics.underline_offset,
        underline_thickness: run.metrics.underline_thickness,
        glyphs: placed,
        text: text
            .get(text_start..text_end)
            .unwrap_or_default()
            .to_owned(),
        text_start: u32::try_from(text_start).unwrap_or(u32::MAX),
        hidden_font: run.hidden_font,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(width: f32, trailing: f32) -> Piece {
        Piece {
            kind: PieceKind::Text {
                item: 0,
                run: 0,
                glyphs: 0..1,
            },
            width: Au::from_px(width),
            trailing: Au::from_px(trailing),
            breaks_before: false,
            breaks_after: trailing > 0.0,
        }
    }

    #[test]
    fn lines_fill_greedily_and_trailing_spaces_hang() {
        // "aaa bbb ccc" with 10px letters and 5px spaces.
        let words = [word(35.0, 5.0), word(35.0, 5.0), word(30.0, 0.0)];
        let empty = &ExclusionSpace::empty();
        assert_eq!(
            break_lines(&words, Au::from_px(100.0), empty, Au::from_px(16.0)),
            vec![0..3]
        );
        // 35 + 30 = 65 fits only if the second space hangs.
        assert_eq!(
            break_lines(&words, Au::from_px(65.0), empty, Au::from_px(16.0)),
            [0..2, 2..3]
        );
        assert_eq!(
            break_lines(&words, Au::from_px(64.0), empty, Au::from_px(16.0)),
            [0..1, 1..2, 2..3]
        );
    }

    #[test]
    fn an_overlong_word_takes_a_line_of_its_own() {
        let words = [word(10.0, 5.0), word(500.0, 5.0), word(10.0, 0.0)];
        assert_eq!(
            break_lines(
                &words,
                Au::from_px(50.0),
                &ExclusionSpace::empty(),
                Au::from_px(16.0)
            ),
            [0..1, 1..2, 2..3]
        );
        assert_eq!(
            break_lines(
                &[],
                Au::from_px(50.0),
                &ExclusionSpace::empty(),
                Au::from_px(16.0)
            ),
            Vec::<Range<usize>>::new()
        );
    }

    #[test]
    fn a_word_without_opportunities_overflows_until_the_next_one() {
        // "aaa" glued to "bbb " (no space between), then "ccc".
        let mut glued = word(30.0, 0.0);
        glued.breaks_after = false;
        let pieces = [glued, word(35.0, 5.0), word(30.0, 0.0)];
        assert_eq!(
            break_lines(
                &pieces,
                Au::from_px(40.0),
                &ExclusionSpace::empty(),
                Au::from_px(16.0)
            ),
            [0..2, 2..3]
        );
    }

    #[test]
    fn forced_breaks_end_their_line() {
        let mut pieces = vec![word(10.0, 0.0), Piece::new(PieceKind::Break, Au::ZERO)];
        pieces.push(Piece::new(PieceKind::Break, Au::ZERO));
        pieces.push(word(10.0, 0.0));
        assert_eq!(
            break_lines(
                &pieces,
                Au::from_px(500.0),
                &ExclusionSpace::empty(),
                Au::from_px(16.0)
            ),
            [0..2, 2..3, 3..4]
        );
    }

    #[test]
    fn normal_line_height_adds_the_line_gap_and_numbers_scale_the_size() {
        let metrics = LineMetrics {
            ascent: Au::from_px(12.0),
            descent: Au::from_px(4.0),
            line_gap: Au::from_px(2.0),
            ..LineMetrics::default()
        };
        let mut style = ComputedStyle::default();
        assert_eq!(
            extents(metrics, &style),
            (Au::from_px(13.0), Au::from_px(5.0))
        );
        Rc::make_mut(&mut style.inherited_text).line_height = LineHeight::Number(1.5);
        // 16px * 1.5 = 24px: 8px of leading, 4px a side.
        assert_eq!(
            extents(metrics, &style),
            (Au::from_px(16.0), Au::from_px(8.0))
        );
    }
}
