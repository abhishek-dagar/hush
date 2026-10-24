//! Anonymous box fixup for tables (CSS Tables 3 section 3).
//!
//! After the box tree is built, every table gets a table wrapper parent,
//! stray cells and content directly inside a table get wrapped in an
//! anonymous row, and stray content directly inside a row gets wrapped in an
//! anonymous cell. Whitespace-only text inside table parts is already dropped
//! by the box tree builder, so here we only restructure boxes.

use base::BoxId;
use css::{ComputedStyle, Display};
use std::rc::Rc;

use crate::box_tree::{BoxKind, BoxTree, LayoutBox};

/// Runs the table anonymous-box fixup on `tree`.
pub(crate) fn fix_tables(tree: &mut BoxTree) {
    let root = match tree.root() {
        Some(root) => root,
        None => return,
    };
    let tables = collect_tables(tree, root);
    for table in tables {
        fix_table(tree, table);
    }
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

    // Restructure the table's direct children: anything that is not a row
    // group, row, column, column group or caption gets wrapped in an
    // anonymous row (cells without a row, stray text).
    restructure_table_children(tree, table, &table_parent_style);

    // Restructure each row's direct children: anything that is not a cell
    // gets wrapped in an anonymous cell.
    restructure_rows(tree, table);
}

/// Whether `id` is a table wrapper box.
fn table_wrapper(tree: &BoxTree, id: BoxId) -> bool {
    matches!(tree.get(id).map(|b| b.kind), Some(BoxKind::TableWrapper))
}

/// Inserts a table wrapper between `table` and its parent, reparenting the
/// table into the wrapper and the wrapper into the former parent's children.
fn wrap_in_table_wrapper(
    tree: &mut BoxTree,
    table: BoxId,
    parent_style: &ComputedStyle,
    parent: Option<BoxId>,
) -> BoxId {
    let wrapper_style = anonymous_style(parent_style, Display::Table);
    let wrapper = tree.boxes.insert(LayoutBox::new(
        None,
        BoxKind::TableWrapper,
        wrapper_style,
        parent,
    ));
    // Replace the table with the wrapper in the former parent's children.
    if let Some(parent) = parent
        && let Some(parent_box) = tree.boxes.get_mut(parent)
        && let Some(pos) = parent_box.children.iter().position(|&c| c == table)
    {
        parent_box.children[pos] = wrapper;
    }
    // The wrapper holds the table.
    if let Some(wrapper_box) = tree.boxes.get_mut(wrapper) {
        wrapper_box.children = vec![table];
    }
    // The table now belongs to the wrapper.
    if let Some(table_box) = tree.boxes.get_mut(table) {
        table_box.parent = Some(wrapper);
    }
    wrapper
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
            | BoxKind::TableRow
            | BoxKind::TableColumn
            | BoxKind::TableColumnGroup
            | BoxKind::TableCaption
    )
}

/// Wraps each direct child of `table` that is not a row group, row, column,
/// column group or caption in an anonymous row.
fn restructure_table_children(tree: &mut BoxTree, table: BoxId, parent_style: &ComputedStyle) {
    let children = tree
        .get(table)
        .map(|b| b.children.clone())
        .unwrap_or_default();
    let mut new_children = Vec::with_capacity(children.len());
    for child in children {
        let kind = tree.get(child).map(|b| b.kind);
        if matches!(kind, Some(k) if keeps_place_in_table(k)) {
            new_children.push(child);
            continue;
        }
        let row = new_anonymous_row(tree, parent_style);
        reparent_child(tree, table, child, row);
        new_children.push(row);
    }
    if let Some(table_box) = tree.boxes.get_mut(table) {
        table_box.children = new_children;
    }
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

/// Wraps each direct child of `row` that is not a cell in an anonymous cell.
fn restructure_row_children(tree: &mut BoxTree, row: BoxId, parent_style: &ComputedStyle) {
    let children = tree
        .get(row)
        .map(|b| b.children.clone())
        .unwrap_or_default();
    let mut new_children = Vec::with_capacity(children.len());
    for child in children {
        if matches!(tree.get(child).map(|b| b.kind), Some(BoxKind::TableCell)) {
            new_children.push(child);
            continue;
        }
        let cell = new_anonymous_cell(tree, parent_style);
        reparent_child(tree, row, child, cell);
        new_children.push(cell);
    }
    if let Some(row_box) = tree.boxes.get_mut(row) {
        row_box.children = new_children;
    }
}

/// Creates an anonymous table row box whose style is inherited from `parent`.
fn new_anonymous_row(tree: &mut BoxTree, parent: &ComputedStyle) -> BoxId {
    tree.boxes.insert(LayoutBox::new(
        None,
        BoxKind::TableRow,
        anonymous_style(parent, Display::TableRow),
        None,
    ))
}

/// Creates an anonymous table cell box whose style is inherited from `parent`.
fn new_anonymous_cell(tree: &mut BoxTree, parent: &ComputedStyle) -> BoxId {
    tree.boxes.insert(LayoutBox::new(
        None,
        BoxKind::TableCell,
        anonymous_style(parent, Display::TableCell),
        None,
    ))
}

/// Moves `child` out of `parent`'s children and into `wrapper`.
fn reparent_child(tree: &mut BoxTree, parent: BoxId, child: BoxId, wrapper: BoxId) {
    if let Some(parent_box) = tree.boxes.get_mut(parent)
        && let Some(pos) = parent_box.children.iter().position(|&c| c == child)
    {
        parent_box.children[pos] = wrapper;
    }
    if let Some(wrapper_box) = tree.boxes.get_mut(wrapper) {
        wrapper_box.children = vec![child];
        wrapper_box.parent = Some(parent);
    }
    if let Some(child_box) = tree.boxes.get_mut(child) {
        child_box.parent = Some(wrapper);
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
        assert_eq!(table_box.children.len(), 1, "one anonymous row");
        let row = table_box.children[0];
        let row_box = tree.get(row).unwrap();
        assert_eq!(row_box.kind, BoxKind::TableRow);
        assert_eq!(row_box.children, vec![cell]);
        assert_eq!(tree.get(cell).unwrap().parent, Some(row));
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
        let wrapper_children = tree.get(root).unwrap().children.clone();
        assert_eq!(wrapper_children, vec![table]);
        let table_children = tree.get(table).unwrap().children.clone();
        assert_eq!(table_children, vec![colgroup, caption]);
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
        let row_box = tree
            .get(
                tree.get(tree.get(root).unwrap().children[0])
                    .unwrap()
                    .children[0],
            )
            .unwrap();
        assert_eq!(row_box.kind, BoxKind::TableRow);
        assert_eq!(row_box.children.len(), 1, "one anonymous cell");
        let cell = row_box.children[0];
        let cell_box = tree.get(cell).unwrap();
        assert_eq!(cell_box.kind, BoxKind::TableCell);
        assert_eq!(cell_box.children, vec![text_box]);
        assert_eq!(tree.get(text_box).unwrap().parent, Some(cell));
    }
}
