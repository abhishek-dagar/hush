//! The user agent style sheet, `ua.css` (spec 0004, child 0003, Sources).

use std::rc::Rc;

use crate::sheet::{Origin, Stylesheet, parse_stylesheet};

const UA_CSS: &str = include_str!("ua.css");

/// Parses the user agent sheet. The content process calls this once at
/// start and hands the `Rc` to every page, so it is never parsed twice and
/// no global state holds it (spec 0004, Key invariants).
pub fn ua_stylesheet() -> Rc<Stylesheet> {
    Rc::new(parse_stylesheet(UA_CSS, Origin::Ua, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_ua_rule_parses() {
        // A rule dropped by the parser would silently lose a default style.
        let blocks = UA_CSS.matches('{').count();
        assert_eq!(ua_stylesheet().len(), blocks);
    }
}
