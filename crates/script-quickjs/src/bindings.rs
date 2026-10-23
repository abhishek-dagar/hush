//! The natives `prelude.js` builds the DOM API on (spec 0004, child 0006):
//! each borrows the host, does one DOM operation, and returns before any
//! JavaScript runs again, so no host borrow is ever held across a call
//! into the engine.
//!
//! Nodes leave as string keys and come back as wrapper objects, instances
//! of [`NodeWrapper`]. A wrapper pins its node while it lives; its `Drop`,
//! which QuickJS runs when it collects the wrapper, only queues the unpin,
//! and the queue is drained after the task (AC-34).

use std::cell::RefCell;
use std::num::NonZeroU32;
use std::rc::Rc;

use dom::{FrameId, NodeId, NodeRef};
use rquickjs::class::{JsClass, Readable, Trace, Tracer};
use rquickjs::function::IntoJsFunc;
use rquickjs::{Class, Ctx, Function, IntoJs, JsLifetime, Object, Result, Value};
use script::{ConsoleLevel, DomException, NodeKind, ScriptHost};

/// The page's side of the bindings.
pub(crate) type Host = Rc<RefCell<dyn ScriptHost>>;
/// Nodes whose wrappers were collected, to unpin after the task.
pub(crate) type Unpins = Rc<RefCell<Vec<NodeRef>>>;

/// The object behind every node wrapper: which node it stands for.
pub(crate) struct NodeWrapper {
    node: NodeRef,
    unpins: Unpins,
}

impl Drop for NodeWrapper {
    fn drop(&mut self) {
        // Runs inside the collector: only queue the unpin. The queue is
        // never borrowed while JavaScript runs, so this cannot clash.
        if let Ok(mut unpins) = self.unpins.try_borrow_mut() {
            unpins.push(self.node);
        }
    }
}

impl<'js> Trace<'js> for NodeWrapper {
    fn trace<'a>(&self, _tracer: Tracer<'a, 'js>) {}
}

// SAFETY: `NodeWrapper` holds no JavaScript values and no lifetimes, so it
// is the same type for every `'js`, which is all `JsLifetime` asks.
unsafe impl<'js> JsLifetime<'js> for NodeWrapper {
    type Changed<'to> = NodeWrapper;
}

impl<'js> JsClass<'js> for NodeWrapper {
    const NAME: &'static str = "NodeWrapper";
    type Mutable = Readable;

    /// Wrappers take their prototype from `prelude.js` (`Element`, `Text`
    /// and so on), so the class needs none of its own.
    fn prototype(_ctx: &Ctx<'js>) -> Result<Option<Object<'js>>> {
        Ok(None)
    }

    fn constructor(_ctx: &Ctx<'js>) -> Result<Option<rquickjs::Constructor<'js>>> {
        Ok(None)
    }
}

/// A node's key: frame, slot index and generation.
pub(crate) fn key(node: NodeRef) -> String {
    format!(
        "{}:{}:{}",
        node.frame.0,
        node.node.index(),
        node.node.generation()
    )
}

/// The node a key names. A made up or stale key names a missing node,
/// which every host call treats as such.
fn parse_key(key: &str) -> Option<NodeRef> {
    let mut parts = key.split(':');
    let frame = parts.next()?.parse().ok()?;
    let index = parts.next()?.parse().ok()?;
    let generation = NonZeroU32::new(parts.next()?.parse().ok()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some(NodeRef {
        frame: FrameId(frame),
        node: NodeId::from_parts(index, generation),
    })
}

/// The node a wrapper stands for.
fn node_of(wrapper: &Class<'_, NodeWrapper>) -> NodeRef {
    wrapper.borrow().node
}

/// The `prelude.js` name of a node kind's prototype.
fn kind_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Element => "Element",
        NodeKind::Text => "Text",
        NodeKind::Comment => "Comment",
        NodeKind::Document => "Document",
        NodeKind::Doctype => "DocumentType",
        NodeKind::Fragment => "DocumentFragment",
    }
}

/// The number code `prelude.js` maps back to a `DOMException` name.
fn code(err: DomException) -> i32 {
    match err {
        DomException::HierarchyRequest => 1,
        DomException::NotFound => 2,
        DomException::InvalidState => 3,
        DomException::InvalidCharacter => 4,
        DomException::Syntax => 5,
        DomException::NotSupported => 6,
    }
}

/// A fallible host call's answer: the value, or the error's code.
fn fallible<'js, T: IntoJs<'js>>(
    ctx: &Ctx<'js>,
    result: std::result::Result<T, DomException>,
) -> Result<Value<'js>> {
    match result {
        Ok(value) => value.into_js(ctx),
        Err(err) => Ok(Value::new_int(ctx.clone(), code(err))),
    }
}

