//! The render side's scroll offsets and the "latest intent wins" rule
//! (spec 0002, Scrolling, AC-13).

use std::collections::HashMap;

use crate::{DisplayList, PointF, RectF, ScrollId, ScrollOffset, SizeF, SpatialId, SpatialKind};

/// How far a sticky box shifts to stay inside its scrollport while its
/// nearest scroll frame scrolls (spec 0009, child 0003, AC-11).
///
/// `sticky_rect` is where the box was laid out, in the scroll frame's
/// content space. `insets` is the box's sticky insets resolved against the
/// scrollport size (`top`, `right`, `bottom`, `left`). `limits` is how far
/// the box may shift on each axis, min then max, from its containing
/// block's content box minus the box's margin box, in content space.
/// Returns the shift to add to the box's laid out position.
///
/// The box never moves up from its flow position. `top` pulls it down to
/// stay `top` px below the scrollport's top edge once scrolling passes it;
/// `bottom` caps how far down via `max_y`. With no `top` inset the box
/// stays put (behaves like `relative`). Paint calls this once per frame to
/// apply the shift at full frame rate (spec 0009, child 0003, AC-11).
#[allow(
    dead_code,
    reason = "paint builds sticky nodes and calls this; wired in the next slice"
)]
pub fn sticky_offset(
    sticky_rect: RectF,
    insets: [Option<f32>; 4],
    limits: ((f32, f32), (f32, f32)),
    frame_offset: PointF,
) -> PointF {
    let [top, _right, _bottom, left] = insets;
    let ((min_y, max_y), (min_x, max_x)) = limits;
    // Scrollport top edge in content space is at frame_offset.y; the box
    // wants its top edge `top` px below it.
    // The box never moves up from its flow position. `top` pulls it down to
    // stay `top` px below the scrollport's top edge once scrolling passes it;
    // `bottom` caps how far down via `max_y`. With no `top` inset the box
    // stays put (behaves like `relative`).
    let y = top
        .map(|t| (frame_offset.y - sticky_rect.origin.y + t).clamp(min_y, max_y))
        .unwrap_or(0.0);
    let x = left
        .map(|l| (frame_offset.x - sticky_rect.origin.x + l).clamp(min_x, max_x))
        .unwrap_or(0.0);
    PointF { x, y }
}

/// What the render side last applied to one scroll frame.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Applied {
    x: f32,
    y: f32,
    /// The newest script scroll generation applied so far.
    generation: u32,
}

/// The render side's scroll offsets for one page, by [`ScrollId`].
///
/// User scrolling moves an offset here without asking the page, so it stays
/// smooth while the page is busy. A new list's offset replaces it only when
/// the list carries a newer script scroll generation.
#[derive(Debug, Default)]
pub struct ScrollState {
    frames: HashMap<ScrollId, Applied>,
}

impl ScrollState {
    /// Settles every scroll frame in a newly received `list` and writes the
    /// result back into it, so drawing it uses the offsets the user sees.
    ///
    /// A frame takes the list's offset when it is new, or when the list's
    /// generation is newer than the last one applied (a script scrolled it).
    /// Otherwise it keeps its own offset. Offsets are kept inside the
    /// scroll range, and frames that left the list are forgotten.
    pub fn apply(&mut self, list: &mut DisplayList) {
        let mut frames = HashMap::with_capacity(self.frames.len());
        for node in &mut list.spatial {
            let SpatialKind::ScrollFrame {
                scroll_id,
                frame_rect,
                content_size,
                offset,
                user_scrollable: _,
            } = &mut node.kind
            else {
                continue;
            };
            let applied = match self.frames.get(scroll_id) {
                Some(kept) if offset.generation <= kept.generation => *kept,
                _ => Applied {
                    x: offset.x,
                    y: offset.y,
                    generation: offset.generation,
                },
            };
            let (x, y) = clamp(applied.x, applied.y, *frame_rect, *content_size);
            *offset = ScrollOffset {
                x,
                y,
                generation: applied.generation,
            };
            frames.insert(
                *scroll_id,
                Applied {
                    x,
                    y,
                    generation: applied.generation,
                },
            );
        }
        self.frames = frames;
    }

    /// A user scroll by `dx`, `dy` CSS px at `at` (root space) on the list
    /// on screen. Moves the innermost scroll frame under the point that can
    /// still move that way, keeping its generation, and writes the new
    /// offset into `list`. Returns the frame and its new offset so the page
    /// can be told with `ScrollChanged`, or `None` if nothing moved.
    pub fn scroll_by(
        &mut self,
        list: &mut DisplayList,
        at: PointF,
        dx: f32,
        dy: f32,
    ) -> Option<(ScrollId, ScrollOffset)> {
        // Children come after their parents, so walking backwards tries
        // inner frames first.
        let candidates: Vec<usize> = (0..list.spatial.len())
            .rev()
            .filter(|&index| visible_at(list, index, at))
            .collect();
        candidates.into_iter().find_map(|index| {
            self.move_frame(list, index, |offset, _, _| (offset.x + dx, offset.y + dy))
        })
    }

