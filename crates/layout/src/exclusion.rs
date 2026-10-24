//! The exclusion space: an immutable list of the rectangles floats occupy,
//! in BFC coordinates (spec 0009, child 0001). It is carried in the
//! constraint space so that only boxes a float can reach see it in their
//! layout cache key, and boxes past the floats still reuse their fragment.

use std::rc::Rc;

use crate::box_tree::FloatSide;
use base::{Au, Point, Rect, Size};

/// A float's margin box, as an exclusion that later content must avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Exclusion {
    /// The float's margin box, in BFC coordinates.
    pub rect: Rect,
    /// Which side the float sits on.
    pub side: FloatSide,
}

/// The result of [`ExclusionSpace::opportunity`]: the first band at or below
/// `block_start` where `min_inline` fits between the floats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opportunity {
    /// The block offset where content can start (the band's top).
    pub rect: Rect,
}

/// The immutable record of where floats sit in one BFC.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExclusionSpace {
    /// The floats, in placement order, in BFC coordinates.
    pub exclusions: Rc<[Exclusion]>,
    /// The block end of the lowest placed left float, or zero.
    pub clear_left: Au,
    /// The block end of the lowest placed right float, or zero.
    pub clear_right: Au,
}

impl ExclusionSpace {
    /// An empty space: nothing excludes anything.
    pub fn empty() -> Self {
        Self {
            exclusions: Rc::new([]),
            clear_left: Au::ZERO,
            clear_right: Au::ZERO,
        }
    }

    /// Returns a new space with `exclusion` added.
    pub fn add(&self, exclusion: Exclusion) -> Self {
        let mut exclusions: Vec<Exclusion> = self.exclusions.iter().copied().collect();
        exclusions.push(exclusion);
        let (clear_left, clear_right) = match exclusion.side {
            FloatSide::Left => (
                self.clear_left
                    .max(exclusion.rect.origin.y + exclusion.rect.size.height),
                self.clear_right,
            ),
            FloatSide::Right => (
                self.clear_left,
                self.clear_right
                    .max(exclusion.rect.origin.y + exclusion.rect.size.height),
            ),
        };
        Self {
            exclusions: exclusions.into(),
            clear_left,
            clear_right,
        }
    }

    /// The first band at or below `block_start` whose free inline range (the
    /// gap between the lowest left float edge and the highest right float
    /// edge at that block offset) is at least `min_inline` wide within
    /// `inline_size`. Past all floats it returns the full width at `top`.
    pub fn opportunity(&self, block_start: Au, inline_size: Au, min_inline: Au) -> Opportunity {
        // Collect the distinct block offsets where the set of floats changes:
        // each float's top and bottom, plus where we start. Sort them so we
        // can walk band by band.
        let mut boundaries: Vec<Au> = self
            .exclusions
            .iter()
            .flat_map(|e| [e.rect.origin.y, e.rect.origin.y + e.rect.size.height])
            .collect();
        boundaries.push(block_start);
        boundaries.sort_unstable();
        boundaries.dedup();

        let mut top = block_start;
        for &bottom in &boundaries {
            if bottom <= top {
                continue;
            }
            // Free width between the lowest left edge and the highest right
            // edge among floats that cover this band.
            let mut left = Au::ZERO;
            let mut right = inline_size;
            for exclusion in &*self.exclusions {
                if exclusion.rect.origin.y < bottom
                    && exclusion.rect.origin.y + exclusion.rect.size.height > top
                {
                    match exclusion.side {
                        FloatSide::Left => {
                            left = left.max(exclusion.rect.origin.x + exclusion.rect.size.width)
                        }
                        FloatSide::Right => right = right.min(exclusion.rect.origin.x),
                    }
                }
            }
            let free = right - left;
            if free >= min_inline {
                return Opportunity {
                    rect: Rect::new(Point::new(left, top), Size::new(free, bottom - top)),
                };
            }
            top = bottom;
        }
        // Past all floats: the full width, starting where we are.
        Opportunity {
            rect: Rect::new(Point::new(Au::ZERO, top), Size::new(inline_size, Au::ZERO)),
        }
    }

    /// The block offset that `clear` requires: `clear_left`, `clear_right`,
    /// or their max, relative to the BFC's top. `None` for `none`.
    pub fn clearance(&self, clear: FloatSide) -> Option<Au> {
        match clear {
            FloatSide::Left => Some(self.clear_left),
            FloatSide::Right => Some(self.clear_right),
        }
    }

    /// The block offset that `clear: both` requires.
    pub fn clearance_both(&self) -> Au {
        self.clear_left.max(self.clear_right)
    }
}

impl Default for ExclusionSpace {
    fn default() -> Self {
        Self::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::new(
            Point::new(Au::from_px(x), Au::from_px(y)),
            Size::new(Au::from_px(w), Au::from_px(h)),
        )
    }

    fn left(x: f32, y: f32, w: f32, h: f32) -> Exclusion {
        Exclusion {
            rect: rect(x, y, w, h),
            side: FloatSide::Left,
        }
    }

    fn right(x: f32, y: f32, w: f32, h: f32) -> Exclusion {
        Exclusion {
            rect: rect(x, y, w, h),
            side: FloatSide::Right,
        }
    }

    #[test]
    fn a_line_beside_two_floats_gets_the_gap_between_them() {
        let space = ExclusionSpace::empty()
            .add(left(0.0, 0.0, 100.0, 50.0))
            .add(right(200.0, 0.0, 100.0, 50.0));
        // The gap between the floats is 100 px wide; anything up to that fits.
        let opp = space.opportunity(Au::ZERO, Au::from_px(400.0), Au::from_px(50.0));
        assert_eq!(opp.rect.origin.x, Au::from_px(100.0));
        assert_eq!(opp.rect.size.width, Au::from_px(100.0));
    }

    #[test]
    fn content_below_a_float_gets_the_full_width() {
        let space = ExclusionSpace::empty().add(left(0.0, 0.0, 100.0, 50.0));
        // 400 px does not fit beside the 100 px float, so the line drops below it.
        let opp = space.opportunity(Au::ZERO, Au::from_px(400.0), Au::from_px(400.0));
        assert_eq!(opp.rect.origin.y, Au::from_px(50.0));
        assert_eq!(opp.rect.size.width, Au::from_px(400.0));
    }

    #[test]
    fn clearance_is_the_lowest_float_on_each_side() {
        let space = ExclusionSpace::empty()
            .add(left(0.0, 0.0, 100.0, 50.0))
            .add(left(0.0, 10.0, 100.0, 30.0))
            .add(right(0.0, 0.0, 100.0, 70.0));
        // The lowest left float ends at 50 px (the first one), not 40.
        assert_eq!(space.clearance(FloatSide::Left), Some(Au::from_px(50.0)));
        assert_eq!(space.clearance(FloatSide::Right), Some(Au::from_px(70.0)));
        assert_eq!(space.clearance_both(), Au::from_px(70.0));
    }

    #[test]
    fn an_empty_space_offers_the_full_width_immediately() {
        let space = ExclusionSpace::empty();
        let opp = space.opportunity(Au::ZERO, Au::from_px(400.0), Au::from_px(100.0));
        assert_eq!(opp.rect.origin, Point::default());
        assert_eq!(opp.rect.size.width, Au::from_px(400.0));
    }

    #[test]
    fn equal_spaces_compare_equal() {
        let space = ExclusionSpace::empty().add(left(0.0, 0.0, 100.0, 50.0));
        assert_eq!(space, space.clone());
    }
}
