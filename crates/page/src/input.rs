//! Pointer and keyboard input, and the states, focus and activation it
//! drives (spec 0006, AC-12 to AC-16, AC-18, AC-21).
//!
//! The browser forwards pointer events in page CSS px. Moves can come much
//! faster than frames, so the page keeps only the latest one and hit tests
//! it once, at the next `BeginFrame`, against the last layout; a press or
//! release first applies the move waiting, so it targets what is under the
//! pointer now. The element under the pointer and every flat tree ancestor
//! of it get `:hover`, the pressed one and its ancestors `:active`; only the
//! elements that join or leave a chain change state, through
//! `Document::set_state`, which restyles only what a rule can reach
//! (AC-19).
//!
//! Focus moves on a primary press and with Tab and Shift+Tab, and past
//! either end of the Tab order it leaves for the browser. `:focus-visible`
//! follows how focus last moved. Activation follows the DOM click rule. The
//! per role keyboard model is not here: keys the page does not use go back
//! to the caller as `unused_key`. Nothing here needs a script engine.

use css::Overflow;
use display_list::{PointF, ScrollId};
use dom::{Document, ElementState, FrameId, NodeId, NodeRef};
use ipc::{CursorKind, FocusDirection, InputEvent, Key, KeyScroll, Modifiers, PointerButton};
use layout::HitTestResult;

use crate::Page;
use crate::editor::{self, EditOp, Motion};
use crate::links::link_at;

/// What one input event led to (spec 0006, Interface surface).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InputOutcome {
    /// The element a press and release, `Enter` or `Space` activated.
    pub activated: Option<NodeRef>,
    /// Tab went past this end of the Tab order: focus left the page.
    pub focus_left: Option<FocusDirection>,
    /// The pointer shape, when it changed.
    pub cursor: Option<CursorKind>,
    /// A key down the page did not use: the key and the frame (or the
    /// viewport when `None`) the browser should scroll with it (spec 0009,
    /// child 0002, AC-9).
    pub scroll: Option<(KeyScroll, Option<ScrollId>)>,
    /// A key down the page did not use, for the caller's keyboard model.
    pub unused_key: Option<Key>,
    /// The answer to a `Copy` or `Cut`: its request id and the selected
    /// text, for `ClipboardWrite` (spec 0008, AC-15).
    pub copied: Option<(u32, String)>,
}

/// How focus last moved, for `:focus-visible` (AC-14).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Modality {
    /// A pointer press.
    #[default]
    Pointer,
    /// Tab, Shift+Tab, `FocusEnter`, or any key while focused.
    Keyboard,
}

/// What holds `:active` down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PressedBy {
    /// The primary pointer button.
    Pointer,
    /// The space bar, on a focused button.
    Space,
}

/// A press in progress (spec 0006, State transitions, Pointer press).
#[derive(Debug, Clone, Copy)]
struct Pressed {
    /// The press target, for activation on release.
    node: NodeId,
    by: PressedBy,
}

/// One page's input state (spec 0006, Data model, `InputState`), in the
/// main frame's document.
#[derive(Debug)]
pub(crate) struct InputState {
    /// The last pointer position over the page, page CSS px.
    pointer: Option<PointF>,
    /// A move not hit tested yet; applied at the next frame.
    pending_move: Option<PointF>,
    /// The hovered element and its flat tree ancestors, innermost first.
    hovered: Vec<NodeId>,
    /// The last hit landed on selectable text, for `cursor: auto`.
    over_text: bool,
    /// The press holding `:active`, if any.
    pressed: Option<Pressed>,
    /// The link under a middle button press, which its release over the
    /// same link activates (spec 0008, AC-17).
    middle_link: Option<NodeId>,
    /// The elements holding `:active`, innermost first.
    active: Vec<NodeId>,
    modality: Modality,
    /// The focused element should show its focus ring. Kept while the
    /// window is in the background, when the ring is hidden.
    focus_visible: bool,
    window_focused: bool,
    /// The pointer shape last reported.
    cursor: CursorKind,
    /// `cursor` changed since the caller last took it.
    cursor_changed: bool,
    /// The last element pressed with the pointer, the fallback scroll target
    /// when nothing focused can scroll (spec 0009, child 0002, Value
    /// sourcing, Key scroll target). Cleared when its node disconnects.
    scroll_origin: Option<NodeRef>,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            pointer: None,
            pending_move: None,
            hovered: Vec::new(),
            over_text: false,
            pressed: None,
            middle_link: None,
            active: Vec::new(),
            modality: Modality::default(),
            focus_visible: false,
            window_focused: true,
            cursor: CursorKind::default(),
            cursor_changed: false,
            scroll_origin: None,
        }
    }
}

impl InputState {
    /// Whether the window has OS focus.
    pub(crate) fn window_focused(&self) -> bool {
        self.window_focused
    }

    /// The innermost hovered node, if any.
    pub(crate) fn hovered(&self) -> Option<NodeId> {
        self.hovered.first().copied()
    }

    /// The pointer moved to `point`.
    pub(crate) fn pointer_move(&mut self, point: PointF) {
        self.pointer = Some(point);
        self.pending_move = Some(point);
    }

