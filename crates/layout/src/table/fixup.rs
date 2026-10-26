//! Anonymous box fixup for tables (CSS Tables 3 section 3).
//!
//! After the box tree is built, table parts that sit in the wrong parent get
//! anonymous boxes around them: a cell in a block gets an anonymous row and
//! table, a row in a block gets an anonymous table. Then every table gets a
//! table wrapper parent, stray cells and content directly inside a table get
//! wrapped in an anonymous row, and stray content directly inside a row gets
//! wrapped in an anonymous cell. Whitespace-only text inside table parts is
//! already dropped by the box tree builder, so here we only restructure boxes.

use base::BoxId;
use css::{ComputedStyle, Display};
use std::rc::Rc;

use crate::box_tree::{BoxKind, BoxTree, InlineItem, LayoutBox};

/// Runs the table anonymous-box fixup on `tree`.
pub(crate) fn fix_tables(tree: &mut BoxTree) {
    let root = match tree.root() {
        Some(root) => root,
        None => return,
    };
    // Table parts in the wrong parent first, so the anonymous tables they
    // make are fixed up like written ones (CSS Tables 3 section 3).
    generate_anonymous_tables(tree, root);
    let tables = collect_tables(tree, root);
    for table in tables {
        fix_table(tree, table);
    }
}

/// Wraps table parts that sit in a parent that cannot hold them in anonymous
/// boxes, bottom up: a cell in a block gets an anonymous row, then the row
/// gets an anonymous table (CSS Tables 3 section 3).
fn generate_anonymous_tables(tree: &mut BoxTree, root: BoxId) {
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        let Some(children) = tree.get(id).map(|b| b.children.clone()) else {
            continue;
        };
        let parts = wrap_table_parts(tree, id, children);
        if let Some(box_) = tree.boxes.get_mut(id) {
            box_.children = parts.clone();
        }
        stack.extend(parts);
    }
}

/// Wraps the runs of `parent`'s children that `parent` cannot hold directly.
fn wrap_table_parts(tree: &mut BoxTree, parent: BoxId, children: Vec<BoxId>) -> Vec<BoxId> {
    let parent_kind = tree.get(parent).map(|b| b.kind);
    let style = tree
        .get(parent)
        .map(|b| Rc::clone(&b.style))
        .unwrap_or_else(ComputedStyle::initial);
    // Cells belong in a row: a block holding cells gets an anonymous row
    // around each run of them.
    let mut parts = children;
    if parent_kind != Some(BoxKind::TableRow) {
        parts = wrap_runs(
            tree,
            parent,
            &style,
            |kind| kind != BoxKind::TableCell,
            BoxKind::TableRow,
            Display::TableRow,
        );
        if let Some(box_) = tree.boxes.get_mut(parent) {
            box_.children = parts.clone();
        }
    }
    // Rows, row groups, columns and captions belong in a table (a row also in
    // a row group): anything else holding them gets an anonymous table around
    // each run.
    if parent_kind != Some(BoxKind::Table) {
        parts = wrap_runs(
            tree,
            parent,
            &style,
            |kind| !is_table_part(kind) || proper_parent(parent_kind, kind),
            BoxKind::Table,
            Display::Table,
        );
    }
    parts
}

/// Whether `child` may sit directly in a `parent` of `parent_kind`.
fn proper_parent(parent_kind: Option<BoxKind>, child: BoxKind) -> bool {
    match child {
        BoxKind::TableCell => parent_kind == Some(BoxKind::TableRow),
        BoxKind::TableRow => matches!(
            parent_kind,
            Some(
                BoxKind::Table
                    | BoxKind::TableRowGroup
                    | BoxKind::TableHeaderGroup
                    | BoxKind::TableFooterGroup
            )
        ),
        BoxKind::TableColumn => matches!(
            parent_kind,
            Some(BoxKind::Table | BoxKind::TableColumnGroup)
        ),
        _ if is_table_part(child) => parent_kind == Some(BoxKind::Table),
        _ => true,
    }
}

/// Whether `kind` is a table part that only a table (or, for a row, a row
/// group) may hold directly.
fn is_table_part(kind: BoxKind) -> bool {
    matches!(
        kind,
        BoxKind::TableRow
            | BoxKind::TableRowGroup
            | BoxKind::TableHeaderGroup
            | BoxKind::TableFooterGroup
            | BoxKind::TableColumn
            | BoxKind::TableColumnGroup
            | BoxKind::TableCaption
    )
}

