//! The per document rule store: rules bucketed by their rightmost compound,
//! media query results, and the cascade order (spec 0004, child 0003,
//! Stylist and Cascade, AC-18, AC-19).

use std::collections::HashMap;
use std::rc::Rc;

use base::Au;
use base::atoms::{Atom, LocalName};

use crate::cascade::{CascadeInputs, compute};
use crate::hints::presentational_hints;
use crate::invalidation::InvalidationMap;
use crate::media::Device;
use crate::properties::Declaration;
use crate::selector::{MatchContext, PseudoElement, Simple};
use crate::sheet::{Origin, Rule, Stylesheet};
use crate::{ComputedStyle, FontFaceRule, MatchElement};

/// One selector of one rule, filed in a bucket.
#[derive(Debug, Clone, Copy)]
struct Entry {
    /// The rule's index in `Stylist::rules`, which is also its source order.
    rule: u32,
    /// Which selector of the rule's list.
    selector: u16,
    specificity: u32,
}

#[derive(Debug, Default)]
struct Buckets {
    ids: HashMap<Atom, Vec<Entry>>,
    classes: HashMap<Atom, Vec<Entry>>,
    tags: HashMap<LocalName, Vec<Entry>>,
    universal: Vec<Entry>,
}

#[derive(Debug)]
struct StylistRule {
    origin: Origin,
    rule: Rule,
    /// The sheet's own `media` attribute, if any.
    sheet_media: Option<Rc<crate::MediaList>>,
}

impl StylistRule {
    fn applies(&self, device: &Device) -> bool {
        self.sheet_media
            .iter()
            .chain(self.rule.media.iter())
            .all(|media| media.evaluate(device))
    }
}

/// One `@font-face` rule and its sheet's `media` attribute.
#[derive(Debug)]
struct StylistFontFace {
    rule: FontFaceRule,
    sheet_media: Option<Rc<crate::MediaList>>,
}

/// A document's rules, ready to match and cascade.
#[derive(Debug)]
pub struct Stylist {
    rules: Vec<StylistRule>,
    font_faces: Vec<StylistFontFace>,
    /// Which rules apply on the current device.
    active: Vec<bool>,
    buckets: Buckets,
    invalidation: Rc<InvalidationMap>,
    device: Device,
    uses_viewport_units: bool,
}

impl Stylist {
    /// A stylist over `sheets`, in cascade source order (the UA sheet
    /// first, then the document's in tree order).
    pub fn new(sheets: &[Rc<Stylesheet>], device: Device) -> Self {
        let mut rules = Vec::new();
        let mut invalidation = InvalidationMap::default();
        for sheet in sheets {
            for rule in &sheet.rules {
                for selector in rule.selectors.iter() {
                    invalidation.add(selector);
                }
                rules.push(StylistRule {
                    origin: sheet.origin,
                    rule: rule.clone(),
                    sheet_media: sheet.media.clone(),
                });
            }
        }
        let font_faces = sheets
            .iter()
            .flat_map(|sheet| {
                sheet.font_faces.iter().map(|rule| StylistFontFace {
                    rule: rule.clone(),
                    sheet_media: sheet.media.clone(),
                })
            })
            .collect();
        let active = rules.iter().map(|rule| rule.applies(&device)).collect();
        let mut stylist = Self {
            rules,
            font_faces,
            active,
            buckets: Buckets::default(),
            invalidation: Rc::new(invalidation),
            device,
            uses_viewport_units: sheets.iter().any(|sheet| sheet.uses_viewport_units),
        };
        stylist.fill_buckets();
        stylist
    }

    fn fill_buckets(&mut self) {
        let mut buckets = Buckets::default();
        for (index, rule) in self.rules.iter().enumerate() {
            if !self.active[index] {
                continue;
            }
            for (selector_index, selector) in rule.rule.selectors.iter().enumerate() {
                let entry = Entry {
                    rule: index as u32,
                    selector: selector_index as u16,
                    specificity: selector.specificity(),
                };
                let simple = &selector.subject().0;
                let id = simple.iter().find_map(|s| match s {
                    Simple::Id(id) => Some(id),
                    _ => None,
                });
                let class = simple.iter().find_map(|s| match s {
                    Simple::Class(class) => Some(class),
                    _ => None,
                });
                let tag = simple.iter().find_map(|s| match s {
                    Simple::Type { lower, .. } => Some(lower),
                    _ => None,
                });
                if let Some(id) = id {
                    buckets.ids.entry(id.clone()).or_default().push(entry);
                } else if let Some(class) = class {
                    buckets
                        .classes
                        .entry(class.clone())
                        .or_default()
                        .push(entry);
                } else if let Some(tag) = tag {
                    buckets.tags.entry(tag.clone()).or_default().push(entry);
                } else {
                    buckets.universal.push(entry);
                }
            }
        }
        self.buckets = buckets;
    }

    /// The selector dependencies of every rule, for restyle hints.
    pub fn invalidation_map(&self) -> Rc<InvalidationMap> {
        Rc::clone(&self.invalidation)
    }

