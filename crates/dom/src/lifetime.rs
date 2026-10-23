//! Pinning and the end of task sweep (spec 0002, Pinning and sweep, AC-4).
//!
//! A node is alive while it is connected, owned by a live node (template
//! content, a shadow root), or in a detached subtree that holds a pin. Script
//! wrappers and any engine structure that keeps a `NodeId` across tasks pin
//! it. Tree links, `shadow_root` and template `content` are owner edges and
//! need no pin.

use std::panic::Location;

use crate::node::{ElementKind, NodeData};
use crate::{Document, DomError, NodeId};

impl Document {
    /// Keeps `id` and the detached subtree it ends up in alive across sweeps.
    #[track_caller]
    pub fn pin(&mut self, id: NodeId) -> Result<(), DomError> {
        let node = self.live_mut(id, Location::caller())?;
        node.pin_count = node.pin_count.saturating_add(1);
        Ok(())
    }

    /// Drops one pin. When the last pin goes from a disconnected node, its
    /// tree becomes a sweep candidate again.
    #[track_caller]
    pub fn unpin(&mut self, id: NodeId) -> Result<(), DomError> {
        let node = self.live_mut(id, Location::caller())?;
        node.pin_count = node
            .pin_count
            .checked_sub(1)
            .ok_or(DomError::InvalidState)?;
        if node.pin_count == 0 && !node.is_connected() {
            let root = self.host_including_root(id);
            self.detached_roots.push(root);
        }
        Ok(())
    }

    /// Ends a task: clears the mutation queue (nothing reads it until
    /// observers exist), then sweeps. Runs after the microtask checkpoint.
    /// The page drains [`Document::take_frame_changes`] first.
    pub fn end_task(&mut self) -> usize {
        self.mutation_queue.clear();
        self.sweep()
    }

    /// Frees every detached subtree that holds no pin, template content and
    /// shadow trees included, and returns how many nodes it freed. Frees nodes
    /// only: boxes go away through box rebuild.
    pub fn sweep(&mut self) -> usize {
        let mut freed = 0;
        for root in std::mem::take(&mut self.detached_roots) {
            // Already freed, or listed twice.
            let Some(node) = self.nodes.get(root) else {
                continue;
            };
            let owned = matches!(
                node.data,
                NodeData::ShadowRoot { .. } | NodeData::DocumentFragment { host: Some(_) }
            );
            if root == self.root || node.parent.is_some() || node.is_connected() || owned {
                continue;
            }
            let Some(subtree) = self.unpinned_subtree(root) else {
                // Pinned: a later unpin lists it again.
                continue;
            };
            for id in subtree {
                self.nodes.remove(id);
                freed += 1;
            }
        }
        freed
    }

    /// Every node owned by `root`, or `None` as soon as one has a pin.
    fn unpinned_subtree(&self, root: NodeId) -> Option<Vec<NodeId>> {
        let mut subtree = Vec::new();
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            let node = self.nodes.get(id)?;
            if node.pin_count > 0 {
                return None;
            }
            subtree.push(id);
            stack.extend(self.children(id));
            if let Some(element) = node.as_element() {
                stack.extend(element.shadow_root);
                if let ElementKind::Template { content } = element.kind {
                    stack.push(content);
                }
            }
        }
        Some(subtree)
    }
}

#[cfg(test)]
impl Document {
    /// How many sweep candidates are waiting.
    pub(crate) fn sweep_candidates(&self) -> usize {
        self.detached_roots
            .iter()
            .filter(|&&id| self.nodes.get(id).is_some_and(|n| n.parent.is_none()))
            .count()
    }
}
