//! Node types: tree links, flags and the per kind data (spec 0002, DOM).

use std::fmt;
use std::ops::BitOr;
use std::rc::Rc;

use base::BoxId;
use base::atoms::{Atom, QualName};
use css::ComputedStyle;

use crate::{FrameId, NodeId};

/// Generates a small flag set over an unsigned integer: consts, `contains`,
/// `|`, and a `Debug` that names the set flags. Own type rather than the
/// `bitflags` crate, to keep dependencies down.
macro_rules! flag_set {
    ($(#[$meta:meta])* $name:ident($repr:ty) { $($(#[$flag_meta:meta])* $flag:ident = $bit:expr;)* }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
        pub struct $name($repr);

        impl $name {
            $($(#[$flag_meta])* pub const $flag: Self = Self(1 << $bit);)*

            /// No flags set.
            pub const fn empty() -> Self {
                Self(0)
            }

            /// Whether every flag in `other` is set.
            pub const fn contains(self, other: Self) -> bool {
                self.0 & other.0 == other.0
            }

            /// The flags in either set. Usable in `const` items, unlike `|`.
            pub const fn union(self, other: Self) -> Self {
                Self(self.0 | other.0)
            }

            /// Whether any flag in `other` is set.
            pub const fn intersects(self, other: Self) -> bool {
                self.0 & other.0 != 0
            }

            #[allow(dead_code, reason = "only node flags are changed in place so far")]
            pub(crate) fn insert(&mut self, other: Self) {
                self.0 |= other.0;
            }

            #[allow(dead_code, reason = "only node flags are changed in place so far")]
            pub(crate) fn remove(&mut self, other: Self) {
                self.0 &= !other.0;
            }
        }

        impl BitOr for $name {
            type Output = Self;

            fn bitor(self, other: Self) -> Self {
                Self(self.0 | other.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let mut set = f.debug_set();
                $(if self.contains(Self::$flag) {
                    set.entry(&format_args!(stringify!($flag)));
                })*
                set.finish()
            }
        }
    };
}

flag_set! {
    /// Per node state bits (spec 0002, Node).
    NodeFlags(u16) {
        /// The node's shadow including root is the document.
        IS_CONNECTED = 0;
        /// The node is a `<template>`'s content fragment.
        IS_INERT = 1;
        /// The element's own style must be recomputed.
        NEEDS_STYLE = 2;
        /// Some flat tree descendant has `NEEDS_STYLE`.
        DESCENDANT_NEEDS_STYLE = 3;
        /// Every element in this subtree must be restyled.
        RESTYLE_SUBTREE = 4;
        /// The boxes for this node's children no longer match the DOM.
        NEEDS_BOX_REBUILD = 5;
        /// Some flat tree descendant has `NEEDS_BOX_REBUILD`.
        DESCENDANT_NEEDS_BOX_REBUILD = 6;
        /// A sibling selector matched against this element.
        AFFECTED_BY_SIBLING_SELECTORS = 7;
        /// The element hosts a shadow root.
        HAS_SHADOW_ROOT = 8;
        /// The element is the subject of a `:has()` rule whose argument may
        /// change independently of it, so it must be restyled when a
        /// descendant or following sibling changes (spec 0010, AC-15).
        HAS_SUBJECT = 9;
    }
}

flag_set! {
    /// Element states that selectors such as `:hover` match against. They
    /// change only through `Document::set_state` (spec 0006, Key
    /// invariants).
    ElementState(u16) {
        /// `:hover`.
        HOVER = 0;
        /// `:active`.
        ACTIVE = 1;
        /// `:focus`.
        FOCUS = 2;
        /// `:focus-visible`.
        FOCUS_VISIBLE = 3;
        /// `:checked`.
        CHECKED = 4;
        /// `:disabled`.
        DISABLED = 5;
        /// `:link`.
        LINK = 6;
        /// `:visited`.
        VISITED = 7;
    }
}

impl From<css::PseudoState> for ElementState {
    fn from(state: css::PseudoState) -> Self {
        match state {
            css::PseudoState::Hover => Self::HOVER,
            css::PseudoState::Active => Self::ACTIVE,
            css::PseudoState::Focus => Self::FOCUS,
            css::PseudoState::FocusVisible => Self::FOCUS_VISIBLE,
            css::PseudoState::Disabled => Self::DISABLED,
        }
    }
}

/// One node: its tree links, its box, and what kind of node it is.
#[derive(Debug)]
pub struct Node {
    pub(crate) parent: Option<NodeId>,
    pub(crate) first_child: Option<NodeId>,
    pub(crate) last_child: Option<NodeId>,
    pub(crate) prev_sibling: Option<NodeId>,
    pub(crate) next_sibling: Option<NodeId>,
    pub(crate) flags: NodeFlags,
    pub(crate) pin_count: u32,
    pub(crate) layout_box: Option<BoxId>,
    pub(crate) data: NodeData,
}

impl Node {
    pub(crate) fn new(data: NodeData) -> Self {
        Self {
            parent: None,
            first_child: None,
            last_child: None,
            prev_sibling: None,
            next_sibling: None,
            flags: NodeFlags::empty(),
            pin_count: 0,
            layout_box: None,
            data,
        }
    }

    /// The parent node.
    pub fn parent(&self) -> Option<NodeId> {
        self.parent
    }

    /// The first child.
    pub fn first_child(&self) -> Option<NodeId> {
        self.first_child
    }

    /// The last child.
    pub fn last_child(&self) -> Option<NodeId> {
        self.last_child
    }

    /// The previous sibling.
    pub fn prev_sibling(&self) -> Option<NodeId> {
        self.prev_sibling
    }

    /// The next sibling.
    pub fn next_sibling(&self) -> Option<NodeId> {
        self.next_sibling
    }

    /// The node's state bits.
    pub fn flags(&self) -> NodeFlags {
        self.flags
    }

    /// Whether the node is in its document (through shadow roots too).
    pub fn is_connected(&self) -> bool {
        self.flags.contains(NodeFlags::IS_CONNECTED)
    }

    /// How many holders outside the tree keep this node alive.
    pub fn pin_count(&self) -> u32 {
        self.pin_count
    }

    /// The node's principal layout box, `None` if it generates none.
    pub fn layout_box(&self) -> Option<BoxId> {
        self.layout_box
    }

    /// What kind of node this is.
    pub fn data(&self) -> &NodeData {
        &self.data
    }

    /// The element data, if this is an element.
    pub fn as_element(&self) -> Option<&ElementData> {
        match &self.data {
            NodeData::Element(element) => Some(element),
            _ => None,
        }
    }

    pub(crate) fn as_element_mut(&mut self) -> Option<&mut ElementData> {
        match &mut self.data {
            NodeData::Element(element) => Some(element),
            _ => None,
        }
    }
}

/// The kind of a node and the data only that kind has.
#[derive(Debug)]
pub enum NodeData {
    /// The document node, root of the tree.
    Document,
    /// A `<!DOCTYPE>`.
    Doctype(Box<DoctypeData>),
    /// An element.
    Element(Box<ElementData>),
    /// A text node.
    Text(String),
    /// A comment.
    Comment(String),
    /// A document fragment. `host` is the `<template>` whose content it is,
    /// `None` for a fragment made by script; the DOM standard's fragment host.
    DocumentFragment {
        /// The owning `<template>`, if this is template content.
        host: Option<NodeId>,
    },
    /// The root of a shadow tree, owned by its host element.
    ShadowRoot {
        /// The element the shadow tree is attached to.
        host: NodeId,
        /// Whether script outside can reach the shadow tree.
        mode: ShadowMode,
    },
}

/// Whether a shadow root is reachable from outside through `shadowRoot`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowMode {
    /// Reachable.
    Open,
    /// Hidden from script outside the shadow tree.
    Closed,
}

/// A doctype's three fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctypeData {
    /// The name, such as `html`.
    pub name: String,
    /// The public identifier, often empty.
    pub public_id: String,
    /// The system identifier, often empty.
    pub system_id: String,
}

/// One attribute on an element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    /// The qualified name.
    pub name: QualName,
    /// The value.
    pub value: String,
}

/// What an `<img>` shows, as the page last set it (spec 0004, child 0005,
/// AC-29). Layout sizes and draws the element from it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ImageState {
    /// No picture yet (it is loading): sized by its `width` and `height`
    /// attributes, else 0 by 0.
    #[default]
    Pending,
    /// Decoded: its size in pixels, and the display list image key it is
    /// drawn with.
    Ready {
        /// The natural width.
        width: u32,
        /// The natural height.
        height: u32,
        /// The `ImageKey` the page registered it under.
        key: u32,
    },
    /// No `src`, or it could not load or decode: its `alt` text shows
    /// instead, or nothing.
    Failed,
}