    /// Whether a move waits for the next frame.
    pub(crate) fn has_pending_move(&self) -> bool {
        self.pending_move.is_some()
    }

    /// Takes the move waiting for this frame, if any.
    pub(crate) fn take_pending_move(&mut self) -> Option<PointF> {
        self.pending_move.take()
    }

    /// The layout changed: what is under a still pointer may have too, so
    /// it is hit tested again at the next frame (AC-12).
    pub(crate) fn layout_changed(&mut self) {
        if self.pointer.is_some() {
            self.pending_move = self.pointer;
        }
    }

    /// A new document replaced the old one: its nodes are gone, so the
    /// chains are dropped without touching them, and the pointer, if still
    /// over the page, is hit tested again against the new document.
    pub(crate) fn document_replaced(&mut self) {
        self.hovered.clear();
        self.active.clear();
        self.pressed = None;
        self.focus_visible = false;
        self.scroll_origin = None;
        self.pending_move = self.pointer;
    }

    /// The pointer shape, if it changed since the last call.
    pub(crate) fn take_cursor_change(&mut self) -> Option<CursorKind> {
        std::mem::take(&mut self.cursor_changed).then_some(self.cursor)
    }

    /// Hovers what `hit` landed on (`None` over no box) and its flat tree
    /// ancestors, and unhovers the elements no longer under the pointer.
    pub(crate) fn hover(&mut self, doc: &mut Document, hit: Option<&HitTestResult>) {
        self.over_text = hit.is_some_and(|hit| hit.selectable_text);
        let chain = hit.map(|hit| chain(doc, hit.node)).unwrap_or_default();
        set_chain(doc, &mut self.hovered, chain, ElementState::HOVER);
    }

    /// Nothing is hovered or pressed any more: the pointer left the page,
    /// or the window went to the background.
    fn release_all(&mut self, doc: &mut Document) {
        self.pending_move = None;
        self.over_text = false;
        set_chain(doc, &mut self.hovered, Vec::new(), ElementState::HOVER);
        self.pressed = None;
        self.middle_link = None;
        set_chain(doc, &mut self.active, Vec::new(), ElementState::ACTIVE);
    }

    /// Puts `:active` on `node` and its flat tree ancestors.
    fn activate_chain(&mut self, doc: &mut Document, node: Option<NodeId>) {
        let chain = node.map(|node| chain(doc, node)).unwrap_or_default();
        set_chain(doc, &mut self.active, chain, ElementState::ACTIVE);
    }

    fn set_cursor(&mut self, cursor: CursorKind) {
        if cursor != self.cursor {
            self.cursor = cursor;
            self.cursor_changed = true;
        }
    }
}

/// Moves `bit` from the elements of `old` to those of `new`, touching only
/// the elements in one and not the other.
fn set_chain(doc: &mut Document, old: &mut Vec<NodeId>, new: Vec<NodeId>, bit: ElementState) {
    for &node in old.iter().filter(|node| !new.contains(node)) {
        doc.set_state(node, ElementState::empty(), bit);
    }
    for &node in new.iter().filter(|node| !old.contains(node)) {
        doc.set_state(node, bit, ElementState::empty());
    }
    *old = new;
}

/// The elements a state on `node` marks: the node, or its parent element
/// for a text node, then every flat tree ancestor that is an element
/// (spec 0006, Value sourcing, Hover chain).
fn chain(doc: &Document, node: NodeId) -> Vec<NodeId> {
    inclusive_ancestors(doc, node)
        .filter(|&id| is_element(doc, id))
        .collect()
}

fn inclusive_ancestors(doc: &Document, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    std::iter::successors(Some(node), |&id| doc.flat_parent(id))
}

fn is_element(doc: &Document, id: NodeId) -> bool {
    doc.is_alive(id) && doc.node(id).is_some_and(|n| n.as_element().is_some())
}

fn state(doc: &Document, id: NodeId) -> ElementState {
    if !doc.is_alive(id) {
        return ElementState::empty();
    }
    doc.node(id)
        .and_then(|n| n.as_element())
        .map_or(ElementState::empty(), |e| e.state())
}

fn is_disabled(doc: &Document, id: NodeId) -> bool {
    state(doc, id).contains(ElementState::DISABLED)
}

fn attribute(doc: &Document, id: NodeId, name: base::atoms::LocalName) -> Option<&str> {
    if !doc.is_alive(id) {
        return None;
    }
    doc.node(id)?
        .as_element()?
        .attribute(&base::atoms::QualName::attribute(name))
}

/// The `tabindex`, parsed with the HTML rules for integers; an invalid
/// value is as if absent (spec 0006, Value sourcing, Tab order).
fn tab_index(doc: &Document, id: NodeId) -> Option<i32> {
    let value = attribute(doc, id, base::local_name!("tabindex"))?;
    let value = value.trim_start_matches(['\t', '\n', '\u{c}', '\r', ' ']);
    let (negative, digits) = match value.as_bytes().first() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    let magnitude: i64 = digits[..end].parse().ok()?;
    let signed = if negative { -magnitude } else { magnitude };
    i32::try_from(signed).ok()
}

