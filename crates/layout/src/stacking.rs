//! Paint order: CSS 2.2 Appendix E for what the engine supports (spec
//! 0006, AC-6), computed once here so paint and hit testing agree.
//!
//! A stacking context paints, in order: its root box's background and
//! border; its children with a negative `z-index`, lowest first; the
//! backgrounds and borders of its in flow blocks; its inline content
//! (inline boxes, text, and atomic inlines and flex items, each painted
//! whole); its positioned descendants with `z-index: auto` or `0` and its
//! other `z-index: 0` contexts, in tree order; its children with a positive
//! `z-index`, lowest first; and last every outline in it (AC-8).
//!
//! A box makes a stacking context when it is the root, when it has an
//! `opacity` below 1, or when it has a `z-index` and is positioned or a
//! flex item. A positioned box without one, an atomic inline and a flex
//! item paint as if they made one, but their descendants that make real
//! ones belong to the enclosing context.
//!
//! The walk uses explicit stacks, so a very deep page cannot overflow the
//! thread stack.

use std::rc::Rc;

use base::{Au, Point};

use crate::fragment::{BoxFragment, Fragment, TextFragment};
use crate::{BoxKind, BoxTree};

/// A fragment with where its border box starts and the walker's state for
/// it (for paint, its space and clip; for hit testing, its clip).
#[derive(Debug, Clone)]
pub struct Placed<'a, S> {
    /// Its border box's top left corner.
    pub origin: Point,
    /// The fragment.
    pub fragment: &'a Rc<Fragment>,
    /// What the walker tracks for it.
    pub state: S,
}

/// One step of paint order.
#[derive(Debug, Clone)]
pub enum Step<'a, S> {
    /// A stacking context starts, with its root box. Its steps follow,
    /// up to the matching [`Step::Leave`].
    Enter {
        /// The context's root box.
        fragment: &'a BoxFragment,
        /// Where its border box starts.
        origin: Point,
        /// The walker's state for it.
        state: S,
    },
    /// The innermost open stacking context ends.
    Leave,
    /// A box's own background, border and picture.
    Box {
        /// The box.
        fragment: &'a BoxFragment,
        /// Where its border box starts.
        origin: Point,
        /// The walker's state for it.
        state: S,
    },
    /// A run of text.
    Text {
        /// The run.
        fragment: &'a TextFragment,
        /// Where its top left corner is.
        origin: Point,
        /// The walker's state for it.
        state: S,
    },
    /// A box's outline: only boxes with an outline wider than zero get one.
    Outline {
        /// The box.
        fragment: &'a BoxFragment,
        /// Where its border box starts.
        origin: Point,
        /// The walker's state for it.
        state: S,
    },
}

/// What a walk asks of its walker for each box: where the box's children
/// sit and the state they get.
pub trait Walker<'a> {
    /// The state carried for each fragment.
    type State: Clone;

    /// The origin `fragment`'s children are placed from (its border box
    /// corner, unless it scrolls them) and their state, given the box at
    /// `origin` with `state`. `None` skips them: nothing in them can
    /// matter, as for a hit test outside the box's clip.
    fn children(
        &mut self,
        fragment: &'a BoxFragment,
        origin: Point,
        state: &Self::State,
    ) -> Option<(Point, Self::State)>;
}

/// The paint order of everything below and including `root`, the root of
/// a stacking context. Hit testing reads it backwards: the last box that
/// contains a point is the one on top.
pub fn paint_order<'a, W: Walker<'a>>(
    boxes: &BoxTree,
    root: Placed<'a, W::State>,
    walker: &mut W,
) -> Vec<Step<'a, W::State>> {
    let layers = gather(boxes, root, walker);
    flatten(layers)
}

/// What a box does to the stacking of its content.
enum Stacking {
    /// Nothing: it paints in its parent's layers.
    Normal,
    /// It makes a stacking context at this `z-index`.
    Context(i32),
    /// It is positioned without a `z-index`: it paints as if it made a
    /// context, among the positioned descendants.
    Positioned,
    /// An atomic inline or a flex item: it paints as if it made a context,
    /// among the inline content.
    Atomic,
    /// A float: paints as its own layer after block backgrounds and before
    /// inline content (CSS 2.2 Appendix E, step 5; spec 0009, child 0001).
    Float,
}