/// Collects every `BoxKind::Table` id, children before parents, using an
/// explicit stack so a deep tree cannot overflow the thread stack.
fn collect_tables(tree: &BoxTree, root: BoxId) -> Vec<BoxId> {
    let mut out = Vec::new();
    let mut stack: Vec<(BoxId, bool)> = vec![(root, false)];
    while let Some((id, processed)) = stack.pop() {
        if processed {
            if matches!(tree.get(id).map(|b| b.kind), Some(BoxKind::Table)) {
                out.push(id);
            }
            continue;
        }
        stack.push((id, true));
        let Some(box_) = tree.get(id) else { continue };
        // An `inline-table` is an atomic inline, so it lives in its parent's
        // inline content, not its block children (spec 0009, child 0004,
        // AC-14).
        for item in &box_.inline {
            match item {
                InlineItem::Atomic(id)
                | InlineItem::Absolute(id)
                | InlineItem::Float(id)
                | InlineItem::Start(id)
                | InlineItem::End(id) => stack.push((*id, false)),
                InlineItem::Text { .. } | InlineItem::Break(_) => {}
            }
        }
        for &child in box_.children.iter().rev() {
            stack.push((child, false));
        }
    }
    out
}

/// Fixes one table subtree: adds a table wrapper, then wraps stray cells and
/// content in rows and rows around stray content.
fn fix_table(tree: &mut BoxTree, table: BoxId) {
    let parent = tree.get(table).and_then(|b| b.parent);

    // `inline-table` makes the wrapper an atomic inline; `table` makes it a
    // block (spec 0009, child 0004, AC-14).
    let inline_table = tree
        .get(table)
        .is_some_and(|b| b.style.display_box.display == Display::InlineTable);

    // Wrap the table in a table wrapper unless it already has one. The
    // wrapper takes the table's position, float, margins and z-index.
    let table_parent_style = parent
        .and_then(|p| tree.boxes.get(p).map(|b| Rc::clone(&b.style)))
        .unwrap_or_else(ComputedStyle::initial);
    let wrapper = match parent {
        Some(p) if table_wrapper(tree, p) => p,
        _ => wrap_in_table_wrapper(tree, table, &table_parent_style, parent),
    };
    // If the table was the tree's root, the wrapper takes its place.
    if tree.root() == Some(table) {
        tree.set_root(wrapper);
    }
    if let Some(wrapper_box) = tree.boxes.get_mut(wrapper) {
        wrapper_box.inline_level = inline_table;
    }
    if let Some(table_box) = tree.boxes.get_mut(table) {
        table_box.inline_level = false;
    }

    // Restructure the table's direct children: anything that is not a row
    // group, row, column, column group or caption gets wrapped in an
    // anonymous row (cells without a row, stray text).
    restructure_table_children(tree, table, &table_parent_style);

    // Restructure each row's direct children: anything that is not a cell
    // gets wrapped in an anonymous cell.
    restructure_rows(tree, table);

    // The wrapper holds the captions too, above or below the table by
    // `caption-side` (spec 0009, child 0004, AC-14).
    place_captions(tree, table, wrapper);
}

/// Whether `id` is a table wrapper box.
fn table_wrapper(tree: &BoxTree, id: BoxId) -> bool {
    matches!(tree.get(id).map(|b| b.kind), Some(BoxKind::TableWrapper))
}