/// Whether the element can take focus at all (AC-13): a link, a control or
/// a valid `tabindex`, not disabled, with a box, and not `visibility:
/// hidden`.
fn is_focusable(doc: &Document, id: NodeId) -> bool {
    use base::local_name;
    if !is_element(doc, id) || is_disabled(doc, id) {
        return false;
    }
    let kind_focusable = (doc.is_html(id, local_name!("a"))
        && attribute(doc, id, local_name!("href")).is_some())
        || doc.is_html(id, local_name!("button"))
        || (doc.is_html(id, local_name!("input"))
            && !attribute(doc, id, local_name!("type"))
                .is_some_and(|t| t.eq_ignore_ascii_case("hidden")))
        || doc.is_html(id, local_name!("select"))
        || doc.is_html(id, local_name!("textarea"))
        || tab_index(doc, id).is_some();
    let Some(node) = doc.node(id) else {
        return false;
    };
    let visible = node
        .as_element()
        .and_then(|e| e.style())
        .is_some_and(|style| style.inherited_box.visibility == css::Visibility::Visible);
    kind_focusable && node.layout_box().is_some() && visible
}

/// Whether Enter activates the element: a `button` or an `a[href]` (AC-15).
fn activates_on_enter(doc: &Document, id: NodeId) -> bool {
    use base::local_name;
    doc.is_html(id, local_name!("button"))
        || (doc.is_html(id, local_name!("a")) && attribute(doc, id, local_name!("href")).is_some())
}

/// Every element in flat tree order, with whether it is focusable and its
/// `tabindex`.
fn focusable_in_tree_order(doc: &Document) -> Vec<(NodeId, bool, Option<i32>)> {
    let mut found = Vec::new();
    let mut stack = vec![doc.root()];
    while let Some(id) = stack.pop() {
        if is_element(doc, id) {
            found.push((id, is_focusable(doc, id), tab_index(doc, id)));
        }
        let mut children: Vec<NodeId> = doc.flat_children(id).collect();
        children.reverse();
        stack.extend(children);
    }
    found
}

/// The sequential focus order (AC-13): `tabindex` above 0 in ascending
/// order, then 0 or none in tree order. Each entry carries its tree index.
fn tab_order(tree: &[(NodeId, bool, Option<i32>)]) -> Vec<(usize, NodeId, i32)> {
    let mut order: Vec<(usize, NodeId, i32)> = tree
        .iter()
        .enumerate()
        .filter(|(_, (_, focusable, index))| *focusable && index.is_none_or(|i| i >= 0))
        .map(|(position, &(id, _, index))| (position, id, index.unwrap_or(0)))
        .collect();
    // Stable, so equal indexes keep tree order; 0 sorts after every
    // positive index.
    order.sort_by_key(|&(_, _, index)| if index == 0 { i32::MAX } else { index });
    order
}

/// Where Tab or Shift+Tab goes from `from` (AC-13): the next or previous
/// element in the Tab order, or `None` past either end. From nothing it
/// starts at the first or last. From an element outside the order (a
/// `tabindex="-1"` one focused by click) it goes on from that element's
/// place in tree order.
fn next_in_tab_order(
    doc: &Document,
    from: Option<NodeId>,
    direction: FocusDirection,
) -> Option<NodeId> {
    let tree = focusable_in_tree_order(doc);
    let order = tab_order(&tree);
    let forward = direction == FocusDirection::Forward;
    let Some(from) = from else {
        let end = if forward { order.first() } else { order.last() };
        return end.map(|&(_, id, _)| id);
    };
    if let Some(at) = order.iter().position(|&(_, id, _)| id == from) {
        let next = if forward {
            order.get(at + 1)
        } else {
            at.checked_sub(1).and_then(|before| order.get(before))
        };
        return next.map(|&(_, id, _)| id);
    }
    let position = tree.iter().position(|&(id, _, _)| id == from)?;
    let mut in_tree_order = order.iter().filter(|&&(_, _, index)| index == 0);
    if forward {
        in_tree_order
            .clone()
            .find(|&&(at, _, _)| at > position)
            .map(|&(_, id, _)| id)
    } else {
        in_tree_order
            .rfind(|&&(at, _, _)| at < position)
            .or_else(|| order.iter().rfind(|&&(_, _, index)| index > 0))
            .map(|&(_, id, _)| id)
    }
}

/// The nearest common inclusive flat tree ancestor of `a` and `b`.
fn common_ancestor(doc: &Document, a: NodeId, b: NodeId) -> Option<NodeId> {
    if !doc.is_alive(a) || !doc.is_alive(b) {
        return None;
    }
    let of_a: Vec<NodeId> = inclusive_ancestors(doc, a).collect();
    inclusive_ancestors(doc, b).find(|id| of_a.contains(id))
}