fn stacking(boxes: &BoxTree, fragment: &BoxFragment, in_line: bool) -> Stacking {
    let Some(layout_box) = boxes.get(fragment.box_id) else {
        return Stacking::Normal;
    };
    let style = &layout_box.style;
    let positioned = style.display_box.is_positioned();
    let flex_item = !layout_box.is_absolute()
        && layout_box
            .parent
            .and_then(|parent| boxes.get(parent))
            .is_some_and(|parent| parent.kind == BoxKind::FlexContainer);
    // `z-index` applies to positioned boxes and flex items only.
    let z_index = style.effects.z_index.filter(|_| positioned || flex_item);
    if z_index.is_some() || style.effects.opacity < 1.0 {
        Stacking::Context(z_index.unwrap_or(0))
    } else if positioned {
        Stacking::Positioned
    } else if flex_item || (in_line && fragment.inline_edges.is_none()) {
        Stacking::Atomic
    } else if layout_box.float.is_some() {
        Stacking::Float
    } else {
        Stacking::Normal
    }
}

/// One entry in a layer: a step, or a whole group of layers painted in
/// its place.
enum Item<'a, S> {
    Step(Step<'a, S>),
    Group(usize),
}

/// The layers of one stacking context, or of a box that paints as if it
/// made one. The `z-index` lists and outlines are only used on real
/// contexts; the others hand theirs to the context around them.
struct Layers<'a, S> {
    /// For a real context, its `Enter` step.
    enter: Option<Step<'a, S>>,
    own: Vec<Item<'a, S>>,
    negative: Vec<(i32, usize)>,
    blocks: Vec<Item<'a, S>>,
    /// Floats: paint after block backgrounds and before inline content,
    /// each whole as if it made a stacking context (Appendix E, step 5).
    floats: Vec<Item<'a, S>>,
    inline: Vec<Item<'a, S>>,
    positioned: Vec<Item<'a, S>>,
    positive: Vec<(i32, usize)>,
    outlines: Vec<Item<'a, S>>,
}

impl<S> Layers<'_, S> {
    fn new() -> Self {
        Self {
            enter: None,
            own: Vec::new(),
            negative: Vec::new(),
            blocks: Vec::new(),
            floats: Vec::new(),
            inline: Vec::new(),
            positioned: Vec::new(),
            positive: Vec::new(),
            outlines: Vec::new(),
        }
    }
}

/// A fragment to sort into layers.
struct Work<'a, S> {
    placed: Placed<'a, S>,
    /// It is the root of the group `layers`, so it paints in its own
    /// layer and is not sorted again.
    root: bool,
    /// It sits in a line.
    in_line: bool,
    /// The group its steps go to.
    layers: usize,
    /// The real stacking context it is in; its children walk in this context.
    context: usize,
    /// The context whose negative/positive/positioned lists descendants
    /// register into: the enclosing context for a float's sub-group, else
    /// `context` itself (spec 0009, child 0001).
    parent_context: usize,
}

