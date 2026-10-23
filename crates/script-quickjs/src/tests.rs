//! The engine against a mock host with only a document: console, timers,
//! lifecycle events, and the runaway caps (AC-33, AC-35). The DOM API
//! itself is tested through `page`, against real documents.

use std::num::NonZeroU32;

use dom::{FrameId, NodeId};
use script::{ConsoleLevel, DomException, NodeKind, ReadyState};

use super::*;

#[derive(Default)]
struct Mock {
    console: Vec<(ConsoleLevel, String)>,
    navigations: Vec<(String, script::LocationAction)>,
    history: Vec<i32>,
    timers: Vec<f64>,
    cleared: Vec<u32>,
    pins: i32,
}

fn document() -> NodeRef {
    NodeRef {
        frame: FrameId::MAIN,
        node: NodeId::from_parts(0, NonZeroU32::MIN),
    }
}

impl ScriptHost for Mock {
    fn document(&self) -> NodeRef {
        document()
    }
    fn document_element(&self) -> Option<NodeRef> {
        None
    }
    fn body(&self) -> Option<NodeRef> {
        None
    }
    fn head(&self) -> Option<NodeRef> {
        None
    }
    fn ready_state(&self) -> ReadyState {
        ReadyState::Interactive
    }
    fn element_by_id(&self, _id: &str) -> Option<NodeRef> {
        None
    }
    fn query_selector_all(&self, _: NodeRef, _: &str) -> Result<Vec<NodeRef>, DomException> {
        Err(DomException::Syntax)
    }
    fn create_element(&mut self, _name: &str) -> Result<NodeRef, DomException> {
        Err(DomException::InvalidCharacter)
    }
    fn create_text_node(&mut self, _data: &str) -> NodeRef {
        document()
    }
    fn node_kind(&self, node: NodeRef) -> Option<NodeKind> {
        (node == document()).then_some(NodeKind::Document)
    }
    fn node_name(&self, _: NodeRef) -> Option<String> {
        Some("#document".into())
    }
    fn parent(&self, _: NodeRef) -> Option<NodeRef> {
        None
    }
    fn first_child(&self, _: NodeRef) -> Option<NodeRef> {
        None
    }
    fn last_child(&self, _: NodeRef) -> Option<NodeRef> {
        None
    }
    fn next_sibling(&self, _: NodeRef) -> Option<NodeRef> {
        None
    }
    fn previous_sibling(&self, _: NodeRef) -> Option<NodeRef> {
        None
    }
    fn child_nodes(&self, _: NodeRef) -> Vec<NodeRef> {
        Vec::new()
    }
    fn text_content(&self, _: NodeRef) -> Option<String> {
        None
    }
    fn set_text_content(&mut self, _: NodeRef, _: &str) -> Result<(), DomException> {
        Ok(())
    }
    fn insert_before(
        &mut self,
        _: NodeRef,
        _: NodeRef,
        _: Option<NodeRef>,
    ) -> Result<(), DomException> {
        Err(DomException::HierarchyRequest)
    }
    fn remove_child(&mut self, _: NodeRef, _: NodeRef) -> Result<(), DomException> {
        Err(DomException::NotFound)
    }
    fn replace_child(&mut self, _: NodeRef, _: NodeRef, _: NodeRef) -> Result<(), DomException> {
        Err(DomException::NotFound)
    }
    fn attribute(&self, _: NodeRef, _: &str) -> Option<String> {
        None
    }
    fn set_attribute(&mut self, _: NodeRef, _: &str, _: &str) -> Result<(), DomException> {
        Ok(())
    }
    fn remove_attribute(&mut self, _: NodeRef, _: &str) -> Result<(), DomException> {
        Ok(())
    }
    fn pin(&mut self, _: NodeRef) {
        self.pins += 1;
    }
    fn unpin(&mut self, _: NodeRef) {
        self.pins -= 1;
    }
    fn set_timeout(&mut self, delay_ms: f64) -> TimerId {
        self.timers.push(delay_ms);
        TimerId(self.timers.len() as u32)
    }
    fn clear_timeout(&mut self, id: TimerId) {
        self.cleared.push(id.0);
    }
    fn console(&mut self, level: ConsoleLevel, message: &str) {
        self.console.push((level, message.to_owned()));
    }
    fn location(&self) -> String {
        "https://a.test/page".into()
    }
    fn navigate(&mut self, url: &str, action: script::LocationAction) -> Result<(), DomException> {
        if url.contains(' ') {
            return Err(DomException::Syntax);
        }
        self.navigations.push((url.to_owned(), action));
        Ok(())
    }
    fn history_go(&mut self, delta: i32) {
        self.history.push(delta);
    }
    fn history_length(&self) -> u32 {
        3
    }
}

fn engine() -> (QuickJsEngine, Rc<RefCell<Mock>>) {
    let mock = Rc::new(RefCell::new(Mock::default()));
    let host: Rc<RefCell<dyn ScriptHost>> = mock.clone();
    (QuickJsEngine::new(host).unwrap(), mock)
}

fn logged(mock: &Rc<RefCell<Mock>>) -> Vec<String> {
    mock.borrow()
        .console
        .iter()
        .map(|(_, m)| m.clone())
        .collect()
}

#[test]
fn console_joins_its_arguments_at_each_level() {
    let (mut engine, mock) = engine();
    engine
        .eval_classic("console.log('a', 1, true); console.warn('w')", "t.js")
        .unwrap();
    assert_eq!(
        mock.borrow().console,
        [
            (ConsoleLevel::Log, "a 1 true".to_owned()),
            (ConsoleLevel::Warn, "w".to_owned())
        ]
    );
    assert_eq!(mock.borrow().pins, 1, "the document's wrapper");
}

