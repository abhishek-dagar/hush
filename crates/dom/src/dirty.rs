//! Walks over the dirty paths that mutations mark (spec 0002, AC-5).
//!
//! Both walks follow the flat tree and only descend where a
//! `DESCENDANT_NEEDS_*` flag says there is work below, so one dirty leaf
//! costs its ancestor path, not the whole tree. Both use a heap stack.

use crate::node::{Node, NodeFlags};
use crate::{Document, NodeId};

impl Document {
    /// The style pass's walk. Calls `visit` for every element on a dirty
    /// path, parents before children, with whether that element's own style
    /// must be recomputed (`false` for an ancestor that is only on the way).
    /// Clears the style flags it walks through.
    ///
    /// `visit` returns whether the element's style changed, which restyles
    /// all its children too: they inherit from it. It may set styles but
    /// must not change the tree.
    pub fn restyle_dirty(&mut self, mut visit: impl FnMut(&mut Self, NodeId, bool) -> bool) {
        self.clear_style_sharing();
        let dirty = NodeFlags::NEEDS_STYLE | NodeFlags::DESCENDANT_NEEDS_STYLE;
        let mut stack = vec![(self.root, false)];
        while let Some((id, forced)) = stack.pop() {
            let Some(node) = self.nodes.get_mut(id) else {
                continue;
            };
            let flags = node.flags;
            node.flags.remove(dirty | NodeFlags::RESTYLE_SUBTREE);
            let changed = id != self.root
                && visit(self, id, forced || flags.contains(NodeFlags::NEEDS_STYLE));

            let forced_below = forced || changed || flags.contains(NodeFlags::RESTYLE_SUBTREE);
            if !forced_below && !flags.contains(NodeFlags::DESCENDANT_NEEDS_STYLE) {
                continue;
            }
            let mut children: Vec<(NodeId, bool)> = self
                .flat_children(id)
                .filter(|&child| {
                    self.nodes.get(child).is_some_and(|n: &Node| {
                        n.as_element().is_some() && (forced_below || n.flags.intersects(dirty))
                    })
                })
                .map(|child| (child, forced_below))
                .collect();
            children.reverse();
            stack.extend(children);
        }
    }

    /// Takes every node whose children's boxes must be rebuilt, in flat tree
    /// order, and clears the box flags it walks through.
    pub fn take_box_rebuild_roots(&mut self) -> Vec<NodeId> {
        let dirty = NodeFlags::NEEDS_BOX_REBUILD | NodeFlags::DESCENDANT_NEEDS_BOX_REBUILD;
        let mut roots = Vec::new();
        let mut stack = vec![self.root];
        while let Some(id) = stack.pop() {
            let Some(node) = self.nodes.get_mut(id) else {
                continue;
            };
            let flags = node.flags;
            node.flags.remove(dirty);
            if flags.contains(NodeFlags::NEEDS_BOX_REBUILD) {
                roots.push(id);
            }
            if !flags.contains(NodeFlags::DESCENDANT_NEEDS_BOX_REBUILD) {
                continue;
            }
            let mut children: Vec<NodeId> = self
                .flat_children(id)
                .filter(|&child| {
                    self.nodes
                        .get(child)
                        .is_some_and(|n| n.flags.intersects(dirty))
                })
                .collect();
            children.reverse();
            stack.extend(children);
        }
        roots
    }
}