/// Which scroll key a key press means, or `None` when it is not a scroll
/// key (spec 0009, child 0002, AC-9). Moved here from
/// `embed::scrolling::key_scroll_for`.
pub(crate) fn scroll_key(key: &Key, modifiers: Modifiers) -> Option<KeyScroll> {
    // Cmd+Up and Cmd+Down are Home and End on macOS.
    if cfg!(target_os = "macos")
        && modifiers.meta
        && !modifiers.ctrl
        && !modifiers.alt
        && !modifiers.shift
    {
        return match key {
            Key::ArrowUp => Some(KeyScroll::Home),
            Key::ArrowDown => Some(KeyScroll::End),
            _ => None,
        };
    }
    // Other chords belong to the app and the OS.
    if modifiers.ctrl || modifiers.alt || modifiers.meta {
        return None;
    }
    let shift = modifiers.shift;
    match key {
        Key::ArrowUp => Some(KeyScroll::LineUp),
        Key::ArrowDown => Some(KeyScroll::LineDown),
        Key::ArrowLeft => Some(KeyScroll::LineLeft),
        Key::ArrowRight => Some(KeyScroll::LineRight),
        Key::Space if shift => Some(KeyScroll::PageUp),
        Key::Space | Key::PageDown => Some(KeyScroll::PageDown),
        Key::PageUp => Some(KeyScroll::PageUp),
        Key::Home => Some(KeyScroll::Home),
        Key::End => Some(KeyScroll::End),
        _ => None,
    }
}

impl Page {
    /// Takes one input event from the browser (spec 0006, AC-12 to AC-16,
    /// AC-21). A move is kept until the next frame; everything else acts at
    /// once. Works the same with or without a script engine.
    pub fn input(&mut self, event: InputEvent) -> InputOutcome {
        let mut outcome = InputOutcome::default();
        match event {
            InputEvent::PointerMove { x, y } => self.input.pointer_move(PointF { x, y }),
            InputEvent::PointerLeave => {
                self.input.pointer = None;
                self.input.release_all(&mut self.main_frame.document);
            }
            InputEvent::PointerDown { x, y, button } => {
                let hit = self.flush_move(PointF { x, y });
                match button {
                    PointerButton::Primary => self.press(hit),
                    PointerButton::Middle => {
                        let doc = &self.main_frame.document;
                        self.input.middle_link = hit.and_then(|hit| link_at(doc, hit.node));
                    }
                    PointerButton::Secondary => {}
                }
            }
            InputEvent::PointerUp { x, y, button } => {
                let hit = self.flush_move(PointF { x, y });
                match button {
                    PointerButton::Primary => {
                        outcome.activated = self.release(hit);
                        let clicked = outcome.activated.map(|node| node.node);
                        if let Some(node) = clicked {
                            self.toggle_checkbox(node);
                        }
                        self.image_click(clicked, x, y);
                    }
                    // A middle click on a link follows it, for now in the
                    // same page; tabs make it open one (spec 0008, AC-17).
                    PointerButton::Middle => {
                        let pressed = self.input.middle_link.take();
                        let doc = &self.main_frame.document;
                        let released = hit.and_then(|hit| link_at(doc, hit.node));
                        if let Some(link) = released.filter(|&link| pressed == Some(link)) {
                            outcome.activated = self.activation(link);
                        }
                    }
                    PointerButton::Secondary => {}
                }
            }
            InputEvent::Key {
                key,
                modifiers,
                pressed,
                repeat,
            } => self.key(key, modifiers, pressed, repeat, &mut outcome),
            InputEvent::FocusEnter { direction } => {
                let doc = &self.main_frame.document;
                if let Some(first) = next_in_tab_order(doc, None, direction) {
                    self.input.modality = Modality::Keyboard;
                    self.focus(Some(first));
                }
            }
            InputEvent::WindowFocus { focused } => {
                self.input.window_focused = focused;
                if !focused {
                    self.input.pointer = None;
                    self.input.release_all(&mut self.main_frame.document);
                }
                self.show_focus();
                // The caret shows only in a focused window.
                self.needs_update = true;
            }
            InputEvent::Text { text } => {
                self.edit(EditOp::Insert(&text));
            }
            InputEvent::SelectAll => {
                if self.edit(EditOp::SelectAll).is_none()
                    && let Some(id) = self.editing()
                {
                    // The whole value is selected: show it from its start,
                    // so a long URL shows its host first (AC-12).
                    self.caret_moved = None;
                    self.show_field_start(id);
                }
            }
            InputEvent::Copy { request } => {
                outcome.copied = self.edit(EditOp::Copy).map(|text| (request, text));
            }
            InputEvent::Cut { request } => {
                outcome.copied = self.edit(EditOp::Cut).map(|text| (request, text));
            }
            InputEvent::Paste { text } => {
                self.edit(EditOp::Paste(&text));
            }
        }
        self.json_sync_toggle();
        self.update_cursor();
        outcome.cursor = self.input.take_cursor_change();
        outcome
    }

    /// The focused element, if any.
    pub fn focused(&self) -> Option<NodeRef> {
        self.main_frame.document.focused().map(|node| NodeRef {
            frame: FrameId::MAIN,
            node,
        })
    }

    /// Moves focus to `node` as a key press would, so its ring shows, or
    /// clears focus with `None`. The browser's chrome uses it for roving
    /// focus inside a composite and for focus coming back from a popup
    /// (spec 0003, keyboard model). A node that cannot take focus is
    /// ignored; returns whether focus is now on `node`.
    pub fn move_focus(&mut self, node: Option<NodeId>) -> bool {
        let doc = &self.main_frame.document;
        if node.is_some_and(|id| !doc.is_alive(id) || !is_focusable(doc, id)) {
            return false;
        }
        self.input.modality = Modality::Keyboard;
        self.focus(node);
        true
    }