#[test]
fn timers_run_once_with_their_arguments_and_clear() {
    // AC-33
    let (mut engine, mock) = engine();
    engine
        .eval_classic(
            "var t = setTimeout(function (a, b) { console.log(a + b); }, 10, 2, 3);
             var u = setTimeout('console.log(\"string\")', -5);
             var v = setTimeout(function () { console.log('never'); });
             clearTimeout(v);",
            "t.js",
        )
        .unwrap();
    // Raw delays: the host clamps them.
    assert_eq!(mock.borrow().timers, [10.0, -5.0, 0.0]);
    assert_eq!(mock.borrow().cleared, [3]);
    engine.run_timer(TimerId(1)).unwrap();
    engine.run_timer(TimerId(1)).unwrap();
    engine.run_timer(TimerId(2)).unwrap();
    engine.run_timer(TimerId(3)).unwrap();
    assert_eq!(logged(&mock), ["5", "string"]);
}

#[test]
fn lifecycle_events_reach_document_then_window_listeners() {
    // AC-33
    let (mut engine, mock) = engine();
    engine
        .eval_classic(
            "document.addEventListener('DOMContentLoaded', function (e) {
                 console.log('doc', e.type, e.target === document, this === document);
             });
             window.addEventListener('DOMContentLoaded', function (e) {
                 console.log('win', e.currentTarget === window);
             });
             addEventListener('load', function () { throw new Error('boom'); });
             addEventListener('load', function () { console.log('load'); });",
            "t.js",
        )
        .unwrap();
    engine
        .fire_event(EventTarget::Document, "DOMContentLoaded")
        .unwrap();
    engine.fire_event(EventTarget::Window, "load").unwrap();
    let log = logged(&mock);
    assert_eq!(log[0], "doc DOMContentLoaded true true");
    assert_eq!(log[1], "win true");
    assert!(log[2].starts_with("Uncaught Error: boom"), "{}", log[2]);
    assert_eq!(
        log[3], "load",
        "one throwing listener does not stop the rest"
    );
}

#[test]
fn exceptions_carry_their_url_and_line_and_dom_errors_their_names() {
    // AC-32
    let (mut engine, mock) = engine();
    let err = engine
        .eval_classic("var x = 1;\nnull.f();", "https://a.test/s.js")
        .unwrap_err();
    let ScriptError::Exception { message, url, line } = err else {
        panic!("{err:?}");
    };
    assert!(message.starts_with("TypeError"), "{message}");
    assert_eq!((url.as_str(), line), ("https://a.test/s.js", 2));

    engine
        .eval_classic(
            "try { document.createElement('x y'); } catch (e) {
                 console.log(e instanceof DOMException, e.name);
             }
             try { document.querySelector('##'); } catch (e) { console.log(e.name); }",
            "t.js",
        )
        .unwrap();
    assert_eq!(logged(&mock), ["true InvalidCharacterError", "SyntaxError"]);
}

#[test]
fn a_runaway_task_is_stopped_and_later_tasks_still_run() {
    // AC-35
    let (mut engine, mock) = engine();
    let started = Instant::now();
    let err = engine
        .eval_classic(
            "setTimeout(function () { console.log('later'); }, 0); for (;;) {}",
            "loop.js",
        )
        .unwrap_err();
    assert_eq!(err, ScriptError::Interrupted);
    let took = started.elapsed();
    assert!(
        took >= TASK_DEADLINE && took < TASK_DEADLINE * 2,
        "{took:?}"
    );
    engine.run_timer(TimerId(1)).unwrap();
    assert_eq!(logged(&mock), ["later"]);
}

#[test]
fn a_heap_bomb_runs_out_of_memory_and_the_page_goes_on() {
    // AC-35
    let (mut engine, mock) = engine();
    let err = engine
        .eval_classic(
            "var a = []; for (;;) { a.push(new Array(1 << 20).fill(1)); }",
            "bomb.js",
        )
        .unwrap_err();
    assert_eq!(err, ScriptError::OutOfMemory);
    engine
        .eval_classic("a = null; console.log('still here');", "after.js")
        .unwrap();
    assert_eq!(logged(&mock), ["still here"]);
}

#[test]
fn promise_jobs_run_at_the_checkpoint() {
    let (mut engine, mock) = engine();
    engine
        .eval_classic(
            "Promise.resolve(1).then(function (v) { console.log('then', v); });
             console.log('sync');",
            "t.js",
        )
        .unwrap();
    assert_eq!(logged(&mock), ["sync"]);
    engine.run_microtasks().unwrap();
    assert_eq!(logged(&mock), ["sync", "then 1"]);
}

#[test]
fn location_and_history_ask_the_host() {
    // Spec 0008, AC-27
    let (mut engine, mock) = engine();
    engine
        .eval_classic(
            "console.log(location.href, String(location), history.length);
             location.href = 'b.html';
             location.assign('c.html');
             location.replace('d.html');
             location.reload();
             history.back(); history.forward(); history.go(-2); history.go('x');
             try { location.assign('bad url'); } catch (e) { console.log(e.name); }",
            "t.js",
        )
        .unwrap();
    use script::LocationAction::{Push, Reload, Replace};
    assert_eq!(
        mock.borrow().navigations,
        [
            ("b.html".to_owned(), Push),
            ("c.html".to_owned(), Push),
            ("d.html".to_owned(), Replace),
            (String::new(), Reload),
        ]
    );
    assert_eq!(mock.borrow().history, [-1, 1, -2, 0]);
    assert_eq!(
        logged(&mock),
        ["https://a.test/page https://a.test/page 3", "SyntaxError"]
    );
}