    /// Moves the scroll frame at `index` to the offset `to` picks, clamped,
    /// and records it. `None` when it is no scroll frame or did not move.
    /// Used for key scrolling, where the frame is already known.
    pub fn scroll_frame(
        &mut self,
        list: &mut DisplayList,
        index: usize,
        to: impl FnOnce(ScrollOffset, RectF, SizeF) -> (f32, f32),
    ) -> Option<(ScrollId, ScrollOffset)> {
        self.move_frame(list, index, to)
    }

    /// Moves scroll frame `index` to the offset `to` picks, clamped, and
    /// records it. `None` when it is no scroll frame or did not move.
    fn move_frame(
        &mut self,
        list: &mut DisplayList,
        index: usize,
        to: impl FnOnce(ScrollOffset, RectF, SizeF) -> (f32, f32),
    ) -> Option<(ScrollId, ScrollOffset)> {
        let SpatialKind::ScrollFrame {
            scroll_id,
            frame_rect,
            content_size,
            offset,
            user_scrollable: _,
        } = &mut list.spatial.get_mut(index)?.kind
        else {
            return None;
        };
        let (x, y) = to(*offset, *frame_rect, *content_size);
        let (x, y) = clamp(x, y, *frame_rect, *content_size);
        if (x, y) == (offset.x, offset.y) {
            return None;
        }
        offset.x = x;
        offset.y = y;
        self.frames.insert(
            *scroll_id,
            Applied {
                x,
                y,
                generation: offset.generation,
            },
        );
        Some((*scroll_id, *offset))
    }

    /// The offset applied to `scroll_id`, if the render side knows it.
    pub fn offset(&self, scroll_id: ScrollId) -> Option<ScrollOffset> {
        self.frames.get(&scroll_id).map(|applied| ScrollOffset {
            x: applied.x,
            y: applied.y,
            generation: applied.generation,
        })
    }
}

/// Keeps an offset between zero and how far the content overhangs the frame.
fn clamp(x: f32, y: f32, frame: RectF, content: SizeF) -> (f32, f32) {
    let max_x = (content.width - frame.size.width).max(0.0);
    let max_y = (content.height - frame.size.height).max(0.0);
    // `max` then `min` also turns a NaN from a hostile list into zero.
    (x.max(0.0).min(max_x), y.max(0.0).min(max_y))
}