    /// The pointer shape, if it changed since the last call or the last
    /// [`InputOutcome`]: send it to the browser with `SetCursor` (AC-18).
    pub fn take_cursor_change(&mut self) -> Option<CursorKind> {
        self.input.take_cursor_change()
    }

    /// Hit tests the pointer move waiting for this frame against the last
    /// layout, and moves `:hover` to what is under it.
    pub(crate) fn apply_pending_move(&mut self) {
        let Some(point) = self.input.take_pending_move() else {
            return;
        };
        let hit = self.hit(point);
        self.input
            .hover(&mut self.main_frame.document, hit.as_ref());
        self.update_cursor();
    }

    /// After a frame's style and layout: a focused element that lost its
    /// box or became hidden loses focus (AC-13), a still pointer is hit
    /// tested again if the layout moved (AC-12), and the cursor follows the
    /// new style (AC-18).
    pub(crate) fn after_layout(&mut self, laid_out: bool) {
        let doc = &mut self.main_frame.document;
        if doc.focused().is_some_and(|id| !is_focusable(doc, id)) {
            doc.set_focus(None);
        }
        if laid_out {
            self.input.layout_changed();
        }
        self.update_cursor();
    }

    /// Applies the pointer's latest position at once, so a press or
    /// release targets what is under it now, and returns that hit.
    fn flush_move(&mut self, point: PointF) -> Option<HitTestResult> {
        self.input.pointer = Some(point);
        self.input.pending_move = None;
        let hit = self.hit(point);
        self.input
            .hover(&mut self.main_frame.document, hit.as_ref());
        hit
    }

    /// A primary press (AC-12, AC-13, AC-14): `:active` on the target, or
    /// its nearest ancestor that is not disabled, and focus on the nearest
    /// focusable inclusive ancestor, or nowhere.
    fn press(&mut self, hit: Option<HitTestResult>) {
        let doc = &mut self.main_frame.document;
        let target = hit.map(|hit| hit.node);
        let active =
            target.and_then(|t| chain(doc, t).into_iter().find(|&id| !is_disabled(doc, id)));
        self.input.activate_chain(doc, active);
        self.input.pressed = target.map(|node| Pressed {
            node,
            by: PressedBy::Pointer,
        });
        let focus =
            target.and_then(|t| inclusive_ancestors(doc, t).find(|&id| is_focusable(doc, id)));
        self.input.modality = Modality::Pointer;
        self.input.scroll_origin = target.map(|node| NodeRef {
            frame: FrameId::MAIN,
            node,
        });
        self.focus(focus);
        // A press on a text input's own box puts the caret where it landed
        // (AC-13).
        if let Some(hit) = hit
            && Some(hit.node) == focus
            && self.editing() == Some(hit.node)
            && let Some(caret) = self.caret_at(hit.node, hit.point.x)
        {
            self.edits.set(
                hit.node,
                editor::EditState {
                    caret,
                    anchor: None,
                },
            );
            self.caret_moved = Some(hit.node);
            self.needs_update = true;
        }
    }

    /// A primary release (AC-15): clears `:active`, and activates the
    /// nearest common ancestor of the press and release targets unless it
    /// sits in a disabled element.
    fn release(&mut self, hit: Option<HitTestResult>) -> Option<NodeRef> {
        let pressed = self.input.pressed.filter(|p| p.by == PressedBy::Pointer)?;
        self.input.pressed = None;
        let doc = &mut self.main_frame.document;
        self.input.activate_chain(doc, None);
        let common = common_ancestor(doc, pressed.node, hit?.node)?;
        self.activation(common)
    }

    /// A click's default action on a checkbox, or on a label holding one:
    /// its checkedness flips (spec 0035, AC-2).
    fn toggle_checkbox(&mut self, node: NodeId) {
        use base::local_name;
        let doc = &mut self.main_frame.document;
        let is_checkbox = |doc: &Document, id| {
            doc.is_html(id, local_name!("input"))
                && attribute(doc, id, local_name!("type"))
                    .is_some_and(|t| t.eq_ignore_ascii_case("checkbox"))
        };
        let label = base::atoms::LocalName::from("label");
        let target = inclusive_ancestors(doc, node).find_map(|id| {
            if is_checkbox(doc, id) {
                return Some(id);
            }
            if !doc.is_html(id, label.clone()) {
                return None;
            }
            let mut stack: Vec<NodeId> = doc.children(id).collect();
            stack.reverse();
            while let Some(child) = stack.pop() {
                if is_checkbox(doc, child) {
                    return Some(child);
                }
                stack.extend(doc.children(child).collect::<Vec<_>>().into_iter().rev());
            }
            None
        });
        let Some(target) = target else {
            return;
        };
        if is_disabled(doc, target) {
            return;
        }
        if state(doc, target).contains(ElementState::CHECKED) {
            doc.set_state(target, ElementState::empty(), ElementState::CHECKED);
        } else {
            doc.set_state(target, ElementState::CHECKED, ElementState::empty());
        }
        self.needs_update = true;
    }

