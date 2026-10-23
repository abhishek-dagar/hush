//! The Zen frame's regions in chrome px, and what a point in them means
//! (spec 0003, frame anatomy).
//!
//! The regions are read from the laid out chrome document: the page card
//! is `#page-slot`'s box, the window buttons are the elements whose
//! `data-command` is a window action, and the drag area is every
//! `data-drag-region` box not covered by a control. Only the resize border
//! is drawn by nothing: it is the `--resize-border` band along the edges.
//! The window hit tests against this, including the Windows hit test the OS
//! asks for outside the event loop.

use crate::ChromeError;
use crate::appearance::CaptionButton;
use crate::tokens::ResolvedTokens;

/// A point in chrome px.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PointF {
    /// Distance from the left edge.
    pub x: f32,
    /// Distance from the top edge.
    pub y: f32,
}

/// A size in chrome px.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SizeF {
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

/// A rectangle in chrome px.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RectF {
    /// Top left corner.
    pub origin: PointF,
    /// Width and height.
    pub size: SizeF,
}

impl RectF {
    /// A rectangle from its left, top, width and height.
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            origin: PointF { x, y },
            size: SizeF { width, height },
        }
    }

    /// Whether `point` is inside (left and top edges inclusive).
    pub fn contains(&self, point: PointF) -> bool {
        point.x >= self.origin.x
            && point.y >= self.origin.y
            && point.x < self.origin.x + self.size.width
            && point.y < self.origin.y + self.size.height
    }
}

/// A window edge or corner to resize from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeEdge {
    /// Top edge.
    North,
    /// Bottom edge.
    South,
    /// Right edge.
    East,
    /// Left edge.
    West,
    /// Top right corner.
    NorthEast,
    /// Top left corner.
    NorthWest,
    /// Bottom right corner.
    SouthEast,
    /// Bottom left corner.
    SouthWest,
}

/// What a point in the window is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// The resize border.
    Resize(ResizeEdge),
    /// A window button.
    Caption(CaptionButton),
    /// The page card.
    Page,
    /// The chrome document takes it: a control, an open menu, or chrome
    /// that does not drag.
    Chrome,
    /// Empty frame: dragging moves the window.
    Drag,
}

/// The frame sizes that come from `tokens.css` rather than from a box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameMetrics {
    /// `--radius-l`: the page card's corner radius.
    pub page_radius: f32,
    /// `--resize-border`: where the edges resize the window (Windows, Linux).
    pub resize_border: f32,
    /// `--sheet-width`: the settings window's width.
    pub sheet_width: f32,
}

impl FrameMetrics {
    /// Reads the frame tokens.
    pub fn from_tokens(tokens: &ResolvedTokens) -> Result<Self, ChromeError> {
        Ok(Self {
            page_radius: tokens.px("--radius-l")?,
            resize_border: tokens.px("--resize-border")?,
            sheet_width: tokens.px("--sheet-width")?,
        })
    }
}

/// Where everything is in the window, in chrome px, from the chrome
/// document's last layout.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FrameLayout {
    /// The window in chrome px.
    pub size: SizeF,
    /// The page card, `#page-slot`'s box. `None` when the chrome document
    /// has no page (the gallery).
    pub page: Option<RectF>,
    /// The page card's corner radius.
    pub page_radius: f32,
    /// The window buttons the chrome draws, with their boxes. Empty on
    /// macOS, which keeps the native traffic lights.
    pub caption_buttons: Vec<(CaptionButton, RectF)>,
    /// The overlay layer's boxes that take the pointer: the open palette
    /// and its scrim over the page card (spec 0008, AC-6). Checked before
    /// the page.
    pub overlay: Vec<RectF>,
    /// Boxes the chrome document handles itself: controls and open menus.
    pub controls: Vec<RectF>,
    /// `data-drag-region` boxes.
    pub drag: Vec<RectF>,
    /// The resize border width; zero where the OS resizes the window.
    pub resize_border: f32,
    /// The sidebar's top row, `#nav-row`: on macOS the traffic lights are
    /// centered on it.
    pub title_row: Option<RectF>,
}

impl FrameLayout {
    /// What `point` is, checked in this order: resize border (unless
    /// `maximized`), window buttons, the overlay, controls, the page card,
    /// the drag area, and anything else is the chrome's.
    pub fn hit(&self, point: PointF, maximized: bool) -> Hit {
        if !maximized && let Some(edge) = self.edge(point) {
            return Hit::Resize(edge);
        }
        if let Some((button, _)) = self.caption_buttons.iter().find(|(_, r)| r.contains(point)) {
            return Hit::Caption(*button);
        }
        if self.overlay.iter().any(|r| r.contains(point))
            || self.controls.iter().any(|r| r.contains(point))
        {
            return Hit::Chrome;
        }
        if self.page.is_some_and(|page| page.contains(point)) {
            return Hit::Page;
        }
        if self.drag.iter().any(|r| r.contains(point)) {
            return Hit::Drag;
        }
        Hit::Chrome
    }