/// Inserts a table wrapper between `table` and its parent, reparenting the
/// table into the wrapper and the wrapper into the former parent's box.
///
/// The wrapper takes the table's margins, position, float and z-index, and
/// the table itself drops them, so the wrapper is what the outside world
/// places (spec 0009, child 0004, AC-14).
fn wrap_in_table_wrapper(
    tree: &mut BoxTree,
    table: BoxId,
    parent_style: &ComputedStyle,
    parent: Option<BoxId>,
) -> BoxId {
    let table_style = tree
        .get(table)
        .map(|b| Rc::clone(&b.style))
        .unwrap_or_else(ComputedStyle::initial);
    let inline_table = table_style.display_box.display == Display::InlineTable;
    let mut wrapper_style = ComputedStyle::inherit_from(parent_style);
    {
        let display_box = Rc::make_mut(&mut wrapper_style.display_box);
        // The table wrapper is a block box for `table` and an inline-block
        // for `inline-table` (CSS 2.2 section 17.4).
        display_box.display = if inline_table {
            Display::InlineBlock
        } else {
            Display::Block
        };
        display_box.position = table_style.display_box.position;
        display_box.float = table_style.display_box.float;
    }
    wrapper_style.margin = Rc::clone(&table_style.margin);
    wrapper_style.position_offsets = Rc::clone(&table_style.position_offsets);
    // The wrapper's width is the table's, resolved against the wrapper's own
    // containing block, so the caption and the table share one width and a
    // percentage applies once (CSS 2.2 section 17.4).
    Rc::make_mut(&mut wrapper_style.size).width = table_style.size.width.clone();
    wrapper_style.effects = Rc::new(css::Effects {
        z_index: table_style.effects.z_index,
        ..css::Effects::default()
    });
    let wrapper = tree.boxes.insert(LayoutBox::new(
        None,
        BoxKind::TableWrapper,
        Rc::new(wrapper_style),
        parent,
    ));
    // Replace the table with the wrapper wherever the parent referred to it:
    // in its block children or, for `inline-table`, its inline content.
    if let Some(parent) = parent
        && let Some(parent_box) = tree.boxes.get_mut(parent)
    {
        for child in &mut parent_box.children {
            if *child == table {
                *child = wrapper;
            }
        }
        for item in &mut parent_box.inline {
            let id = match item {
                InlineItem::Atomic(id) | InlineItem::Absolute(id) | InlineItem::Float(id) => id,
                _ => continue,
            };
            if *id == table {
                *id = wrapper;
            }
        }
    }
    // The wrapper holds the table.
    if let Some(wrapper_box) = tree.boxes.get_mut(wrapper) {
        wrapper_box.children = vec![table];
        wrapper_box.float = None;
    }
    // The table now belongs to the wrapper, and the outside world places the
    // wrapper instead: its margins, position, float and z-index move up.
    let float = tree.get(table).and_then(|b| b.float);
    if let Some(wrapper_box) = tree.boxes.get_mut(wrapper) {
        wrapper_box.float = float;
    }
    if let Some(table_box) = tree.boxes.get_mut(table) {
        table_box.parent = Some(wrapper);
        table_box.float = None;
        let style = Rc::make_mut(&mut table_box.style);
        style.margin = Rc::default();
        style.position_offsets = Rc::default();
        let display_box = Rc::make_mut(&mut style.display_box);
        display_box.position = css::Position::Static;
        display_box.float = css::Float::None;
        style.effects = Rc::new(css::Effects {
            z_index: None,
            ..css::Effects::default()
        });
    }
    wrapper
}

/// Moves the table's captions into its wrapper, above the table for
/// `caption-side: top` and below it for `bottom` (spec 0009, child 0004,
/// AC-14). Captions keep their document order within each side.
fn place_captions(tree: &mut BoxTree, table: BoxId, wrapper: BoxId) {
    // Captions may still sit in the table (a fresh tree) or already in the
    // wrapper (a fixup run twice).
    let mut captions: Vec<BoxId> = tree
        .get(table)
        .map(|b| {
            b.children
                .iter()
                .copied()
                .filter(|&child| is_caption(tree, child))
                .collect()
        })
        .unwrap_or_default();
    if let Some(wrapper_box) = tree.get(wrapper) {
        captions.extend(
            wrapper_box
                .children
                .iter()
                .copied()
                .filter(|&child| is_caption(tree, child)),
        );
    }
    let caption_ids = captions.clone();
    if let Some(table_box) = tree.boxes.get_mut(table) {
        table_box
            .children
            .retain(|child| !caption_ids.contains(child));
    }
    let mut top = Vec::new();
    let mut bottom = Vec::new();
    for caption in captions {
        let side = tree
            .get(caption)
            .map(|b| b.style.inherited_table.caption_side)
            .unwrap_or_default();
        match side {
            css::CaptionSide::Top => top.push(caption),
            css::CaptionSide::Bottom => bottom.push(caption),
        }
        if let Some(caption_box) = tree.boxes.get_mut(caption) {
            caption_box.parent = Some(wrapper);
        }
    }
    if let Some(wrapper_box) = tree.boxes.get_mut(wrapper) {
        let mut children = top;
        children.push(table);
        children.extend(bottom);
        wrapper_box.children = children;
    }
}

