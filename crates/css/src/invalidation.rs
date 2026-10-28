//! Which elements a change to an id, class, attribute or element state can
//! restyle (spec 0004, child 0003, Invalidation, AC-21; spec 0006, AC-19).

use std::collections::HashMap;

use base::atoms::{Atom, LocalName, QualName};
use base::{local_name, ns};

use crate::selector::{Compound, Selector, Simple};
use crate::{MatchElement, PseudoState, RestyleHint};

/// For every id, class, attribute name and element state some selector
/// mentions, the elements a change to it can restyle.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InvalidationMap {
    ids: HashMap<Atom, RestyleHint>,
    classes: HashMap<Atom, RestyleHint>,
    attributes: HashMap<LocalName, RestyleHint>,
    states: HashMap<PseudoState, RestyleHint>,
}

impl InvalidationMap {
    /// Records what `selector` mentions, by where it mentions it.
    pub(crate) fn add(&mut self, selector: &Selector) {
        for (index, compound) in selector.compounds.iter().enumerate() {
            let hint = match index {
                0 => RestyleHint::SELF,
                // The combinator just right of this compound decides who
                // it reaches: later siblings, or descendants.
                _ if selector.combinators[index - 1].is_sibling() => RestyleHint::LATER_SIBLINGS,
                _ => RestyleHint::SUBTREE,
            };
            self.add_compound(compound, hint);
        }
    }

    fn add_compound(&mut self, compound: &Compound, hint: RestyleHint) {
        for simple in &compound.0 {
            match simple {
                Simple::Id(id) => merge(&mut self.ids, id.clone(), hint),
                Simple::Class(class) => merge(&mut self.classes, class.clone(), hint),
                Simple::Attribute { name, .. } => merge(&mut self.attributes, name.clone(), hint),
                Simple::Link | Simple::Visited => {
                    merge(&mut self.attributes, local_name!("href"), hint);
                }
                Simple::State(state) => merge(&mut self.states, *state, hint),
                // `:enabled` flips exactly when `:disabled` does.
                Simple::Enabled => merge(&mut self.states, PseudoState::Disabled, hint),
                Simple::Not(list) | Simple::Is(list) | Simple::Where(list) | Simple::Has(list) => {
                    for selector in list {
                        for compound in &selector.compounds {
                            self.add_compound(compound, hint);
                        }
                    }
                }
                Simple::Type { .. } | Simple::FirstChild | Simple::LastChild | Simple::Root => {}
            }
        }
    }

    fn attribute(&self, name: &LocalName) -> RestyleHint {
        self.attributes.get(name).copied().unwrap_or_default()
    }
}

fn merge<K: std::hash::Hash + Eq>(map: &mut HashMap<K, RestyleHint>, key: K, hint: RestyleHint) {
    let entry = map.entry(key).or_default();
    *entry = *entry | hint;
}