    fn edge(&self, point: PointF) -> Option<ResizeEdge> {
        let border = self.resize_border;
        if border <= 0.0 {
            return None;
        }
        let west = point.x < border;
        let east = point.x >= self.size.width - border;
        let north = point.y < border;
        let south = point.y >= self.size.height - border;
        Some(match (north, south, west, east) {
            (true, _, true, _) => ResizeEdge::NorthWest,
            (true, _, _, true) => ResizeEdge::NorthEast,
            (_, true, true, _) => ResizeEdge::SouthWest,
            (_, true, _, true) => ResizeEdge::SouthEast,
            (true, ..) => ResizeEdge::North,
            (_, true, ..) => ResizeEdge::South,
            (_, _, true, _) => ResizeEdge::West,
            (_, _, _, true) => ResizeEdge::East,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TOKENS_CSS, Tokens};

    fn metrics() -> FrameMetrics {
        let tokens = Tokens::parse(TOKENS_CSS).unwrap().resolve(&[]);
        FrameMetrics::from_tokens(&tokens).unwrap()
    }

    /// A 1280 by 800 window: a page card, a menu button, Windows style
    /// buttons top right, and the whole window as drag area.
    fn layout(resize_border: f32) -> FrameLayout {
        let size = SizeF {
            width: 1280.0,
            height: 800.0,
        };
        FrameLayout {
            size,
            page: Some(RectF::new(240.0, 32.0, 1032.0, 760.0)),
            page_radius: 8.0,
            caption_buttons: vec![
                (CaptionButton::Minimize, RectF::new(1142.0, 0.0, 46.0, 32.0)),
                (CaptionButton::Maximize, RectF::new(1188.0, 0.0, 46.0, 32.0)),
                (CaptionButton::Close, RectF::new(1234.0, 0.0, 46.0, 32.0)),
            ],
            overlay: Vec::new(),
            controls: vec![RectF::new(4.0, 2.0, 28.0, 28.0)],
            drag: vec![RectF::new(0.0, 0.0, 1280.0, 800.0)],
            resize_border,
            title_row: None,
        }
    }

    #[test]
    fn the_tokens_give_the_card_radius_resize_border_and_sheet_width() {
        let m = metrics();
        assert_eq!(m.page_radius, 8.0);
        assert_eq!(m.resize_border, 6.0);
        assert_eq!(m.sheet_width, 480.0);
    }

    #[test]
    fn buttons_controls_page_and_drag_area_hit_in_that_order() {
        let frame = layout(0.0);
        let hit = |x, y| frame.hit(PointF { x, y }, false);
        assert_eq!(hit(1200.0, 16.0), Hit::Caption(CaptionButton::Maximize));
        assert_eq!(hit(10.0, 10.0), Hit::Chrome, "the menu button");
        assert_eq!(hit(600.0, 400.0), Hit::Page);
        assert_eq!(hit(100.0, 400.0), Hit::Drag, "the empty sidebar");
        assert_eq!(hit(1.0, 1.0), Hit::Drag, "no resize border on macOS");
    }

    #[test]
    fn the_overlay_takes_the_pointer_before_the_page() {
        // Spec 0008, AC-6: an open palette's scrim covers the card.
        let mut frame = layout(0.0);
        assert_eq!(frame.hit(PointF { x: 600.0, y: 400.0 }, false), Hit::Page);
        frame.overlay.push(RectF::new(240.0, 32.0, 1032.0, 760.0));
        assert_eq!(frame.hit(PointF { x: 600.0, y: 400.0 }, false), Hit::Chrome);
    }

    #[test]
    fn chrome_without_a_drag_region_or_page_is_the_chrome_documents() {
        let gallery = FrameLayout {
            size: SizeF {
                width: 800.0,
                height: 600.0,
            },
            ..FrameLayout::default()
        };
        assert_eq!(gallery.hit(PointF { x: 5.0, y: 5.0 }, false), Hit::Chrome);
    }

    #[test]
    fn edges_resize_unless_maximized() {
        let frame = layout(6.0);
        let hit = |x, y, maximized| frame.hit(PointF { x, y }, maximized);
        assert_eq!(hit(2.0, 2.0, false), Hit::Resize(ResizeEdge::NorthWest));
        assert_eq!(hit(1279.0, 400.0, false), Hit::Resize(ResizeEdge::East));
        assert_eq!(hit(600.0, 799.0, false), Hit::Resize(ResizeEdge::South));
        assert_eq!(hit(600.0, 799.0, true), Hit::Drag);
        assert_eq!(
            hit(1279.0, 1.0, false),
            Hit::Resize(ResizeEdge::NorthEast),
            "the corner beats the close button"
        );
    }
}