/// Whether `id` is a table caption box.
fn is_caption(tree: &BoxTree, id: BoxId) -> bool {
    matches!(tree.get(id).map(|b| b.kind), Some(BoxKind::TableCaption))
}

/// The style of an anonymous box: inherited from its parent container, with
/// `display` overridden.
fn anonymous_style(parent: &ComputedStyle, display: Display) -> Rc<ComputedStyle> {
    let mut style = ComputedStyle::inherit_from(parent);
    Rc::make_mut(&mut style.display_box).display = display;
    Rc::new(style)
}

/// Whether `kind` is a table part that may sit directly inside a table
/// without being wrapped.
fn keeps_place_in_table(kind: BoxKind) -> bool {
    matches!(
        kind,
        BoxKind::TableRowGroup
            | BoxKind::TableHeaderGroup
            | BoxKind::TableFooterGroup
            | BoxKind::TableRow
            | BoxKind::TableColumn
            | BoxKind::TableColumnGroup
            | BoxKind::TableCaption
    )
}

/// Whether `kind` is a row group, header or footer.
fn is_row_group(kind: BoxKind) -> bool {
    matches!(
        kind,
        BoxKind::TableRowGroup | BoxKind::TableHeaderGroup | BoxKind::TableFooterGroup
    )
}

/// Wraps each direct child of `table` that is not a row group, row, column,
/// column group or caption in an anonymous row, then wraps each run of
/// consecutive rows in an anonymous row group (CSS Tables 3 section 3).
///
/// A run of stray boxes shares one anonymous row, so cells written directly
/// in a table sit side by side instead of stacking (AC-14).
fn restructure_table_children(tree: &mut BoxTree, table: BoxId, parent_style: &ComputedStyle) {
    let parts = wrap_runs(
        tree,
        table,
        parent_style,
        keeps_place_in_table,
        BoxKind::TableRow,
        Display::TableRow,
    );

    let mut new_children = Vec::with_capacity(parts.len());
    let mut run: Vec<BoxId> = Vec::new();
    for child in parts {
        if matches!(tree.get(child).map(|b| b.kind), Some(BoxKind::TableRow)) {
            run.push(child);
            continue;
        }
        flush_rows(tree, table, parent_style, &mut run, &mut new_children);
        new_children.push(child);
    }
    flush_rows(tree, table, parent_style, &mut run, &mut new_children);
    if let Some(table_box) = tree.boxes.get_mut(table) {
        table_box.children = new_children.clone();
    }

    // A cell written directly in a row group (no row) gets an anonymous row
    // around it and its neighbours (CSS Tables 3 section 3).
    for child in new_children {
        if tree.get(child).is_some_and(|b| is_row_group(b.kind)) {
            let style = tree
                .get(child)
                .map(|b| Rc::clone(&b.style))
                .unwrap_or_else(ComputedStyle::initial);
            let rows = wrap_runs(
                tree,
                child,
                &style,
                |kind| kind == BoxKind::TableRow,
                BoxKind::TableRow,
                Display::TableRow,
            );
            if let Some(group_box) = tree.boxes.get_mut(child) {
                group_box.children = rows;
            }
        }
    }
}

/// Wraps each run of direct children of `parent` that `keeps` does not
/// accept in one anonymous box of `kind`, and returns the children in order.
/// Consecutive stray boxes share one anonymous box.
fn wrap_runs(
    tree: &mut BoxTree,
    parent: BoxId,
    parent_style: &ComputedStyle,
    keeps: impl Fn(BoxKind) -> bool,
    kind: BoxKind,
    display: Display,
) -> Vec<BoxId> {
    let children = tree
        .get(parent)
        .map(|b| b.children.clone())
        .unwrap_or_default();
    let mut out = Vec::with_capacity(children.len());
    let mut stray: Vec<BoxId> = Vec::new();
    for child in children {
        let child_kind = tree.get(child).map(|b| b.kind);
        if matches!(child_kind, Some(k) if keeps(k)) {
            flush_stray(
                tree,
                parent,
                parent_style,
                &mut stray,
                &mut out,
                kind,
                display,
            );
            out.push(child);
        } else {
            stray.push(child);
        }
    }
    flush_stray(
        tree,
        parent,
        parent_style,
        &mut stray,
        &mut out,
        kind,
        display,
    );
    out
}