/// Whether spatial node `index` is a scroll frame whose scrollport, and the
/// scrollport of every scroll frame around it, contains `at`.
fn visible_at(list: &DisplayList, index: usize, at: PointF) -> bool {
    if !matches!(
        list.spatial.get(index).map(|n| &n.kind),
        Some(SpatialKind::ScrollFrame { .. })
    ) {
        return false;
    }
    let mut current = Some(index);
    // Bounded like `DisplayList::to_root`, in case of a parent cycle.
    for _ in 0..=list.spatial.len() {
        let Some(node) = current.and_then(|i| list.spatial.get(i)) else {
            return true;
        };
        if let SpatialKind::ScrollFrame { frame_rect, .. } = node.kind {
            let parent = node.parent.unwrap_or(SpatialId::ROOT);
            let Some(origin) = list.to_root(parent, frame_rect.origin) else {
                return false;
            };
            let port = RectF {
                origin,
                size: frame_rect.size,
            };
            if !port.contains(at) {
                return false;
            }
        }
        current = node.parent.map(|p| p.0 as usize);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Epoch, PageId};

    const ID: ScrollId = ScrollId(42);

    /// A list with one 100 by 100 scroll frame over 1000px of content.
    fn list(epoch: u64, y: f32, generation: u32) -> DisplayList {
        let mut list = DisplayList::new(PageId(1), Epoch(epoch), SizeF::default());
        list.push_spatial(
            SpatialId::ROOT,
            SpatialKind::ScrollFrame {
                scroll_id: ID,
                frame_rect: RectF {
                    origin: PointF { x: 0.0, y: 0.0 },
                    size: SizeF {
                        width: 100.0,
                        height: 100.0,
                    },
                },
                content_size: SizeF {
                    width: 100.0,
                    height: 1000.0,
                },
                offset: ScrollOffset {
                    x: 0.0,
                    y,
                    generation,
                },
                user_scrollable: [true, true],
            },
        );
        list
    }

    fn shown_y(list: &DisplayList) -> f32 {
        match list.spatial[1].kind {
            SpatialKind::ScrollFrame { offset, .. } => offset.y,
            _ => f32::NAN,
        }
    }

    const INSIDE: PointF = PointF { x: 50.0, y: 50.0 };

    #[test]
    fn latest_intent_wins_between_script_and_user_scrolls() {
        // AC-13
        let mut state = ScrollState::default();
        let mut first = list(1, 0.0, 0);
        state.apply(&mut first);

        // A script scrolls to 300: the list carries generation 1, so it applies.
        let mut scripted = list(2, 300.0, 1);
        state.apply(&mut scripted);
        assert_eq!(shown_y(&scripted), 300.0);

        // The user scrolls 50 further, continuing from the script's position.
        let moved = state.scroll_by(&mut scripted, INSIDE, 0.0, 50.0);
        assert_eq!(
            moved,
            Some((
                ID,
                ScrollOffset {
                    x: 0.0,
                    y: 350.0,
                    generation: 1
                }
            ))
        );
        assert_eq!(shown_y(&scripted), 350.0, "drawn at once");

        // A list painted before the page heard about the user scroll keeps
        // generation 1, so the render side keeps its own offset.
        let mut late = list(3, 300.0, 1);
        state.apply(&mut late);
        assert_eq!(shown_y(&late), 350.0);

        // A stale list with an older generation is ignored the same way.
        let mut stale = list(4, 0.0, 0);
        state.apply(&mut stale);
        assert_eq!(shown_y(&stale), 350.0);

        // The next script scroll wins again.
        let mut newer = list(5, 10.0, 2);
        state.apply(&mut newer);
        assert_eq!(shown_y(&newer), 10.0);
    }

    #[test]
    fn user_scrolls_stop_at_the_ends_and_outside_the_frame() {
        let mut state = ScrollState::default();
        let mut shown = list(1, 0.0, 0);
        state.apply(&mut shown);
        assert_eq!(state.scroll_by(&mut shown, INSIDE, 0.0, -10.0), None, "top");
        let end = state.scroll_by(&mut shown, INSIDE, 0.0, 5000.0);
        assert_eq!(end.map(|(_, o)| o.y), Some(900.0), "clamped to the end");
        assert_eq!(state.scroll_by(&mut shown, INSIDE, 0.0, 1.0), None);
        let outside = PointF { x: 150.0, y: 50.0 };
        assert_eq!(state.scroll_by(&mut shown, outside, 0.0, -1.0), None);
    }

    #[test]
    fn offsets_out_of_range_are_clamped_and_gone_frames_forgotten() {
        let mut state = ScrollState::default();
        let mut wild = list(1, f32::NAN, 0);
        state.apply(&mut wild);
        assert_eq!(shown_y(&wild), 0.0);
        let mut far = list(2, 99_999.0, 1);
        state.apply(&mut far);
        assert_eq!(shown_y(&far), 900.0);

        let mut empty = DisplayList::new(PageId(1), Epoch(3), SizeF::default());
        state.apply(&mut empty);
        assert_eq!(state.offset(ID), None);
    }

    #[test]
    fn the_inner_frame_scrolls_first_then_the_outer_one() {
        let mut shown = list(1, 0.0, 0);
        let inner_id = ScrollId(7);
        shown.push_spatial(
            SpatialId(1),
            SpatialKind::ScrollFrame {
                scroll_id: inner_id,
                frame_rect: RectF {
                    origin: PointF { x: 0.0, y: 0.0 },
                    size: SizeF {
                        width: 100.0,
                        height: 50.0,
                    },
                },
                content_size: SizeF {
                    width: 100.0,
                    height: 60.0,
                },
                offset: ScrollOffset::default(),
                user_scrollable: [true, true],
            },
        );
        let mut state = ScrollState::default();
        state.apply(&mut shown);
        let at = PointF { x: 10.0, y: 10.0 };
        let first = state.scroll_by(&mut shown, at, 0.0, 100.0);
        assert_eq!(first.map(|(id, o)| (id, o.y)), Some((inner_id, 10.0)));
        let second = state.scroll_by(&mut shown, at, 0.0, 100.0);
        assert_eq!(second.map(|(id, o)| (id, o.y)), Some((ID, 100.0)));
    }

    #[test]
    fn sticky_shift_stays_put_until_scrolled_past_it() {
        let rect = RectF {
            origin: PointF { x: 0.0, y: 100.0 },
            size: SizeF::default(),
        };
        let limits = ((0.0, 1000.0), (0.0, 1000.0));
        // Not scrolled: the box stays at its flow position.
        let not_scrolled = sticky_offset(
            rect,
            [Some(10.0), None, None, None],
            limits,
            PointF { x: 0.0, y: 0.0 },
        );
        assert_eq!(not_scrolled, PointF { x: 0.0, y: 0.0 });
        // Scrolled 150 px: the box shifts down to stay 10 px below the
        // scrollport's top edge.
        let scrolled = sticky_offset(
            rect,
            [Some(10.0), None, None, None],
            limits,
            PointF { x: 0.0, y: 150.0 },
        );
        assert_eq!(scrolled, PointF { x: 0.0, y: 60.0 });
        // No `top` inset: the box never sticks.
        let no_inset = sticky_offset(
            rect,
            [None, None, None, None],
            limits,
            PointF { x: 0.0, y: 150.0 },
        );
        assert_eq!(no_inset, PointF::default());
        // The shift never pushes past the max limit.
        let clamped = sticky_offset(
            rect,
            [Some(10.0), None, None, None],
            ((0.0, 30.0), (0.0, 1000.0)),
            PointF { x: 0.0, y: 150.0 },
        );
        assert_eq!(clamped, PointF { x: 0.0, y: 30.0 });
    }
}