/// Data only some element types have. New element types add variants;
/// nothing else goes on [`Node`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementKind {
    /// A `<template>` and its inert content fragment, which it owns.
    Template {
        /// The content fragment.
        content: NodeId,
    },
    /// An `<iframe>` and the frame showing its document, while connected.
    IFrame {
        /// The child frame, `None` until the page creates it.
        frame: Option<FrameId>,
    },
    /// An `<img>` and what it shows.
    Img {
        /// Set by the page as its image loads.
        image: ImageState,
    },
    /// Any other element.
    Other,
}

/// What an element carries beyond the tree links.
#[derive(Debug)]
pub struct ElementData {
    pub(crate) name: QualName,
    pub(crate) attrs: Vec<Attribute>,
    pub(crate) id: Option<Atom>,
    pub(crate) classes: Vec<Atom>,
    pub(crate) state: ElementState,
    pub(crate) style: Option<Rc<ComputedStyle>>,
    /// An `<input>`'s `::placeholder` style, while it shows its placeholder.
    pub(crate) placeholder: Option<Rc<ComputedStyle>>,
    /// A text input's current value once edited (its dirty value), which
    /// replaces the `value` attribute from then on (spec 0008, AC-16).
    pub(crate) value: Option<String>,
    pub(crate) shadow_root: Option<NodeId>,
    pub(crate) kind: ElementKind,
}