    /// `node` as an activation, unless it or an ancestor is disabled.
    fn activation(&self, node: NodeId) -> Option<NodeRef> {
        let doc = &self.main_frame.document;
        let disabled = inclusive_ancestors(doc, node).any(|id| is_disabled(doc, id));
        (!disabled && is_element(doc, node)).then_some(NodeRef {
            frame: FrameId::MAIN,
            node,
        })
    }

    /// A key down or up (AC-13 to AC-15).
    fn key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        pressed: bool,
        repeat: bool,
        outcome: &mut InputOutcome,
    ) {
        let focused = self.main_frame.document.focused();
        if !pressed {
            if key == Key::Space
                && let Some(press) = self.input.pressed.filter(|p| p.by == PressedBy::Space)
            {
                self.input.pressed = None;
                self.input
                    .activate_chain(&mut self.main_frame.document, None);
                if focused == Some(press.node) {
                    outcome.activated = self.activation(press.node);
                }
            }
            return;
        }
        // A key while focused by a click turns the ring on (AC-14). The
        // browser never forwards a modifier alone.
        if focused.is_some() && self.input.modality == Modality::Pointer {
            self.input.modality = Modality::Keyboard;
            self.input.focus_visible = true;
            self.show_focus();
        }
        let plain = !(modifiers.ctrl || modifiers.alt || modifiers.meta);
        // On macOS, Cmd+Left and Cmd+Right keep their text meaning in a
        // field: start and end (AC-29).
        let command =
            cfg!(target_os = "macos") && modifiers.meta && !modifiers.ctrl && !modifiers.alt;
        if (plain || command) && self.editing().is_some() {
            let motion = |to| EditOp::Move {
                to,
                extend: modifiers.shift,
            };
            let op = match key {
                Key::ArrowLeft if command => Some(motion(Motion::Home)),
                Key::ArrowRight if command => Some(motion(Motion::End)),
                _ if command => None,
                Key::Backspace => Some(EditOp::Backspace),
                Key::Delete => Some(EditOp::Delete),
                Key::ArrowLeft => Some(motion(Motion::Left)),
                Key::ArrowRight => Some(motion(Motion::Right)),
                Key::Home => Some(motion(Motion::Home)),
                Key::End => Some(motion(Motion::End)),
                _ => None,
            };
            if let Some(op) = op {
                self.edit(op);
                return;
            }
        }
        let doc = &self.main_frame.document;
        let is_button = focused.is_some_and(|id| doc.is_html(id, base::local_name!("button")));
        match key {
            Key::Tab if plain => {
                let direction = if modifiers.shift {
                    FocusDirection::Backward
                } else {
                    FocusDirection::Forward
                };
                let next = next_in_tab_order(doc, focused, direction);
                self.input.modality = Modality::Keyboard;
                self.focus(next);
                if next.is_none() {
                    outcome.focus_left = Some(direction);
                }
            }
            Key::Enter if plain && focused.is_some_and(|id| activates_on_enter(doc, id)) => {
                outcome.activated = focused.and_then(|id| self.activation(id));
            }
            Key::Space if plain && is_button => {
                if !repeat {
                    self.input
                        .activate_chain(&mut self.main_frame.document, focused);
                    self.input.pressed = focused.map(|node| Pressed {
                        node,
                        by: PressedBy::Space,
                    });
                }
            }
            key => {
                // A key the page did not use: if it scrolls, name the frame
                // (or the viewport) the browser should move with it.
                if let Some(scroll) = scroll_key(&key, modifiers) {
                    outcome.scroll = Some((scroll, self.scroll_target(scroll)));
                } else {
                    outcome.unused_key = Some(key);
                }
            }
        }
    }

    /// The frame a scroll key should move: the nearest inclusive flat tree
    /// ancestor of the focused element (else `scroll_origin`) that is a user
    /// scrollable container on the key's axis, else `None` for the viewport
    /// (spec 0009, child 0002, Value sourcing, Key scroll target).
    fn scroll_target(&self, key: KeyScroll) -> Option<ScrollId> {
        let horizontal = matches!(key, KeyScroll::LineLeft | KeyScroll::LineRight);
        let doc = &self.main_frame.document;
        let mut node = doc.focused();
        while let Some(id) = node {
            let overflow = doc
                .node(id)
                .and_then(|n| n.as_element())
                .and_then(|e| e.style())
                .map(|style| {
                    if horizontal {
                        style.display_box.overflow_x
                    } else {
                        style.display_box.overflow_y
                    }
                });
            // `scroll` and `auto` are user scrollable on that axis; `hidden`
            // and `clip` are not (spec 0009, child 0002, AC-6 and AC-7).
            if let Some(overflow) = overflow
                && matches!(overflow, Overflow::Scroll | Overflow::Auto)
            {
                return Some(paint::scroll_id(FrameId::MAIN.0, id));
            }
            node = doc.flat_parent(id);
        }
        None
    }

    /// The focused text input the editor works on, if any (AC-13).
    pub(crate) fn editing(&self) -> Option<NodeId> {
        let doc = &self.main_frame.document;
        doc.focused().filter(|&id| {
            editor::editable_type(doc, id)
                && !inclusive_ancestors(doc, id).any(|a| is_disabled(doc, a))
        })
    }

    /// Applies `op` to the focused text input, if any: the new value goes
    /// into the DOM, the caret and selection into the page, and the next
    /// frame shows them. Returns text for the clipboard.
    fn edit(&mut self, op: EditOp<'_>) -> Option<String> {
        let id = self.editing()?;
        let doc = &self.main_frame.document;
        let value = doc.input_value(id)?;
        let limits = editor::limits(doc, id, self.text_cap);
        let state = self.edits.get(id, &value);
        let applied = editor::apply(&value, state, op, limits);
        if let Some(value) = applied.value {
            // The element is focused, so alive and an element.
            let _ = self.main_frame.document.set_input_value(id, value);
        }
        self.edits.set(id, applied.state);
        self.caret_moved = Some(id);
        self.needs_update = true;
        applied.clipboard
    }

    /// Replaces the value of the text input `id` from outside the editor,
    /// the caret at its end and nothing selected, showing the value from
    /// its start: the chrome mirrors the address into its input this way
    /// (spec 0008, AC-3, AC-12). Not an edit, so the caret does not
    /// scroll into view. Anything but a live text input is left alone.
    pub fn set_input_text(&mut self, id: NodeId, text: &str) {
        let doc = &mut self.main_frame.document;
        if !doc.is_alive(id) || !editor::editable_type(doc, id) {
            return;
        }
        if doc.input_value(id).as_deref() != Some(text) {
            let _ = doc.set_input_value(id, text.to_owned());
        }
        self.edits.set(id, editor::EditState::at_end(text));
        if self.caret_moved == Some(id) {
            self.caret_moved = None;
        }
        self.show_field_start(id);
        self.needs_update = true;
    }

    /// Scrolls the text input `id` back to the start of its value.
    fn show_field_start(&mut self, id: NodeId) {
        let scroll_id = paint::scroll_id(FrameId::MAIN.0, id);
        if self.scroll_offset(scroll_id).x != 0.0 {
            self.scroll_to(
                NodeRef {
                    frame: FrameId::MAIN,
                    node: id,
                },
                0.0,
                0.0,
            );
        }
    }

    /// The focused text input's caret, selection and current value, for
    /// the caller to read (tests, the chrome's address).
    pub fn edit_state(&self) -> Option<(String, editor::EditState)> {
        let id = self.editing()?;
        let value = self.main_frame.document.input_value(id)?;
        let state = editor::snapped(&value, self.edits.get(id, &value));
        Some((value, state))
    }

    /// Moves focus to `node` (or nowhere), with `:focus-visible` by the
    /// current modality, always for a text field (AC-14).
    fn focus(&mut self, node: Option<NodeId>) {
        let doc = &mut self.main_frame.document;
        doc.set_focus(node);
        self.input.focus_visible = node
            .is_some_and(|id| self.input.modality == Modality::Keyboard || doc.is_text_field(id));
        self.show_focus();
    }

    /// Puts the focus bits on the focused element, or hides them while the
    /// window is in the background (AC-13).
    fn show_focus(&mut self) {
        let doc = &mut self.main_frame.document;
        let Some(focused) = doc.focused() else {
            return;
        };
        let (shown, hidden) = match (self.input.window_focused, self.input.focus_visible) {
            (false, _) => (
                ElementState::empty(),
                ElementState::FOCUS | ElementState::FOCUS_VISIBLE,
            ),
            (true, false) => (ElementState::FOCUS, ElementState::FOCUS_VISIBLE),
            (true, true) => (
                ElementState::FOCUS | ElementState::FOCUS_VISIBLE,
                ElementState::empty(),
            ),
        };
        doc.set_state(focused, shown, hidden);
    }

    /// The pointer shape for what is hovered (AC-18): its `cursor`, with
    /// `auto` a text beam over selectable text and the arrow elsewhere.
    fn update_cursor(&mut self) {
        let doc = &self.main_frame.document;
        let style = self
            .input
            .hovered
            .first()
            .filter(|&&id| doc.is_alive(id))
            .and_then(|&id| doc.node(id))
            .and_then(|n| n.as_element())
            .and_then(|e| e.style());
        let cursor = match style.map(|s| s.inherited_box.cursor) {
            Some(css::Cursor::Pointer) => CursorKind::Pointer,
            Some(css::Cursor::Text) => CursorKind::Text,
            Some(css::Cursor::ZoomIn) => CursorKind::ZoomIn,
            Some(css::Cursor::ZoomOut) => CursorKind::ZoomOut,
            Some(css::Cursor::Auto) if self.input.over_text => CursorKind::Text,
            _ => CursorKind::Default,
        };
        self.input.set_cursor(cursor);
    }
}