fn level(name: &str) -> ConsoleLevel {
    match name {
        "debug" => ConsoleLevel::Debug,
        "info" => ConsoleLevel::Info,
        "warn" => ConsoleLevel::Warn,
        "error" => ConsoleLevel::Error,
        _ => ConsoleLevel::Log,
    }
}

fn define<'js, P, F>(object: &Object<'js>, name: &str, f: F) -> Result<()>
where
    F: IntoJsFunc<'js, P> + 'js,
{
    let function = Function::new(object.ctx().clone(), f)?.with_name(name)?;
    object.set(name, function)
}

/// The natives object `prelude.js` is called with.
pub(crate) fn natives<'js>(ctx: &Ctx<'js>, host: &Host, unpins: &Unpins) -> Result<Object<'js>> {
    let native = Object::new(ctx.clone())?;
    let keyed = |node: Option<NodeRef>| node.map(key);

    // Wrappers.
    let h = Rc::clone(host);
    define(&native, "kind", move |node: String| {
        let kind = parse_key(&node).and_then(|node| h.borrow().node_kind(node));
        kind.map(kind_name)
    })?;
    let (h, u) = (Rc::clone(host), Rc::clone(unpins));
    define(
        &native,
        "instantiate",
        move |node: String, proto: Object<'js>| -> Result<Value<'js>> {
            let ctx = proto.ctx().clone();
            let Some(node) = parse_key(&node) else {
                return Ok(Value::new_null(ctx));
            };
            let wrapper = NodeWrapper {
                node,
                unpins: Rc::clone(&u),
            };
            let instance = Class::instance_proto(wrapper, proto)?;
            h.borrow_mut().pin(node);
            Ok(instance.into_value())
        },
    )?;

    // The document.
    let h = Rc::clone(host);
    define(&native, "document", move || key(h.borrow().document()))?;
    let h = Rc::clone(host);
    define(&native, "documentElement", move || {
        keyed(h.borrow().document_element())
    })?;
    let h = Rc::clone(host);
    define(&native, "body", move || keyed(h.borrow().body()))?;
    let h = Rc::clone(host);
    define(&native, "head", move || keyed(h.borrow().head()))?;
    let h = Rc::clone(host);
    define(&native, "readyState", move || {
        h.borrow().ready_state().as_str()
    })?;
    let h = Rc::clone(host);
    define(&native, "elementById", move |id: String| {
        keyed(h.borrow().element_by_id(&id))
    })?;
    let h = Rc::clone(host);
    define(
        &native,
        "query",
        move |scope: Class<'js, NodeWrapper>, selectors: String| -> Result<Value<'js>> {
            let ctx = scope.ctx().clone();
            let found = h
                .borrow()
                .query_selector_all(node_of(&scope), &selectors)
                .map(|nodes| nodes.into_iter().map(key).collect::<Vec<_>>());
            fallible(&ctx, found)
        },
    )?;
    let h = Rc::clone(host);
    define(
        &native,
        "createElement",
        move |ctx: Ctx<'js>, name: String| -> Result<Value<'js>> {
            let created = h.borrow_mut().create_element(&name).map(key);
            fallible(&ctx, created)
        },
    )?;
    let h = Rc::clone(host);
    define(&native, "createTextNode", move |data: String| {
        key(h.borrow_mut().create_text_node(&data))
    })?;

    // Nodes.
    let h = Rc::clone(host);
    define(&native, "nodeType", move |node: Class<'js, NodeWrapper>| {
        h.borrow()
            .node_kind(node_of(&node))
            .map(NodeKind::node_type)
    })?;
    let h = Rc::clone(host);
    define(&native, "nodeName", move |node: Class<'js, NodeWrapper>| {
        h.borrow().node_name(node_of(&node))
    })?;
    type Step = fn(&dyn ScriptHost, NodeRef) -> Option<NodeRef>;
    let steps: [(&str, Step); 5] = [
        ("parent", |host, node| host.parent(node)),
        ("firstChild", |host, node| host.first_child(node)),
        ("lastChild", |host, node| host.last_child(node)),
        ("nextSibling", |host, node| host.next_sibling(node)),
        ("previousSibling", |host, node| host.previous_sibling(node)),
    ];
    for (name, step) in steps {
        let h = Rc::clone(host);
        define(&native, name, move |node: Class<'js, NodeWrapper>| {
            step(&*h.borrow(), node_of(&node)).map(key)
        })?;
    }
    let h = Rc::clone(host);
    define(
        &native,
        "childNodes",
        move |node: Class<'js, NodeWrapper>| {
            let children = h.borrow().child_nodes(node_of(&node));
            children.into_iter().map(key).collect::<Vec<_>>()
        },
    )?;
    let h = Rc::clone(host);
    define(
        &native,
        "textContent",
        move |node: Class<'js, NodeWrapper>| h.borrow().text_content(node_of(&node)),
    )?;
    let h = Rc::clone(host);
    define(
        &native,
        "setTextContent",
        move |node: Class<'js, NodeWrapper>, text: String| -> Result<Value<'js>> {
            let ctx = node.ctx().clone();
            let done = h.borrow_mut().set_text_content(node_of(&node), &text);
            fallible(&ctx, done)
        },
    )?;
    let h = Rc::clone(host);
    define(
        &native,
        "insertBefore",
        move |parent: Class<'js, NodeWrapper>,
              child: Class<'js, NodeWrapper>,
              reference: Option<Class<'js, NodeWrapper>>|
              -> Result<Value<'js>> {
            let ctx = parent.ctx().clone();
            let reference = reference.as_ref().map(node_of);
            let done = h
                .borrow_mut()
                .insert_before(node_of(&parent), node_of(&child), reference);
            fallible(&ctx, done)
        },
    )?;
    let h = Rc::clone(host);
    define(
        &native,
        "removeChild",
        move |parent: Class<'js, NodeWrapper>,
              child: Class<'js, NodeWrapper>|
              -> Result<Value<'js>> {
            let ctx = parent.ctx().clone();
            let done = h
                .borrow_mut()
                .remove_child(node_of(&parent), node_of(&child));
            fallible(&ctx, done)
        },
    )?;
    let h = Rc::clone(host);
    define(
        &native,
        "replaceChild",
        move |parent: Class<'js, NodeWrapper>,
              new_child: Class<'js, NodeWrapper>,
              old_child: Class<'js, NodeWrapper>|
              -> Result<Value<'js>> {
            let ctx = parent.ctx().clone();
            let done = h.borrow_mut().replace_child(
                node_of(&parent),
                node_of(&new_child),
                node_of(&old_child),
            );
            fallible(&ctx, done)
        },
    )?;

    // Elements.
    let h = Rc::clone(host);
    define(
        &native,
        "getAttribute",
        move |element: Class<'js, NodeWrapper>, name: String| {
            h.borrow().attribute(node_of(&element), &name)
        },
    )?;
    let h = Rc::clone(host);
    define(
        &native,
        "setAttribute",
        move |element: Class<'js, NodeWrapper>,
              name: String,
              value: String|
              -> Result<Value<'js>> {
            let ctx = element.ctx().clone();
            let done = h
                .borrow_mut()
                .set_attribute(node_of(&element), &name, &value);
            fallible(&ctx, done)
        },
    )?;
    let h = Rc::clone(host);
    define(
        &native,
        "removeAttribute",
        move |element: Class<'js, NodeWrapper>, name: String| -> Result<Value<'js>> {
            let ctx = element.ctx().clone();
            let done = h.borrow_mut().remove_attribute(node_of(&element), &name);
            fallible(&ctx, done)
        },
    )?;

    // Timers and the console.
    let h = Rc::clone(host);
    define(&native, "setTimeout", move |delay: f64| {
        h.borrow_mut().set_timeout(delay).0
    })?;
    let h = Rc::clone(host);
    define(&native, "clearTimeout", move |id: f64| {
        if id.is_finite() && id >= 1.0 && id <= f64::from(u32::MAX) {
            h.borrow_mut().clear_timeout(script::TimerId(id as u32));
        }
    })?;
    let h = Rc::clone(host);
    define(&native, "console", move |name: String, message: String| {
        h.borrow_mut().console(level(&name), &message);
    })?;
    Ok(native)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_and_junk_names_nothing() {
        let node = NodeRef {
            frame: FrameId(2),
            node: NodeId::from_parts(17, NonZeroU32::new(3).unwrap()),
        };
        assert_eq!(key(node), "2:17:3");
        assert_eq!(parse_key(&key(node)), Some(node));
        for junk in ["", "1:2", "1:2:0", "a:b:c", "1:2:3:4"] {
            assert_eq!(parse_key(junk), None, "{junk}");
        }
    }
}