impl ElementData {
    pub(crate) fn new(name: QualName, kind: ElementKind) -> Self {
        Self {
            name,
            attrs: Vec::new(),
            id: None,
            classes: Vec::new(),
            state: ElementState::empty(),
            style: None,
            placeholder: None,
            value: None,
            shadow_root: None,
            kind,
        }
    }

    /// The element's qualified name.
    pub fn name(&self) -> &QualName {
        &self.name
    }

    /// Every attribute, in the order they were first set.
    pub fn attributes(&self) -> &[Attribute] {
        &self.attrs
    }

    /// The value of the attribute called `name`, if set.
    pub fn attribute(&self, name: &QualName) -> Option<&str> {
        self.attrs
            .iter()
            .find(|attr| attr.name == *name)
            .map(|attr| attr.value.as_str())
    }

    /// The `id`, cached from the attribute. An empty `id` counts as none.
    pub fn id(&self) -> Option<&Atom> {
        self.id.as_ref()
    }

    /// The class names, cached from the `class` attribute, without repeats.
    pub fn classes(&self) -> &[Atom] {
        &self.classes
    }

    /// The `:hover`, `:focus` and similar states.
    pub fn state(&self) -> ElementState {
        self.state
    }

    /// The computed style, `None` until styled or while disconnected.
    pub fn style(&self) -> Option<&Rc<ComputedStyle>> {
        self.style.as_ref()
    }

    /// An `<input>`'s `::placeholder` style, set by the style pass while
    /// the input shows its placeholder (spec 0006, AC-17).
    pub fn placeholder_style(&self) -> Option<&Rc<ComputedStyle>> {
        self.placeholder.as_ref()
    }

    /// The attached shadow root, if any.
    pub fn shadow_root(&self) -> Option<NodeId> {
        self.shadow_root
    }

    /// Element type specific data.
    pub fn kind(&self) -> ElementKind {
        self.kind
    }
}
