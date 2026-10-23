//! What a style change costs downstream (spec 0002, Style damage, AC-8).

use std::rc::Rc;

use crate::ComputedStyle;

/// The work a style change causes, from least to most. Variants are
/// ordered, so the worst of several changes is their `max`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StyleDamage {
    /// Nothing visible changed.
    #[default]
    None,
    /// Colors, background, visibility, effects or the outline changed:
    /// paint again.
    Repaint,
    /// Sizes, margins, padding, borders, fonts, text or flex properties
    /// changed: lay the box out again.
    Relayout,
    /// `display`, `position`, `float` or anything else that decides which
    /// boxes exist changed: build the boxes again.
    RebuildBoxes,
}

impl StyleDamage {
    /// Compares `old` and `new` group by group: pointer first, which is
    /// enough for groups that were shared, then by value.
    pub fn compute(old: &ComputedStyle, new: &ComputedStyle) -> Self {
        if std::ptr::eq(old, new) {
            return Self::None;
        }
        if !same(&old.display_box, &new.display_box) {
            return Self::RebuildBoxes;
        }

        let text_layout_changed = !same(&old.inherited_text, &new.inherited_text)
            && !old.inherited_text.same_layout(&new.inherited_text);
        let border_layout_changed =
            !same(&old.border, &new.border) && !old.border.same_layout(&new.border);
        let relayout = text_layout_changed
            || border_layout_changed
            || !same(&old.font, &new.font)
            || !same(&old.list, &new.list)
            || !same(&old.size, &new.size)
            || !same(&old.margin, &new.margin)
            || !same(&old.padding, &new.padding)
            || !same(&old.position_offsets, &new.position_offsets)
            || !same(&old.flex, &new.flex);
        if relayout {
            return Self::Relayout;
        }

        // What is left in these groups only changes how things look.
        let repaint = !same(&old.inherited_text, &new.inherited_text)
            || !same(&old.border, &new.border)
            || !same(&old.inherited_box, &new.inherited_box)
            || !same(&old.background, &new.background)
            || !same(&old.effects, &new.effects)
            || !same(&old.outline, &new.outline);
        if repaint { Self::Repaint } else { Self::None }
    }
}

fn same<T: PartialEq>(old: &Rc<T>, new: &Rc<T>) -> bool {
    Rc::ptr_eq(old, new) || old == new
}

#[cfg(test)]
mod tests {
    use crate::{BorderStyle, Color, Display, Length, Position};

    use super::*;

    fn changed(change: impl FnOnce(&mut ComputedStyle)) -> StyleDamage {
        let old = ComputedStyle::default();
        let mut new = old.clone();
        change(&mut new);
        StyleDamage::compute(&old, &new)
    }

    #[test]
    fn each_kind_of_change_gives_its_damage() {
        // AC-8
        assert_eq!(changed(|_| {}), StyleDamage::None);
        assert_eq!(
            changed(|s| Rc::make_mut(&mut s.inherited_text).color = Color::rgb8(1, 2, 3)),
            StyleDamage::Repaint
        );
        assert_eq!(
            changed(|s| Rc::make_mut(&mut s.background).color = Color::BLACK),
            StyleDamage::Repaint
        );
        assert_eq!(
            changed(|s| Rc::make_mut(&mut s.size).width = Some(Length::px(10.0))),
            StyleDamage::Relayout
        );
        assert_eq!(
            changed(|s| Rc::make_mut(&mut s.margin).left = Some(Length::px(1.0))),
            StyleDamage::Relayout
        );
        assert_eq!(
            changed(|s| Rc::make_mut(&mut s.display_box).display = Display::Block),
            StyleDamage::RebuildBoxes
        );
        assert_eq!(
            changed(|s| Rc::make_mut(&mut s.display_box).position = Position::Absolute),
            StyleDamage::RebuildBoxes
        );
    }

    #[test]
    fn equal_groups_in_different_allocations_give_no_damage() {
        let old = ComputedStyle::default();
        let mut new = old.clone();
        new.margin = Rc::new((*old.margin).clone());
        assert!(!Rc::ptr_eq(&old.margin, &new.margin));
        assert_eq!(StyleDamage::compute(&old, &new), StyleDamage::None);
    }

    #[test]
    fn border_colors_repaint_and_border_widths_relayout() {
        assert_eq!(
            changed(|s| Rc::make_mut(&mut s.border).top.color = Color::rgb8(9, 9, 9)),
            StyleDamage::Repaint
        );
        assert_eq!(
            changed(|s| {
                let top = &mut Rc::make_mut(&mut s.border).top;
                top.style = BorderStyle::Solid;
                top.width = base::Au::from_px(2.0);
            }),
            StyleDamage::Relayout
        );
    }

    #[test]
    fn the_worst_change_wins() {
        assert_eq!(
            changed(|s| {
                Rc::make_mut(&mut s.inherited_text).color = Color::rgb8(1, 2, 3);
                Rc::make_mut(&mut s.padding).top = Length::px(3.0);
            }),
            StyleDamage::Relayout
        );
        assert!(StyleDamage::Repaint < StyleDamage::RebuildBoxes);
    }
}