#[cfg(test)]
mod tests {
    use base::atoms::QualName;
    use base::local_name;

    use super::*;

    /// `html` > `body` > (`div` > text, `p`).
    fn document() -> (Document, [NodeId; 5]) {
        let mut doc = Document::new("about:blank");
        let element = |doc: &mut Document, parent, name| {
            let id = doc.create_element(QualName::html(name));
            doc.append_child(parent, id).unwrap();
            id
        };
        let root = doc.root();
        let html = element(&mut doc, root, local_name!("html"));
        let body = element(&mut doc, html, local_name!("body"));
        let div = element(&mut doc, body, local_name!("div"));
        let p = element(&mut doc, body, local_name!("p"));
        let text = doc.create_text("hi");
        doc.append_child(div, text).unwrap();
        (doc, [html, body, div, p, text])
    }

    fn hovered(doc: &Document, nodes: &[NodeId]) -> Vec<bool> {
        nodes
            .iter()
            .map(|&id| state(doc, id).contains(ElementState::HOVER))
            .collect()
    }

    fn hit(node: NodeId) -> HitTestResult {
        HitTestResult {
            node,
            point: base::Point::default(),
            selectable_text: false,
        }
    }

    #[test]
    fn hover_marks_the_element_and_its_ancestors_and_moves_with_the_pointer() {
        // Spec 0006, AC-12
        let (mut doc, [html, body, div, p, text]) = document();
        let mut input = InputState::default();
        input.hover(&mut doc, Some(&hit(text)));
        assert_eq!(
            hovered(&doc, &[html, body, div, p]),
            [true, true, true, false],
            "a text hit hovers its parent element and up"
        );
        input.hover(&mut doc, Some(&hit(p)));
        assert_eq!(
            hovered(&doc, &[html, body, div, p]),
            [true, true, false, true]
        );
        input.release_all(&mut doc);
        assert_eq!(hovered(&doc, &[html, body, div, p]), [false; 4]);
    }

