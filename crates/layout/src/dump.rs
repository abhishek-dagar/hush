//! Stable text dumps of the box tree and the fragment tree (spec 0002,
//! AC-17). Boxes and fragments are labeled by their node's tree position
//! path, never by raw ids, so the same input always dumps the same bytes.

use std::fmt::Write;
use std::rc::Rc;

use base::{Au, BoxId, Point, Rect, Size};
use dom::{Document, NodeId};

use crate::{BoxFlags, BoxKind, BoxTree, Fragment, FragmentTree, InlineItem};

impl BoxTree {
    /// Every box from the root down, indented by depth, with its kind, its
    /// node's path, its positioning and any dirty flags, then its inline
    /// content: text, inline box edges, forced breaks, and each atomic
    /// inline's and absolute box's own box and content in place.
    pub fn dump(&self, doc: &Document) -> String {
        enum Entry {
            Box(BoxId, usize),
            Line(String),
        }
        let mut out = String::new();
        let mut stack: Vec<Entry> = self
            .root()
            .map(|root| Entry::Box(root, 0))
            .into_iter()
            .collect();
        while let Some(entry) = stack.pop() {
            let (id, depth) = match entry {
                Entry::Line(line) => {
                    out.push_str(&line);
                    continue;
                }
                Entry::Box(id, depth) => (id, depth),
            };
            let Some(layout_box) = self.get(id) else {
                continue;
            };
            let kind = match (layout_box.kind, layout_box.inline_level) {
                (BoxKind::BlockContainer, false) => "block",
                (BoxKind::BlockContainer, true) => "inline-block",
                (BoxKind::FlexContainer, _) => "flex",
                (BoxKind::InlineBox, _) => "inline",
                (BoxKind::Replaced, _) => "replaced",
            };
            let _ = write!(
                out,
                "{:indent$}{kind} {}",
                "",
                node_label(doc, layout_box.node),
                indent = depth * 2
            );
            match layout_box.style.display_box.position {
                css::Position::Static => {}
                css::Position::Relative => out.push_str(" relative"),
                css::Position::Absolute => out.push_str(" absolute"),
                css::Position::Fixed => out.push_str(" fixed"),
                css::Position::Sticky => out.push_str(" sticky"),
            }
            if let Some(marker) = &layout_box.marker {
                let _ = write!(out, " marker={marker:?}");
            }
            if layout_box.kind == BoxKind::Replaced {
                let axis = |a: Option<Au>| a.map_or_else(|| "auto".into(), px);
                let natural = layout_box.natural;
                let _ = write!(
                    out,
                    " natural={}x{}",
                    axis(natural.width),
                    axis(natural.height)
                );
            }
            if layout_box.flags.contains(BoxFlags::NEEDS_LAYOUT) {
                out.push_str(" needs-layout");
            }
            if layout_box.flags.contains(BoxFlags::CHILD_NEEDS_LAYOUT) {
                out.push_str(" child-needs-layout");
            }
            out.push('\n');
            let indent = (depth + 1) * 2;
            let label = |id: BoxId| node_label(doc, self.get(id).and_then(|b| b.node));
            let mut entries: Vec<Entry> = layout_box
                .inline
                .iter()
                .map(|item| match item {
                    InlineItem::Text { node, text, .. } => {
                        Entry::Line(format!("{:indent$}text {} {text:?}\n", "", doc.path(*node)))
                    }
                    InlineItem::Start(id) => {
                        Entry::Line(format!("{:indent$}start {}\n", "", label(*id)))
                    }
                    InlineItem::End(id) => {
                        Entry::Line(format!("{:indent$}end {}\n", "", label(*id)))
                    }
                    InlineItem::Break(node) => {
                        Entry::Line(format!("{:indent$}break {}\n", "", doc.path(*node)))
                    }
                    InlineItem::Atomic(id) | InlineItem::Absolute(id) | InlineItem::Float(id) => {
                        Entry::Box(*id, depth + 1)
                    }
                })
                .collect();
            entries.extend(
                layout_box
                    .children
                    .iter()
                    .map(|&c| Entry::Box(c, depth + 1)),
            );
            stack.extend(entries.into_iter().rev());
        }
        out
    }
}

impl FragmentTree {
    /// Every fragment from the root down, indented by depth: its node's
    /// path, its offset from its parent, its size, and its scrollable
    /// overflow when that reaches past the border box; for an inline box's
    /// piece, which edges it lacks; for a line, its baseline; for text, its
    /// baseline and characters. Lengths are in CSS px.
    pub fn dump(&self, doc: &Document) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "fragments viewport={}", size(self.viewport));
        let mut stack: Vec<(Point, &Rc<Fragment>, usize)> = self
            .root
            .iter()
            .map(|(at, fragment)| (*at, fragment, 1))
            .collect();
        while let Some((at, fragment, depth)) = stack.pop() {
            let indent = depth * 2;
            match &**fragment {
                Fragment::Text(text) => {
                    let _ = writeln!(
                        out,
                        "{:indent$}text {} at={} size={} baseline={} {:?}",
                        "",
                        doc.path(text.node),
                        point(at),
                        size(text.size),
                        px(text.baseline),
                        text.text,
                    );
                }
                Fragment::Line(line) => {
                    let _ = writeln!(
                        out,
                        "{:indent$}line at={} size={} baseline={}",
                        "",
                        point(at),
                        size(line.size),
                        px(line.baseline),
                    );
                }
                Fragment::Box(fragment) => {
                    let _ = write!(
                        out,
                        "{:indent$}box {} at={} size={}",
                        "",
                        node_label(doc, fragment.node),
                        point(at),
                        size(fragment.size),
                    );
                    if let Some(edges) = fragment.inline_edges {
                        out.push_str(" inline");
                        if !edges.start {
                            out.push_str(" no-start");
                        }
                        if !edges.end {
                            out.push_str(" no-end");
                        }
                    }
                    if fragment.scrollable_overflow != Rect::new(Point::default(), fragment.size) {
                        let _ = write!(out, " overflow={}", rect(fragment.scrollable_overflow));
                    }
                    if fragment.is_scroll_container {
                        out.push_str(" scroll-container");
                    }
                    out.push('\n');
                }
            }
            stack.extend(
                fragment
                    .children()
                    .iter()
                    .rev()
                    .map(|(at, child)| (*at, child, depth + 1)),
            );
        }
        out
    }
}

fn node_label(doc: &Document, node: Option<NodeId>) -> String {
    node.map_or_else(|| "(anonymous)".into(), |node| doc.path(node))
}

/// CSS px with at most two decimals and no trailing zeros.
fn px(au: Au) -> String {
    let text = format!("{:.2}", au.to_f32_px());
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".into()
    } else {
        text.into()
    }
}

fn point(p: Point) -> String {
    format!("{},{}", px(p.x), px(p.y))
}

fn size(s: Size) -> String {
    format!("{}x{}", px(s.width), px(s.height))
}

fn rect(r: Rect) -> String {
    format!("({} {})", point(r.origin), size(r.size))
}