/// Wraps the stray boxes collected so far in one anonymous box of `kind` and
/// adds it to `out`. Does nothing for an empty run.
fn flush_stray(
    tree: &mut BoxTree,
    parent: BoxId,
    parent_style: &ComputedStyle,
    stray: &mut Vec<BoxId>,
    out: &mut Vec<BoxId>,
    kind: BoxKind,
    display: Display,
) {
    if stray.is_empty() {
        return;
    }
    let wrapper = tree.boxes.insert(LayoutBox::new(
        None,
        kind,
        anonymous_style(parent_style, display),
        Some(parent),
    ));
    for &child in stray.iter() {
        if let Some(child_box) = tree.boxes.get_mut(child) {
            child_box.parent = Some(wrapper);
        }
    }
    if let Some(wrapper_box) = tree.boxes.get_mut(wrapper) {
        wrapper_box.children = std::mem::take(stray);
    }
    out.push(wrapper);
}

/// Moves a run of rows into one anonymous row group and adds the group to
/// `out`. Does nothing for an empty run.
fn flush_rows(
    tree: &mut BoxTree,
    table: BoxId,
    parent_style: &ComputedStyle,
    run: &mut Vec<BoxId>,
    out: &mut Vec<BoxId>,
) {
    if run.is_empty() {
        return;
    }
    let group = tree.boxes.insert(LayoutBox::new(
        None,
        BoxKind::TableRowGroup,
        anonymous_style(parent_style, Display::TableRowGroup),
        Some(table),
    ));
    for &row in run.iter() {
        if let Some(row_box) = tree.boxes.get_mut(row) {
            row_box.parent = Some(group);
        }
    }
    if let Some(group_box) = tree.boxes.get_mut(group) {
        group_box.children = std::mem::take(run);
    }
    out.push(group);
}

/// Walks `table`'s subtree and wraps each row's non-cell children in an
/// anonymous cell.
fn restructure_rows(tree: &mut BoxTree, table: BoxId) {
    let mut stack = vec![table];
    while let Some(id) = stack.pop() {
        let children = match tree.get(id) {
            Some(box_) => box_.children.clone(),
            None => continue,
        };
        for child in children {
            if tree
                .get(child)
                .is_some_and(|b| matches!(b.kind, BoxKind::TableRow))
            {
                let child_style = tree
                    .get(child)
                    .map(|b| Rc::clone(&b.style))
                    .unwrap_or_else(ComputedStyle::initial);
                restructure_row_children(tree, child, &child_style);
            }
            stack.push(child);
        }
    }
}