    /// The `@font-face` rules whose media match the current device, in
    /// cascade order, so a later rule for the same face wins (spec 0006,
    /// AC-10).
    pub fn font_faces(&self) -> Vec<&FontFaceRule> {
        self.font_faces
            .iter()
            .filter(|face| {
                face.sheet_media
                    .iter()
                    .chain(face.rule.media.iter())
                    .all(|media| media.evaluate(&self.device))
            })
            .map(|face| &face.rule)
            .collect()
    }

    /// The device media queries are evaluated against.
    pub fn device(&self) -> Device {
        self.device
    }

    /// Moves to a new viewport. Returns whether every style must be
    /// computed again: a media query result flipped (the buckets are
    /// rebuilt) or some value uses viewport units (AC-19).
    pub fn set_device(&mut self, device: Device) -> bool {
        if device == self.device {
            return false;
        }
        self.device = device;
        let active: Vec<bool> = self
            .rules
            .iter()
            .map(|rule| rule.applies(&device))
            .collect();
        let flipped = active != self.active;
        if flipped {
            self.active = active;
            self.fill_buckets();
        }
        flipped || self.uses_viewport_units
    }

    /// Computes `element`'s style: matches its candidate rules, orders their
    /// declarations by the cascade (AC-18), then computes values against
    /// `parent`'s style. `style_attribute` holds its `style` declarations,
    /// which sit above every selector. `root_font_size` is for `rem`.
    pub fn cascade<E: MatchElement + Clone>(
        &self,
        element: &E,
        parent: Option<&ComputedStyle>,
        style_attribute: &[Declaration],
        root_font_size: Au,
        context: &mut MatchContext<E>,
    ) -> ComputedStyle {
        let matched = self.matching(element, None, context);
        let presentational = presentational_hints(element);
        self.cascade_matched(
            matched,
            parent,
            style_attribute,
            root_font_size,
            &presentational,
        )
    }

    /// Computes the `::placeholder` style of the `<input>` `element`, which
    /// inherits from `input_style`, the input's own (spec 0006, AC-2,
    /// AC-17). Only rules whose selector ends in `::placeholder` apply.
    pub fn compute_placeholder<E: MatchElement + Clone>(
        &self,
        element: &E,
        input_style: &ComputedStyle,
        root_font_size: Au,
    ) -> Rc<ComputedStyle> {
        let mut context = MatchContext::default();
        let matched = self.matching(element, Some(PseudoElement::Placeholder), &mut context);
        Rc::new(self.cascade_matched(matched, Some(input_style), &[], root_font_size, &[]))
    }

    fn cascade_matched(
        &self,
        mut matched: Vec<Entry>,
        parent: Option<&ComputedStyle>,
        style_attribute: &[Declaration],
        root_font_size: Au,
        presentational: &[Declaration],
    ) -> ComputedStyle {
        matched.sort_by_key(|entry| {
            let origin = match self.rules[entry.rule as usize].origin {
                Origin::Ua => 0,
                Origin::Author => 1,
            };
            (origin, entry.specificity, entry.rule)
        });
        let block = |entry: &Entry| self.rules[entry.rule as usize].rule.declarations.iter();
        let from = |origin: Origin| {
            matched
                .iter()
                .filter(move |entry| self.rules[entry.rule as usize].origin == origin)
                .flat_map(block)
        };
        let normal = |d: &&Declaration| !d.important;
        let important = |d: &&Declaration| d.important;

        // Lowest priority first: UA normal, then presentational hints
        // (author origin, specificity 0, before every author rule), then
        // author normal, the style attribute, author important, the style
        // attribute's important, UA important.
        let ordered = from(Origin::Ua)
            .filter(normal)
            .chain(presentational.iter())
            .chain(from(Origin::Author).filter(normal))
            .chain(style_attribute.iter().filter(normal))
            .chain(from(Origin::Author).filter(important))
            .chain(style_attribute.iter().filter(important))
            .chain(from(Origin::Ua).filter(important));
        compute(
            ordered,
            &CascadeInputs {
                parent,
                root_font_size,
                device: &self.device,
            },
        )
    }

    /// Every selector from the element's buckets that matches it and styles
    /// `pseudo` (the element itself when `None`).
    fn matching<E: MatchElement + Clone>(
        &self,
        element: &E,
        pseudo: Option<PseudoElement>,
        context: &mut MatchContext<E>,
    ) -> Vec<Entry> {
        let buckets = &self.buckets;
        let mut candidates: Vec<&Entry> = Vec::new();
        if let Some(entries) = element.id().and_then(|id| buckets.ids.get(id)) {
            candidates.extend(entries);
        }
        for class in element.classes() {
            if let Some(entries) = buckets.classes.get(class) {
                candidates.extend(entries);
            }
        }
        let local = &element.name().local;
        let tag = if local.bytes().any(|b| b.is_ascii_uppercase()) {
            buckets
                .tags
                .get(&LocalName::from(local.to_ascii_lowercase()))
        } else {
            buckets.tags.get(local)
        };
        candidates.extend(tag.into_iter().flatten());
        candidates.extend(&buckets.universal);

        candidates
            .into_iter()
            .filter(|entry| {
                let selector =
                    &self.rules[entry.rule as usize].rule.selectors[entry.selector as usize];
                selector.pseudo_element() == pseudo && selector.matches_compounds(element, context)
            })
            .copied()
            .collect()
    }
}