/// Sorts every fragment into the layers of its group, depth first in tree
/// order. Group 0 is the root's context.
fn gather<'a, W: Walker<'a>>(
    boxes: &BoxTree,
    root: Placed<'a, W::State>,
    walker: &mut W,
) -> Vec<Layers<'a, W::State>> {
    let mut groups = vec![Layers::new()];
    let mut work = vec![Work {
        placed: root,
        root: true,
        in_line: false,
        layers: 0,
        context: 0,
        parent_context: 0,
    }];
    while let Some(Work {
        placed,
        root,
        in_line,
        layers,
        context,
        parent_context,
    }) = work.pop()
    {
        let Placed {
            origin,
            fragment,
            state,
        } = placed;
        let fragment_box = match &**fragment {
            Fragment::Box(fragment_box) => fragment_box,
            Fragment::Line(line) => {
                work.extend(line.children.iter().rev().map(|(at, child)| Work {
                    placed: Placed {
                        origin: origin.offset_by(*at),
                        fragment: child,
                        state: state.clone(),
                    },
                    root: false,
                    in_line: true,
                    layers,
                    context,
                    parent_context,
                }));
                continue;
            }
            Fragment::Text(text) => {
                groups[layers].inline.push(Item::Step(Step::Text {
                    fragment: text,
                    origin,
                    state,
                }));
                continue;
            }
        };
        if root && layers == context && groups[layers].enter.is_none() {
            groups[layers].enter = Some(Step::Enter {
                fragment: fragment_box,
                origin,
                state: state.clone(),
            });
        }
        let kind = if !root {
            stacking(boxes, fragment_box, in_line)
        } else {
            Stacking::Normal
        };
        match kind {
            Stacking::Float => {
                // A float paints as its own layer after block backgrounds and
                // before inline content (Appendix E, step 5). Its sub-group
                // paints the float box plus its content as a unit; positioned
                // descendants and real stacking contexts register with the
                // enclosing context (parent_context).
                let group = groups.len();
                groups.push(Layers::new());
                groups[layers].floats.push(Item::Group(group));
                work.push(Work {
                    placed: Placed {
                        origin,
                        fragment,
                        state,
                    },
                    root: true,
                    in_line,
                    layers: group,
                    context: group,
                    parent_context,
                });
                continue;
            }
            _ if !matches!(kind, Stacking::Normal) => {
                let group = groups.len();
                groups.push(Layers::new());
                let new_context = match kind {
                    Stacking::Context(z) if z < 0 => {
                        groups[parent_context].negative.push((z, group));
                        group
                    }
                    Stacking::Context(z) if z > 0 => {
                        groups[parent_context].positive.push((z, group));
                        group
                    }
                    Stacking::Context(_) => {
                        groups[parent_context].positioned.push(Item::Group(group));
                        group
                    }
                    Stacking::Positioned => {
                        groups[parent_context].positioned.push(Item::Group(group));
                        context
                    }
                    Stacking::Atomic | Stacking::Normal => {
                        groups[layers].inline.push(Item::Group(group));
                        context
                    }
                    Stacking::Float => unreachable!(),
                };
                work.push(Work {
                    placed: Placed {
                        origin,
                        fragment,
                        state,
                    },
                    root: true,
                    in_line,
                    layers: group,
                    context: new_context,
                    parent_context,
                });
                continue;
            }
            _ => {}
        }

        let is_normal = matches!(kind, Stacking::Normal);
        if is_normal {
            let own = Step::Box {
                fragment: fragment_box,
                origin,
                state: state.clone(),
            };
            let group = &mut groups[layers];
            if root {
                group.own.push(Item::Step(own));
            } else if fragment_box.inline_edges.is_some() {
                group.inline.push(Item::Step(own));
            } else {
                group.blocks.push(Item::Step(own));
            }
        }
        // A table's collapsed borders paint in the outline pass too, after
        // its cells (spec 0009, child 0004, AC-17).
        let outlined = fragment_box.collapsed_borders.is_some()
            || boxes
                .get(fragment_box.box_id)
                .is_some_and(|b| b.style.outline.width > Au::ZERO);
        if outlined {
            groups[context].outlines.push(Item::Step(Step::Outline {
                fragment: fragment_box,
                origin,
                state: state.clone(),
            }));
        }
        let Some((children_origin, children_state)) = walker.children(fragment_box, origin, &state)
        else {
            continue;
        };
        let children_in_line = fragment_box.inline_edges.is_some();
        work.extend(fragment_box.children.iter().rev().map(|(at, child)| Work {
            placed: Placed {
                origin: children_origin.offset_by(*at),
                fragment: child,
                state: children_state.clone(),
            },
            root: false,
            in_line: children_in_line,
            layers,
            context,
            parent_context,
        }));
    }
    groups
}

/// The groups' steps in paint order, from group 0.
fn flatten<'a, S>(mut groups: Vec<Layers<'a, S>>) -> Vec<Step<'a, S>> {
    enum Emit<'a, S> {
        Group(usize),
        Step(Step<'a, S>),
    }
    let mut out = Vec::new();
    let mut pending = vec![Emit::Group(0)];
    while let Some(next) = pending.pop() {
        let index = match next {
            Emit::Step(step) => {
                out.push(step);
                continue;
            }
            Emit::Group(index) => index,
        };
        let Some(group) = groups.get_mut(index) else {
            continue;
        };
        let group = std::mem::replace(group, Layers::new());
        let item = |item: Item<'a, S>| -> Emit<'a, S> {
            match item {
                Item::Step(step) => Emit::Step(step),
                Item::Group(index) => Emit::Group(index),
            }
        };
        // Sorting is stable, so equal `z-index` values keep tree order.
        let by_z = |mut list: Vec<(i32, usize)>| {
            list.sort_by_key(|&(z, _)| z);
            list.into_iter().map(|(_, index)| Emit::Group(index))
        };
        let is_context = group.enter.is_some();
        let mut sequence: Vec<Emit<'_, S>> = Vec::new();
        sequence.extend(group.enter.map(Emit::Step));
        sequence.extend(group.own.into_iter().map(item));
        sequence.extend(by_z(group.negative));
        sequence.extend(group.blocks.into_iter().map(item));
        sequence.extend(group.outlines.into_iter().map(item));
        sequence.extend(group.floats.into_iter().map(item));
        sequence.extend(group.inline.into_iter().map(item));
        sequence.extend(group.positioned.into_iter().map(item));
        sequence.extend(by_z(group.positive));
        if is_context {
            sequence.push(Emit::Step(Step::Leave));
        }
        pending.extend(sequence.into_iter().rev());
    }
    out
}
