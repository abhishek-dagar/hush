//! The style sharing cache: siblings that would compute the same style get
//! the same `Rc<ComputedStyle>` (spec 0002, Style sharing cache, AC-7).

use std::collections::VecDeque;
use std::rc::Rc;

use base::atoms::QualName;
use base::local_name;

use crate::{ComputedStyle, MatchElement};

/// How many candidates the cache keeps.
pub const SHARING_CACHE_SIZE: usize = 16;

/// A recently styled element whose style a later sibling may share.
#[derive(Debug)]
struct Candidate<K> {
    element: K,
    parent_style: Option<Rc<ComputedStyle>>,
    style: Rc<ComputedStyle>,
}

/// A most recently used list of styled siblings, cleared whenever the
/// traversal moves to a different parent.
///
/// `K` is how the caller names an element (a `NodeId` in `dom`), so this
/// crate never names a `dom` type. The cache keeps only keys and resolves
/// them back to elements when it compares.
#[derive(Debug)]
pub struct SharingCache<K> {
    parent: Option<K>,
    /// Most recent first.
    candidates: VecDeque<Candidate<K>>,
}

impl<K> Default for SharingCache<K> {
    fn default() -> Self {
        Self {
            parent: None,
            candidates: VecDeque::with_capacity(SHARING_CACHE_SIZE),
        }
    }
}

impl<K: Copy + PartialEq> SharingCache<K> {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many candidates are cached.
    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    /// Whether no candidates are cached.
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    /// Forgets every candidate.
    pub fn clear(&mut self) {
        self.parent = None;
        self.candidates.clear();
    }

    /// Finds a cached sibling whose style `element`, a child of `parent`
    /// styled as `parent_style`, can share. `resolve` turns a cached key
    /// back into an element; a key it cannot resolve is skipped. A hit
    /// becomes the most recent candidate.
    pub fn lookup<E: MatchElement>(
        &mut self,
        parent: K,
        parent_style: Option<&Rc<ComputedStyle>>,
        element: &E,
        resolve: impl Fn(K) -> Option<E>,
    ) -> Option<Rc<ComputedStyle>> {
        self.enter(parent);
        if !may_share(element) {
            return None;
        }
        let index = self.candidates.iter().position(|candidate| {
            same_parent_style(candidate.parent_style.as_ref(), parent_style)
                && resolve(candidate.element).is_some_and(|other| can_share(element, &other))
        })?;
        let hit = self.candidates.remove(index)?;
        let style = Rc::clone(&hit.style);
        self.candidates.push_front(hit);
        Some(style)
    }

    /// Remembers that `element`, named `key` and a child of `parent`, was
    /// styled `style`. Elements that may never share are not cached.
    pub fn insert<E: MatchElement>(
        &mut self,
        parent: K,
        parent_style: Option<&Rc<ComputedStyle>>,
        key: K,
        element: &E,
        style: Rc<ComputedStyle>,
    ) {
        self.enter(parent);
        if !may_share(element) {
            return;
        }
        self.candidates.retain(|candidate| candidate.element != key);
        self.candidates.truncate(SHARING_CACHE_SIZE - 1);
        self.candidates.push_front(Candidate {
            element: key,
            parent_style: parent_style.map(Rc::clone),
            style,
        });
    }

    /// Clears the cache when the traversal moved to another parent.
    fn enter(&mut self, parent: K) {
        if self.parent != Some(parent) {
            self.candidates.clear();
            self.parent = Some(parent);
        }
    }
}

/// Whether `element` may share a style at all: nothing about it is unique
/// to it, and no sibling selector can tell it apart from its siblings.
fn may_share(element: &impl MatchElement) -> bool {
    element.id().is_none()
        && element
            .attribute(&QualName::attribute(local_name!("style")))
            .is_none()
        && !element.has_shadow_root()
        && !element.is_affected_by_sibling_selectors()
}

/// Whether two siblings would compute the same style.
fn can_share<E: MatchElement>(element: &E, other: &E) -> bool {
    may_share(other)
        && element.name() == other.name()
        && element.classes() == other.classes()
        && element.same_attributes(other)
        && element.same_state(other)
}