/// Wraps each run of direct children of `row` that is not a cell in one
/// anonymous cell (CSS Tables 3 section 3).
fn restructure_row_children(tree: &mut BoxTree, row: BoxId, parent_style: &ComputedStyle) {
    let children = wrap_runs(
        tree,
        row,
        parent_style,
        |kind| kind == BoxKind::TableCell,
        BoxKind::TableCell,
        Display::TableCell,
    );
    if let Some(row_box) = tree.boxes.get_mut(row) {
        row_box.children = children;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::box_tree::BoxTree;

    fn plain_box(kind: BoxKind) -> LayoutBox {
        LayoutBox::new(None, kind, ComputedStyle::initial(), None)
    }

    #[test]
    fn a_table_gets_a_table_wrapper_parent() {
        let mut tree = BoxTree::default();
        let table = tree.boxes.insert(plain_box(BoxKind::Table));
        tree.set_root(table);
        fix_tables(&mut tree);
        let root = tree.root().unwrap();
        assert_eq!(tree.get(root).unwrap().kind, BoxKind::TableWrapper);
        assert_eq!(
            tree.get(root).unwrap().children,
            vec![table],
            "the wrapper holds the table"
        );
        assert_eq!(tree.get(table).unwrap().parent, Some(root));
    }

    #[test]
    fn a_table_that_already_has_a_wrapper_is_not_wrapped_again() {
        let mut tree = BoxTree::default();
        let wrapper = tree.boxes.insert(plain_box(BoxKind::TableWrapper));
        let table = tree.boxes.insert(plain_box(BoxKind::Table));
        tree.boxes.get_mut(wrapper).unwrap().children = vec![table];
        tree.boxes.get_mut(table).unwrap().parent = Some(wrapper);
        tree.set_root(wrapper);
        fix_tables(&mut tree);
        assert_eq!(
            tree.iter()
                .filter(|b| b.kind == BoxKind::TableWrapper)
                .count(),
            1
        );
    }

    #[test]
    fn a_cell_directly_in_a_table_gets_an_anonymous_row() {
        let mut tree = BoxTree::default();
        let table = tree.boxes.insert(plain_box(BoxKind::Table));
        let cell = tree.boxes.insert(plain_box(BoxKind::TableCell));
        tree.boxes.get_mut(table).unwrap().children = vec![cell];
        tree.boxes.get_mut(cell).unwrap().parent = Some(table);
        tree.set_root(table);
        fix_tables(&mut tree);
        let root = tree.root().unwrap();
        assert_eq!(tree.get(root).unwrap().kind, BoxKind::TableWrapper);
        let table_box = tree.get(tree.get(root).unwrap().children[0]).unwrap();
        assert_eq!(table_box.kind, BoxKind::Table);
        assert_eq!(table_box.children.len(), 1, "one anonymous row group");
        let group = tree.get(table_box.children[0]).unwrap();
        assert_eq!(group.kind, BoxKind::TableRowGroup);
        assert_eq!(group.children.len(), 1, "one anonymous row");
        let row = group.children[0];
        let row_box = tree.get(row).unwrap();
        assert_eq!(row_box.kind, BoxKind::TableRow);
        assert_eq!(row_box.children, vec![cell]);
        assert_eq!(tree.get(cell).unwrap().parent, Some(row));
    }

    #[test]
    fn consecutive_rows_directly_in_a_table_get_one_row_group() {
        let mut tree = BoxTree::default();
        let table = tree.boxes.insert(plain_box(BoxKind::Table));
        let first = tree.boxes.insert(plain_box(BoxKind::TableRow));
        let second = tree.boxes.insert(plain_box(BoxKind::TableRow));
        tree.boxes.get_mut(table).unwrap().children = vec![first, second];
        tree.boxes.get_mut(first).unwrap().parent = Some(table);
        tree.boxes.get_mut(second).unwrap().parent = Some(table);
        tree.set_root(table);
        fix_tables(&mut tree);
        let root = tree.root().unwrap();
        let table_box = tree.get(tree.get(root).unwrap().children[0]).unwrap();
        assert_eq!(table_box.children.len(), 1, "one anonymous row group");
        let group = tree.get(table_box.children[0]).unwrap();
        assert_eq!(group.kind, BoxKind::TableRowGroup);
        assert_eq!(group.children, vec![first, second], "both rows in it");
        assert_eq!(tree.get(first).unwrap().parent, Some(table_box.children[0]));
        assert_eq!(
            tree.get(second).unwrap().parent,
            Some(table_box.children[0])
        );
    }

    #[test]
    fn columns_and_captions_are_not_wrapped() {
        let mut tree = BoxTree::default();
        let table = tree.boxes.insert(plain_box(BoxKind::Table));
        let colgroup = tree.boxes.insert(plain_box(BoxKind::TableColumnGroup));
        let caption = tree.boxes.insert(plain_box(BoxKind::TableCaption));
        tree.boxes.get_mut(table).unwrap().children = vec![colgroup, caption];
        tree.boxes.get_mut(colgroup).unwrap().parent = Some(table);
        tree.boxes.get_mut(caption).unwrap().parent = Some(table);
        tree.set_root(table);
        fix_tables(&mut tree);
        let root = tree.root().unwrap();
        // The wrapper holds the caption above the table; the table keeps its
        // columns (spec 0009, child 0004, AC-14).
        let wrapper_children = tree.get(root).unwrap().children.clone();
        assert_eq!(wrapper_children, vec![caption, table]);
        assert_eq!(tree.get(caption).unwrap().parent, Some(root));
        let table_children = tree.get(table).unwrap().children.clone();
        assert_eq!(table_children, vec![colgroup]);
    }

    #[test]
    fn caption_side_bottom_puts_the_caption_after_the_table() {
        let mut tree = BoxTree::default();
        let table = tree.boxes.insert(plain_box(BoxKind::Table));
        let mut caption = plain_box(BoxKind::TableCaption);
        let style = Rc::make_mut(&mut caption.style);
        Rc::make_mut(&mut style.inherited_table).caption_side = css::CaptionSide::Bottom;
        let caption = tree.boxes.insert(caption);
        tree.boxes.get_mut(table).unwrap().children = vec![caption];
        tree.boxes.get_mut(caption).unwrap().parent = Some(table);
        tree.set_root(table);
        fix_tables(&mut tree);
        let root = tree.root().unwrap();
        assert_eq!(tree.get(root).unwrap().children, vec![table, caption]);
    }

    #[test]
    fn inline_table_makes_an_atomic_inline_wrapper() {
        let mut tree = BoxTree::default();
        let parent = tree.boxes.insert(plain_box(BoxKind::BlockContainer));
        let mut table_box = plain_box(BoxKind::Table);
        let style = Rc::make_mut(&mut table_box.style);
        Rc::make_mut(&mut style.display_box).display = Display::InlineTable;
        table_box.inline_level = true;
        let table = tree.boxes.insert(table_box);
        tree.boxes.get_mut(table).unwrap().parent = Some(parent);
        tree.boxes.get_mut(parent).unwrap().inline = vec![InlineItem::Atomic(table)];
        tree.set_root(parent);
        fix_tables(&mut tree);
        let parent_box = tree.get(parent).unwrap();
        let wrapper = match parent_box.inline.as_slice() {
            [InlineItem::Atomic(wrapper)] => *wrapper,
            other => panic!("expected one atomic inline, got {other:?}"),
        };
        let wrapper_box = tree.get(wrapper).unwrap();
        assert_eq!(wrapper_box.kind, BoxKind::TableWrapper);
        assert!(wrapper_box.inline_level, "the wrapper sits on the line");
        assert_eq!(wrapper_box.children, vec![table]);
        assert!(
            !tree.get(table).unwrap().inline_level,
            "the table is a block"
        );
    }

    #[test]
    fn stray_content_in_a_row_gets_an_anonymous_cell() {
        let mut tree = BoxTree::default();
        let table = tree.boxes.insert(plain_box(BoxKind::Table));
        let row = tree.boxes.insert(plain_box(BoxKind::TableRow));
        let text_box = tree.boxes.insert(plain_box(BoxKind::BlockContainer));
        tree.boxes.get_mut(table).unwrap().children = vec![row];
        tree.boxes.get_mut(row).unwrap().children = vec![text_box];
        tree.boxes.get_mut(row).unwrap().parent = Some(table);
        tree.boxes.get_mut(text_box).unwrap().parent = Some(row);
        tree.set_root(table);
        fix_tables(&mut tree);
        let root = tree.root().unwrap();
        let table_box = tree.get(tree.get(root).unwrap().children[0]).unwrap();
        let group = tree.get(table_box.children[0]).unwrap();
        let row_box = tree.get(group.children[0]).unwrap();
        assert_eq!(row_box.kind, BoxKind::TableRow);
        assert_eq!(row_box.children.len(), 1, "one anonymous cell");
        let cell = row_box.children[0];
        let cell_box = tree.get(cell).unwrap();
        assert_eq!(cell_box.kind, BoxKind::TableCell);
        assert_eq!(cell_box.children, vec![text_box]);
        assert_eq!(tree.get(text_box).unwrap().parent, Some(cell));
    }

    #[test]
    fn consecutive_cells_directly_in_a_table_share_one_row() {
        // AC-14: cells written straight into a table sit side by side in one
        // anonymous row, not one row each.
        let mut tree = BoxTree::default();
        let table = tree.boxes.insert(plain_box(BoxKind::Table));
        let first = tree.boxes.insert(plain_box(BoxKind::TableCell));
        let second = tree.boxes.insert(plain_box(BoxKind::TableCell));
        tree.boxes.get_mut(table).unwrap().children = vec![first, second];
        tree.boxes.get_mut(first).unwrap().parent = Some(table);
        tree.boxes.get_mut(second).unwrap().parent = Some(table);
        tree.set_root(table);
        fix_tables(&mut tree);
        let root = tree.root().unwrap();
        let table_box = tree.get(tree.get(root).unwrap().children[0]).unwrap();
        let group = tree.get(table_box.children[0]).unwrap();
        assert_eq!(group.kind, BoxKind::TableRowGroup);
        let row = tree.get(group.children[0]).unwrap();
        assert_eq!(row.kind, BoxKind::TableRow);
        assert_eq!(row.children, vec![first, second], "both cells in one row");
    }

    #[test]
    fn a_header_group_keeps_its_place_in_the_table() {
        // AC-14: `<thead>` and `<tfoot>` are row groups, not stray content.
        let mut tree = BoxTree::default();
        let table = tree.boxes.insert(plain_box(BoxKind::Table));
        let head = tree.boxes.insert(plain_box(BoxKind::TableHeaderGroup));
        let foot = tree.boxes.insert(plain_box(BoxKind::TableFooterGroup));
        tree.boxes.get_mut(table).unwrap().children = vec![head, foot];
        tree.boxes.get_mut(head).unwrap().parent = Some(table);
        tree.boxes.get_mut(foot).unwrap().parent = Some(table);
        tree.set_root(table);
        fix_tables(&mut tree);
        let root = tree.root().unwrap();
        let table_box = tree.get(tree.get(root).unwrap().children[0]).unwrap();
        assert_eq!(table_box.children, vec![head, foot]);
    }

    #[test]
    fn a_cell_directly_in_a_row_group_gets_a_row() {
        // AC-14: a cell inside `<tbody>` with no `<tr>` gets an anonymous row.
        let mut tree = BoxTree::default();
        let table = tree.boxes.insert(plain_box(BoxKind::Table));
        let group = tree.boxes.insert(plain_box(BoxKind::TableRowGroup));
        let cell = tree.boxes.insert(plain_box(BoxKind::TableCell));
        tree.boxes.get_mut(table).unwrap().children = vec![group];
        tree.boxes.get_mut(group).unwrap().children = vec![cell];
        tree.boxes.get_mut(group).unwrap().parent = Some(table);
        tree.boxes.get_mut(cell).unwrap().parent = Some(group);
        tree.set_root(table);
        fix_tables(&mut tree);
        let group_box = tree.get(group).unwrap();
        assert_eq!(group_box.children.len(), 1);
        let row = tree.get(group_box.children[0]).unwrap();
        assert_eq!(row.kind, BoxKind::TableRow);
        assert_eq!(row.children, vec![cell]);
    }

    #[test]
    fn a_cell_in_a_block_gets_an_anonymous_row_and_table() {
        // AC-14: a `table-cell` in a block makes an anonymous table with one
        // row, so the cells sit side by side instead of stacking.
        let mut tree = BoxTree::default();
        let block = tree.boxes.insert(plain_box(BoxKind::BlockContainer));
        let first = tree.boxes.insert(plain_box(BoxKind::TableCell));
        let second = tree.boxes.insert(plain_box(BoxKind::TableCell));
        tree.boxes.get_mut(block).unwrap().children = vec![first, second];
        tree.boxes.get_mut(first).unwrap().parent = Some(block);
        tree.boxes.get_mut(second).unwrap().parent = Some(block);
        tree.set_root(block);
        fix_tables(&mut tree);
        let wrapper = tree.get(block).unwrap().children[0];
        let wrapper_box = tree.get(wrapper).unwrap();
        assert_eq!(wrapper_box.kind, BoxKind::TableWrapper);
        let table = wrapper_box.children[0];
        let table_box = tree.get(table).unwrap();
        assert_eq!(table_box.kind, BoxKind::Table);
        let group = tree.get(table_box.children[0]).unwrap();
        assert_eq!(group.kind, BoxKind::TableRowGroup);
        let row = tree.get(group.children[0]).unwrap();
        assert_eq!(row.kind, BoxKind::TableRow);
        assert_eq!(row.children, vec![first, second], "both cells in one row");
    }

    #[test]
    fn consecutive_rows_in_a_block_share_one_anonymous_table() {
        // AC-14: consecutive rows in a block get one anonymous table, not one
        // each.
        let mut tree = BoxTree::default();
        let block = tree.boxes.insert(plain_box(BoxKind::BlockContainer));
        let first = tree.boxes.insert(plain_box(BoxKind::TableRow));
        let second = tree.boxes.insert(plain_box(BoxKind::TableRow));
        tree.boxes.get_mut(block).unwrap().children = vec![first, second];
        tree.boxes.get_mut(first).unwrap().parent = Some(block);
        tree.boxes.get_mut(second).unwrap().parent = Some(block);
        tree.set_root(block);
        fix_tables(&mut tree);
        let wrapper = tree.get(block).unwrap().children[0];
        let table = tree.get(wrapper).unwrap().children[0];
        let table_box = tree.get(table).unwrap();
        assert_eq!(table_box.kind, BoxKind::Table);
        let group = tree.get(table_box.children[0]).unwrap();
        assert_eq!(group.kind, BoxKind::TableRowGroup);
        assert_eq!(group.children, vec![first, second], "both rows in it");
    }
}
