//! AC-1: after any sequence of mutations every tree link is consistent, and
//! AC-2: a rejected mutation leaves the tree exactly as it was.
//!
//! A property test over random operation sequences. It uses a small seeded
//! generator rather than a property testing crate, so a failing seed can be
//! rerun as is.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use base::atoms::{LocalName, QualName};
use base::local_name;
use dom::{Document, NodeData, NodeId, ShadowMode};

const SEQUENCES: u64 = 300;
const STEPS: usize = 200;

/// xorshift64*: tiny, deterministic, good enough to pick operations.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick(&mut self, pool: &[NodeId]) -> Option<NodeId> {
        (!pool.is_empty()).then(|| pool[self.below(pool.len())])
    }
}

/// Every live node's links and connectedness, to compare before and after.
fn snapshot(doc: &Document) -> Vec<String> {
    doc.node_ids()
        .map(|id| {
            let n = doc.node(id).unwrap();
            format!(
                "{id:?} {:?} {:?} {:?} {:?} {:?} {}",
                n.parent(),
                n.first_child(),
                n.last_child(),
                n.prev_sibling(),
                n.next_sibling(),
                n.is_connected()
            )
        })
        .collect()
}

fn check_links(doc: &Document, seed: u64, step: usize) {
    let at = format!("seed {seed}, step {step}");
    for id in doc.node_ids() {
        let node = doc.node(id).unwrap();

        // The parent lists this node as a child.
        if let Some(parent) = node.parent() {
            assert!(doc.is_alive(parent), "{at}: {id:?} has a freed parent");
            assert!(
                doc.children(parent).any(|c| c == id),
                "{at}: {parent:?} does not list its child {id:?}"
            );
        }

        // The sibling chain agrees both ways and its ends match first and last.
        let mut prev = None;
        let mut current = node.first_child();
        while let Some(child) = current {
            let child_node = doc.node(child).unwrap();
            assert_eq!(
                child_node.parent(),
                Some(id),
                "{at}: {child:?} wrong parent"
            );
            assert_eq!(
                child_node.prev_sibling(),
                prev,
                "{at}: {child:?} wrong prev"
            );
            prev = Some(child);
            current = child_node.next_sibling();
        }
        assert_eq!(node.last_child(), prev, "{at}: {id:?} wrong last child");
        if node.parent().is_none() {
            assert_eq!(node.prev_sibling(), None, "{at}: a root with siblings");
            assert_eq!(node.next_sibling(), None, "{at}: a root with siblings");
        }

        // Connected exactly when the shadow including root is the document.
        let mut top = id;
        loop {
            let n = doc.node(top).unwrap();
            match (n.parent(), n.data()) {
                (Some(parent), _) => top = parent,
                (None, NodeData::ShadowRoot { host, .. }) => top = *host,
                _ => break,
            }
        }
        assert_eq!(
            node.is_connected(),
            top == doc.root(),
            "{at}: {id:?} connectedness is wrong"
        );
    }
}

fn run(seed: u64) {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let mut doc = Document::new("about:blank");
    let html = doc.create_element(QualName::html(local_name!("html")));
    doc.append_child(doc.root(), html).unwrap();
    let mut pool: Vec<NodeId> = vec![doc.root(), html];
    let mut pinned: Vec<NodeId> = Vec::new();
    let names = [
        local_name!("div"),
        local_name!("p"),
        local_name!("span"),
        local_name!("template"),
        local_name!("iframe"),
    ];

    for step in 0..STEPS {
        pool.retain(|&id| doc.is_alive(id));
        pinned.retain(|&id| doc.is_alive(id));
        let before = snapshot(&doc);
        let (Some(a), Some(b)) = (rng.pick(&pool), rng.pick(&pool)) else {
            continue;
        };
        let reference = rng.pick(&pool).filter(|_| rng.below(2) == 0);

        let result = match rng.below(12) {
            0 | 1 => {
                let name: LocalName = names[rng.below(names.len())].clone();
                pool.push(doc.create_element(QualName::html(name)));
                Ok(())
            }
            2 => {
                pool.push(doc.create_text("t"));
                Ok(())
            }
            3 => {
                pool.push(doc.create_document_fragment());
                Ok(())
            }
            4 | 5 => doc.append_child(a, b),
            6 => doc.insert_before(a, b, reference),
            7 => doc.remove_child(a, b),
            8 => doc.replace_child(a, b, reference.unwrap_or(b)),
            9 => doc
                .attach_shadow(a, ShadowMode::Open)
                .map(|shadow| pool.push(shadow)),
            10 => {
                let pin = doc.pin(a);
                pinned.push(a);
                pin
            }
            _ => {
                if let Some(id) = pinned.pop() {
                    doc.unpin(id).unwrap();
                }
                // The sweep never frees a connected node or a pinned one.
                let kept: Vec<NodeId> = doc
                    .node_ids()
                    .filter(|&id| doc.is_connected(id) || doc.node(id).unwrap().pin_count() > 0)
                    .collect();
                doc.end_task();
                assert!(
                    kept.iter().all(|&id| doc.is_alive(id)),
                    "seed {seed}, step {step}: the sweep freed a live node"
                );
                Ok(())
            }
        };
        if result.is_err() {
            assert_eq!(
                snapshot(&doc),
                before,
                "seed {seed}, step {step}: a failed call changed the tree"
            );
        }
        check_links(&doc, seed, step);
    }
}

#[test]
fn random_mutation_sequences_keep_every_link_consistent() {
    for seed in 0..SEQUENCES {
        run(seed);
    }
}