/// What changed on an element, for [`restyle_hint`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementChange<'a> {
    /// An attribute other than `id` and `class` was set, changed or removed.
    Attribute(&'a QualName),
    /// The `id` changed from `old` to `new`.
    Id {
        /// The id before.
        old: Option<&'a Atom>,
        /// The id after.
        new: Option<&'a Atom>,
    },
    /// The `class` attribute changed; these classes were added or removed.
    Class(&'a [Atom]),
    /// The element entered or left `state` (spec 0006, AC-19).
    State(PseudoState),
}

/// Which elements `change` on `element` may restyle (spec 0002, Mutation
/// API, step 4).
///
/// With the document's `map`, only what some selector mentions restyles,
/// and only as far as that selector reaches. A `style` attribute always
/// restyles the element itself. Without a map (a document whose styles are
/// assigned by hand), every change restyles the element and its subtree,
/// plus later siblings when sibling selectors matched it.
pub fn restyle_hint(
    map: Option<&InvalidationMap>,
    element: &impl MatchElement,
    change: &ElementChange<'_>,
) -> RestyleHint {
    let Some(map) = map else {
        let siblings = if element.is_affected_by_sibling_selectors() {
            RestyleHint::LATER_SIBLINGS
        } else {
            RestyleHint::NONE
        };
        return RestyleHint::SELF | RestyleHint::SUBTREE | siblings;
    };
    let get = |table: &HashMap<Atom, RestyleHint>, key: Option<&Atom>| {
        key.and_then(|key| table.get(key))
            .copied()
            .unwrap_or_default()
    };
    match *change {
        ElementChange::Attribute(name) => {
            if name.ns != ns!("") {
                return RestyleHint::NONE;
            }
            if name.local == local_name!("style") {
                return RestyleHint::SELF;
            }
            map.attribute(&name.local)
        }
        ElementChange::Id { old, new } => {
            get(&map.ids, old) | get(&map.ids, new) | map.attribute(&local_name!("id"))
        }
        ElementChange::Class(changed) => changed
            .iter()
            .fold(map.attribute(&local_name!("class")), |hint, class| {
                hint | get(&map.classes, Some(class))
            }),
        // No rule uses the state: nothing restyles. Only in subject
        // compounds: the element itself. Further left: its subtree, plus
        // later siblings' for sibling combinators.
        ElementChange::State(state) => map.states.get(&state).copied().unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selector::tests::parse;
    use crate::sharing::tests::Element;

    fn map(selectors: &str) -> InvalidationMap {
        let mut map = InvalidationMap::default();
        for selector in parse(selectors).unwrap() {
            map.add(&selector);
        }
        map
    }

    #[test]
    fn hints_reach_as_far_as_the_selectors_do() {
        // AC-21
        let map = map(".self, .up .x, .left + .x, .left2 ~ .x, #id, [data-on], a:link");
        let class = |name: &str| {
            let changed = [Atom::from(name)];
            restyle_hint(
                Some(&map),
                &Element::default(),
                &ElementChange::Class(&changed),
            )
        };
        assert_eq!(class("self"), RestyleHint::SELF);
        assert_eq!(class("up"), RestyleHint::SUBTREE);
        assert_eq!(class("left"), RestyleHint::LATER_SIBLINGS);
        assert_eq!(class("left2"), RestyleHint::LATER_SIBLINGS);
        assert_eq!(class("unused"), RestyleHint::NONE);
        assert_eq!(class("x"), RestyleHint::SELF);

        let attr = |name: &str| {
            let name = QualName::attribute(LocalName::from(name));
            restyle_hint(
                Some(&map),
                &Element::default(),
                &ElementChange::Attribute(&name),
            )
        };
        assert_eq!(attr("data-on"), RestyleHint::SELF);
        assert_eq!(attr("href"), RestyleHint::SELF);
        assert_eq!(attr("title"), RestyleHint::NONE);
        assert_eq!(attr("style"), RestyleHint::SELF);

        let id = Atom::from("id");
        let hint = restyle_hint(
            Some(&map),
            &Element::default(),
            &ElementChange::Id {
                old: Some(&id),
                new: None,
            },
        );
        assert_eq!(hint, RestyleHint::SELF);
    }

    #[test]
    fn state_changes_restyle_only_as_far_as_state_selectors_reach() {
        // Spec 0006, AC-19
        let hint = |selectors: &str| {
            restyle_hint(
                Some(&map(selectors)),
                &Element::default(),
                &ElementChange::State(PseudoState::Hover),
            )
        };
        assert_eq!(hint(".a, .b .c"), RestyleHint::NONE, "no rule uses it");
        assert_eq!(hint("button:hover"), RestyleHint::SELF);
        assert_eq!(hint(".row:hover .label"), RestyleHint::SUBTREE);
        assert_eq!(hint(".x:hover + .y"), RestyleHint::LATER_SIBLINGS);
        assert_eq!(
            hint(":not(:hover)"),
            RestyleHint::SELF,
            "a negation reaches as far as its compound"
        );
        let disabled = |selectors: &str| {
            restyle_hint(
                Some(&map(selectors)),
                &Element::default(),
                &ElementChange::State(PseudoState::Disabled),
            )
        };
        assert_eq!(disabled("button:enabled"), RestyleHint::SELF);
        assert_eq!(disabled("fieldset:disabled .x"), RestyleHint::SUBTREE);
    }

    #[test]
    fn without_a_map_every_change_restyles_the_subtree() {
        let name = QualName::attribute(local_name!("class"));
        let hint = restyle_hint(None, &Element::default(), &ElementChange::Attribute(&name));
        assert_eq!(hint, RestyleHint::SELF | RestyleHint::SUBTREE);
    }
}