    #[test]
    fn only_the_latest_move_waits_for_the_frame() {
        let mut input = InputState::default();
        assert!(!input.has_pending_move());
        input.pointer_move(PointF { x: 1.0, y: 1.0 });
        input.pointer_move(PointF { x: 5.0, y: 6.0 });
        assert_eq!(input.take_pending_move(), Some(PointF { x: 5.0, y: 6.0 }));
        assert_eq!(input.take_pending_move(), None);
        input.document_replaced();
        assert_eq!(
            input.take_pending_move(),
            Some(PointF { x: 5.0, y: 6.0 }),
            "a new document is hit tested where the pointer still is"
        );
        input.layout_changed();
        assert!(input.has_pending_move(), "a new layout is hit tested again");
    }

    #[test]
    fn tabindex_parses_with_the_html_integer_rules() {
        // Spec 0006, Value sourcing, Tab order
        let (mut doc, [_, _, div, _, _]) = document();
        let mut parse = |value: &str| {
            doc.set_attribute(div, QualName::attribute(local_name!("tabindex")), value)
                .unwrap();
            tab_index(&doc, div)
        };
        assert_eq!(parse("2"), Some(2));
        assert_eq!(parse("  -1"), Some(-1));
        assert_eq!(parse("+3"), Some(3));
        assert_eq!(parse("4px"), Some(4));
        assert_eq!(parse(""), None);
        assert_eq!(parse("x1"), None);
        assert_eq!(parse("99999999999"), None);
    }

    #[test]
    fn scroll_key_maps_plain_keys_to_scroll_amounts() {
        // Spec 0009, child 0002, AC-9: arrows, space and page keys scroll,
        // shifted space pages up; chords and other keys do not.
        let none = Modifiers {
            shift: false,
            ctrl: false,
            alt: false,
            meta: false,
        };
        let shift = Modifiers {
            shift: true,
            ctrl: false,
            alt: false,
            meta: false,
        };
        assert_eq!(scroll_key(&Key::ArrowDown, none), Some(KeyScroll::LineDown));
        assert_eq!(scroll_key(&Key::ArrowUp, none), Some(KeyScroll::LineUp));
        assert_eq!(scroll_key(&Key::ArrowLeft, none), Some(KeyScroll::LineLeft));
        assert_eq!(
            scroll_key(&Key::ArrowRight, none),
            Some(KeyScroll::LineRight)
        );
        assert_eq!(scroll_key(&Key::Space, none), Some(KeyScroll::PageDown));
        assert_eq!(scroll_key(&Key::Space, shift), Some(KeyScroll::PageUp));
        assert_eq!(scroll_key(&Key::PageDown, none), Some(KeyScroll::PageDown));
        assert_eq!(scroll_key(&Key::PageUp, none), Some(KeyScroll::PageUp));
        assert_eq!(scroll_key(&Key::Home, none), Some(KeyScroll::Home));
        assert_eq!(scroll_key(&Key::End, none), Some(KeyScroll::End));
        assert_eq!(scroll_key(&Key::Character("a".into()), none), None);
        assert_eq!(scroll_key(&Key::Tab, none), None);
        let meta = Modifiers {
            shift: false,
            ctrl: false,
            alt: false,
            meta: true,
        };
        assert_eq!(
            scroll_key(&Key::ArrowDown, Modifiers { alt: true, ..none }),
            None
        );
        if cfg!(target_os = "macos") {
            assert_eq!(scroll_key(&Key::ArrowDown, meta), Some(KeyScroll::End));
        } else {
            assert_eq!(scroll_key(&Key::ArrowDown, meta), None);
        }
    }
}
