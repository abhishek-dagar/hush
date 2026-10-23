//! Focus and the `:disabled` state of form controls (spec 0006, Data
//! model, Focus; AC-13, AC-16).
//!
//! The document keeps at most one focused element and pins it while it
//! holds focus. Whether `:focus` and `:focus-visible` show on it depends on
//! the window and on how focus moved, which only the page knows, so the
//! page sets those bits; the document only takes them off the element that
//! loses focus. `DISABLED` is kept here, from the `disabled` attributes of
//! the controls and their `fieldset` ancestors, on every change that can
//! move it.

use base::atoms::QualName;
use base::{local_name, ns};

use std::panic::Location;

use crate::node::{ElementState, NodeFlags};
use crate::{Document, DomError, NodeId};

impl Document {
    /// The focused element, if any (spec 0006, Interface surface).
    pub fn focused(&self) -> Option<NodeId> {
        self.focused
    }

    /// Moves focus to the connected element `node`, or clears it with
    /// `None`. Pins the new element and unpins the old one, which loses
    /// `:focus` and `:focus-visible`. A node that is not a connected element
    /// clears focus.
    pub fn set_focus(&mut self, node: Option<NodeId>) {
        let node = node.filter(|&id| self.element(id).is_some() && self.is_connected(id));
        if node == self.focused {
            return;
        }
        if let Some(old) = self.focused.take() {
            let lost = ElementState::FOCUS | ElementState::FOCUS_VISIBLE;
            self.set_state(old, ElementState::empty(), lost);
            let _ = self.unpin(old);
        }
        if let Some(new) = node
            && self.pin(new).is_ok()
        {
            self.focused = Some(new);
        }
    }

    /// Whether `id` is a `button`, `input`, `select` or `textarea`, the
    /// controls `disabled` applies to.
    pub fn is_form_control(&self, id: NodeId) -> bool {
        self.element(id).is_some_and(|element| {
            let name = element.name();
            name.ns == ns!("http://www.w3.org/1999/xhtml")
                && matches!(
                    name.local,
                    local_name!("button")
                        | local_name!("input")
                        | local_name!("select")
                        | local_name!("textarea")
                )
        })
    }

    /// A text input's current value (spec 0008, AC-16): what the user
    /// typed once they edited it, else its `value` attribute, with line
    /// breaks dropped (HTML value sanitization). `None` for anything but
    /// an `<input>`.
    pub fn input_value(&self, id: NodeId) -> Option<String> {
        if !self.is_html(id, local_name!("input")) {
            return None;
        }
        let element = self.element(id)?;
        let value = match &element.value {
            Some(value) => value.as_str(),
            None => element
                .attribute(&QualName::attribute(local_name!("value")))
                .unwrap_or_default(),
        };
        Some(value.chars().filter(|&c| c != '\n' && c != '\r').collect())
    }

    /// Sets a text input's current value, leaving its `value` attribute
    /// as it is (spec 0008, AC-16). It shows on the next frame.
    #[track_caller]
    pub fn set_input_value(&mut self, id: NodeId, value: String) -> Result<(), DomError> {
        let element = self
            .live_mut(id, Location::caller())?
            .as_element_mut()
            .ok_or(DomError::InvalidState)?;
        if element.value.as_ref() == Some(&value) {
            return Ok(());
        }
        element.value = Some(value);
        // Whether the placeholder shows may change with it.
        self.mark_style(id, NodeFlags::NEEDS_STYLE);
        self.mark_box_rebuild(id);
        self.needs_frame = true;
        Ok(())
    }

    /// Whether `id` is an `<input>` that takes text: one of the text like
    /// types, or a missing or unknown `type`, which means `text` (spec
    /// 0006, AC-14, AC-17).
    pub fn is_text_field(&self, id: NodeId) -> bool {
        if !self.is_html(id, local_name!("input")) {
            return false;
        }
        let kind = self
            .element(id)
            .and_then(|element| element.attribute(&QualName::attribute(local_name!("type"))))
            .unwrap_or_default()
            .to_ascii_lowercase();
        !matches!(
            kind.as_str(),
            "hidden"
                | "checkbox"
                | "radio"
                | "file"
                | "submit"
                | "reset"
                | "button"
                | "image"
                | "color"
                | "range"
                | "date"
                | "datetime-local"
                | "month"
                | "time"
                | "week"
        )
    }

    /// Sets or clears `DISABLED` on every form control at or below `root`,
    /// and takes focus from a control that just became disabled (AC-13,
    /// AC-16).
    pub(crate) fn refresh_disabled(&mut self, root: NodeId) {
        let controls: Vec<NodeId> = std::iter::once(root)
            .chain(self.descendants(root))
            .filter(|&id| self.is_form_control(id))
            .collect();
        for id in controls {
            if self.is_disabled_control(id) {
                self.set_state(id, ElementState::DISABLED, ElementState::empty());
                if self.focused == Some(id) {
                    self.set_focus(None);
                }
            } else {
                self.set_state(id, ElementState::empty(), ElementState::DISABLED);
            }
        }
    }

    /// The HTML rule: the control has a `disabled` attribute, or sits in a
    /// `fieldset[disabled]` outside that fieldset's first `legend`.
    fn is_disabled_control(&self, id: NodeId) -> bool {
        if self.has_disabled_attribute(id) {
            return true;
        }
        let mut child = id;
        let mut current = self.nodes.get(id).and_then(|n| n.parent);
        while let Some(ancestor) = current {
            if self.is_html(ancestor, local_name!("fieldset"))
                && self.has_disabled_attribute(ancestor)
                && self.first_legend(ancestor) != Some(child)
            {
                return true;
            }
            child = ancestor;
            current = self.nodes.get(ancestor).and_then(|n| n.parent);
        }
        false
    }

    fn has_disabled_attribute(&self, id: NodeId) -> bool {
        self.element(id).is_some_and(|element| {
            element
                .attribute(&QualName::attribute(local_name!("disabled")))
                .is_some()
        })
    }

    /// The fieldset's first `legend` child, if any.
    fn first_legend(&self, fieldset: NodeId) -> Option<NodeId> {
        self.children(fieldset)
            .find(|&child| self.is_html(child, local_name!("legend")))
    }

    /// Whether `id` is the HTML element `local`.
    pub fn is_html(&self, id: NodeId, local: base::atoms::LocalName) -> bool {
        self.element(id).is_some_and(|element| {
            let name = element.name();
            name.ns == ns!("http://www.w3.org/1999/xhtml") && name.local == local
        })
    }
}
