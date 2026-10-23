//! Points, sizes, rectangles and side offsets. Layout uses them in [`Au`].

use crate::Au;

/// A point, in physical coordinates with a top left origin.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Point<T = Au> {
    /// Distance from the left.
    pub x: T,
    /// Distance from the top.
    pub y: T,
}

impl<T> Point<T> {
    /// A point at `x`, `y`.
    pub const fn new(x: T, y: T) -> Self {
        Self { x, y }
    }
}

impl Point<Au> {
    /// Adds an offset to this point.
    pub fn offset_by(self, other: Self) -> Self {
        Self::new(self.x + other.x, self.y + other.y)
    }
}

/// A width and a height.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Size<T = Au> {
    /// Horizontal extent.
    pub width: T,
    /// Vertical extent.
    pub height: T,
}

impl<T> Size<T> {
    /// A size of `width` × `height`.
    pub const fn new(width: T, height: T) -> Self {
        Self { width, height }
    }
}

/// A rectangle given by its top left corner and its size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Rect<T = Au> {
    /// The top left corner.
    pub origin: Point<T>,
    /// The width and height.
    pub size: Size<T>,
}

impl<T> Rect<T> {
    /// A rectangle at `origin` with `size`.
    pub const fn new(origin: Point<T>, size: Size<T>) -> Self {
        Self { origin, size }
    }
}

/// One value per side of a box, such as margins or border widths.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SideOffsets<T = Au> {
    /// The top side.
    pub top: T,
    /// The right side.
    pub right: T,
    /// The bottom side.
    pub bottom: T,
    /// The left side.
    pub left: T,
}

impl SideOffsets<Au> {
    /// Left plus right.
    pub fn horizontal(&self) -> Au {
        self.left + self.right
    }

    /// Top plus bottom.
    pub fn vertical(&self) -> Au {
        self.top + self.bottom
    }
}