fn same_parent_style(a: Option<&Rc<ComputedStyle>>, b: Option<&Rc<ComputedStyle>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Rc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use base::atoms::Atom;

    use super::*;
    use crate::Color;

    /// A synthetic element with no relatives, so the cache's rules are
    /// checked on their own (spec 0002, Consequences).
    #[derive(Clone, Default)]
    pub(crate) struct Element {
        name: Option<QualName>,
        id: Option<Atom>,
        classes: Vec<Atom>,
        attrs: Vec<(QualName, String)>,
        state: u16,
        shadow: bool,
        sibling_selectors: bool,
    }

    impl MatchElement for Element {
        fn name(&self) -> &QualName {
            self.name.as_ref().expect("test elements are named")
        }
        fn id(&self) -> Option<&Atom> {
            self.id.as_ref()
        }
        fn classes(&self) -> &[Atom] {
            &self.classes
        }
        fn attribute(&self, name: &QualName) -> Option<&str> {
            self.attrs
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.as_str())
        }
        fn parent_element(&self) -> Option<Self> {
            None
        }
        fn prev_sibling_element(&self) -> Option<Self> {
            None
        }
        fn next_sibling_element(&self) -> Option<Self> {
            None
        }
        fn is_root(&self) -> bool {
            false
        }
        fn same_attributes(&self, other: &Self) -> bool {
            self.attrs == other.attrs
        }
        fn same_state(&self, other: &Self) -> bool {
            self.state == other.state
        }
        fn is_in_state(&self, state: crate::PseudoState) -> bool {
            self.state & (1 << state as u16) != 0
        }
        fn has_shadow_root(&self) -> bool {
            self.shadow
        }
        fn is_affected_by_sibling_selectors(&self) -> bool {
            self.sibling_selectors
        }
    }

    fn div() -> Element {
        Element {
            name: Some(QualName::html(local_name!("div"))),
            classes: vec![Atom::from("card")],
            attrs: vec![(QualName::attribute(local_name!("class")), "card".into())],
            ..Element::default()
        }
    }

    /// A cache holding `div()` as element 0 under parent 100, its style, and
    /// the parent style.
    fn cache() -> (
        SharingCache<u32>,
        Vec<Element>,
        Rc<ComputedStyle>,
        Rc<ComputedStyle>,
    ) {
        let parent_style = ComputedStyle::initial();
        let mut style = ComputedStyle::inherit_from(&parent_style);
        Rc::make_mut(&mut style.background).color = Color::rgb8(1, 2, 3);
        let style = Rc::new(style);
        let mut cache = SharingCache::new();
        let elements = vec![div()];
        cache.insert(100, Some(&parent_style), 0, &elements[0], Rc::clone(&style));
        (cache, elements, style, parent_style)
    }

    fn try_share(element: Element) -> Option<Rc<ComputedStyle>> {
        let (mut cache, elements, _, parent_style) = cache();
        cache.lookup(100, Some(&parent_style), &element, |k| {
            elements.get(k as usize).cloned()
        })
    }

    #[test]
    fn an_identical_sibling_gets_the_very_same_style() {
        // AC-7
        let (mut cache, elements, style, parent_style) = cache();
        let shared = cache
            .lookup(100, Some(&parent_style), &div(), |k| {
                elements.get(k as usize).cloned()
            })
            .expect("a hit");
        assert!(Rc::ptr_eq(&shared, &style));
    }

    #[test]
    fn anything_that_sets_an_element_apart_refuses_sharing() {
        // AC-7
        let with_attr = |name: &str, value: &str| {
            let mut e = div();
            e.attrs
                .push((QualName::attribute(name.into()), value.to_owned()));
            e
        };
        let cases: [(&str, Element); 7] = [
            (
                "an id",
                Element {
                    id: Some(Atom::from("x")),
                    ..with_attr("id", "x")
                },
            ),
            ("a style attribute", with_attr("style", "color: red")),
            (
                "a different class list",
                Element {
                    classes: vec![Atom::from("other")],
                    ..div()
                },
            ),
            ("different attributes", with_attr("title", "hi")),
            ("different state", Element { state: 1, ..div() }),
            (
                "sibling selectors",
                Element {
                    sibling_selectors: true,
                    ..div()
                },
            ),
            (
                "a shadow root",
                Element {
                    shadow: true,
                    ..div()
                },
            ),
        ];
        for (why, element) in cases {
            assert!(try_share(element).is_none(), "{why} must refuse sharing");
        }
        assert!(try_share(div()).is_some());
    }

    #[test]
    fn a_different_parent_or_parent_style_misses() {
        let (mut cache, elements, _, parent_style) = cache();
        let resolve = |k: u32| elements.get(k as usize).cloned();
        let other_style = Rc::new(ComputedStyle::default());
        assert!(
            cache
                .lookup(100, Some(&other_style), &div(), resolve)
                .is_none()
        );
        assert!(
            cache
                .lookup(200, Some(&parent_style), &div(), resolve)
                .is_none()
        );
        assert!(
            cache.is_empty(),
            "moving to another parent clears the cache"
        );
    }

    #[test]
    fn the_cache_keeps_the_most_recent_sixteen() {
        let style = ComputedStyle::initial();
        let mut cache = SharingCache::new();
        for key in 0..40_u32 {
            cache.insert(1, None, key, &div(), Rc::clone(&style));
        }
        assert_eq!(cache.len(), SHARING_CACHE_SIZE);
        // The oldest keys were dropped, so they no longer resolve to a hit.
        let elements: Vec<Element> = (0..40).map(|_| div()).collect();
        let only_old = |k: u32| (k < 24).then(|| elements[k as usize].clone());
        assert!(cache.lookup(1, None, &div(), only_old).is_none());
    }
}
