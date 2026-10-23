//! The `html5lib-tests` tree construction suite (spec 0004, AC-16).
//!
//! Every case in `tests/html5lib-tests/tree-construction/*.dat` is parsed
//! through the whole `Parser` (bytes, decoder, tokenizer, tree builder)
//! into a plain test sink, printed in the suite's tree format and compared.
//! A case marked `#script-on` or `#script-off` runs in that mode; a case
//! with neither runs in both. Cases under `scripted/` need
//! `document.write`, which slice 1 does not have, so they are not run.
//!
//! Known failures live in `tests/known-failures.txt`, one run per line as
//! `<file>:<case number>:<on|off> <reason>`. The test fails when a run
//! fails that is not listed, when a listed run passes (so the list only
//! shrinks), or when the list holds more than 2% of the runs.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use base::atoms::QualName;
use base::ns;

use crate::{
    Attribute, CreateFlags, NodeOrText, ParseStep, Parser, ParserOptions, QuirksMode, TreeSink,
};

/// A plain tree: node 0 is the document.
struct Sink {
    nodes: Vec<Node>,
}

enum Data {
    Document,
    Doctype(String, String, String),
    /// An element, with its template contents fragment when it has one.
    Element(QualName, Vec<Attribute>, Option<usize>),
    Text(String),
    Comment(String),
    Fragment,
}

struct Node {
    parent: Option<usize>,
    children: Vec<usize>,
    data: Data,
}

impl Sink {
    fn new() -> Self {
        Self {
            nodes: vec![Node {
                parent: None,
                children: Vec::new(),
                data: Data::Document,
            }],
        }
    }

    fn add(&mut self, data: Data) -> usize {
        self.nodes.push(Node {
            parent: None,
            children: Vec::new(),
            data,
        });
        self.nodes.len() - 1
    }

    fn detach(&mut self, node: usize) {
        if let Some(parent) = self.nodes[node].parent.take() {
            self.nodes[parent].children.retain(|&c| c != node);
        }
    }

    /// Makes `child` a node, or joins its text to `neighbor`.
    fn node_for(&mut self, child: NodeOrText<'_, usize>, neighbor: Option<usize>) -> Option<usize> {
        match child {
            NodeOrText::Node(node) => Some(node),
            NodeOrText::Text(text) => {
                if let Some(neighbor) = neighbor
                    && let Data::Text(existing) = &mut self.nodes[neighbor].data
                {
                    existing.push_str(text);
                    return None;
                }
                Some(self.add(Data::Text(text.to_owned())))
            }
        }
    }

    fn dump(&self, parent: usize, depth: usize, out: &mut String) {
        for &child in &self.nodes[parent].children {
            let indent = format!("| {}", "  ".repeat(depth));
            match &self.nodes[child].data {
                Data::Document | Data::Fragment => {}
                Data::Doctype(name, public, system) => {
                    if public.is_empty() && system.is_empty() {
                        let _ = writeln!(out, "{indent}<!DOCTYPE {name}>");
                    } else {
                        let _ =
                            writeln!(out, "{indent}<!DOCTYPE {name} \"{public}\" \"{system}\">");
                    }
                }
                Data::Element(name, attrs, contents) => {
                    let prefix = if name.ns == ns!("http://www.w3.org/2000/svg") {
                        "svg "
                    } else if name.ns == ns!("http://www.w3.org/1998/Math/MathML") {
                        "math "
                    } else {
                        ""
                    };
                    let _ = writeln!(out, "{indent}<{prefix}{}>", name.local);
                    let attrs: BTreeMap<String, &str> = attrs
                        .iter()
                        .map(|attr| {
                            let shown = match &attr.name.prefix {
                                Some(prefix) => format!("{prefix} {}", attr.name.local),
                                None if attr.name.ns == ns!("http://www.w3.org/2000/xmlns/") => {
                                    format!("xmlns {}", attr.name.local)
                                }
                                None => attr.name.local.to_string(),
                            };
                            (shown, attr.value.as_str())
                        })
                        .collect();
                    for (attr, value) in attrs {
                        let _ = writeln!(out, "{indent}  {attr}=\"{value}\"");
                    }
                    if let Some(contents) = contents {
                        let _ = writeln!(out, "{indent}  content");
                        self.dump(*contents, depth + 2, out);
                    }
                }
                Data::Text(text) => {
                    let _ = writeln!(out, "{indent}\"{text}\"");
                }
                Data::Comment(text) => {
                    let _ = writeln!(out, "{indent}<!-- {text} -->");
                }
            }
            self.dump(child, depth + 1, out);
        }
    }
}

impl TreeSink for Sink {
    type Handle = usize;

    fn document(&self) -> usize {
        0
    }

    fn create_element(
        &mut self,
        name: QualName,
        attrs: Vec<Attribute>,
        flags: CreateFlags,
    ) -> usize {
        let contents = flags.template.then(|| self.add(Data::Fragment));
        self.add(Data::Element(name, attrs, contents))
    }

    fn create_comment(&mut self, text: &str) -> usize {
        self.add(Data::Comment(text.to_owned()))
    }

    fn append(&mut self, parent: usize, child: NodeOrText<'_, usize>) {
        let last = self.nodes[parent].children.last().copied();
        if let Some(node) = self.node_for(child, last) {
            self.detach(node);
            self.nodes[node].parent = Some(parent);
            self.nodes[parent].children.push(node);
        }
    }

    fn insert_before(&mut self, sibling: usize, child: NodeOrText<'_, usize>) {
        let Some(parent) = self.nodes[sibling].parent else {
            return;
        };
        let position = self.nodes[parent]
            .children
            .iter()
            .position(|&c| c == sibling);
        let previous = position
            .and_then(|i| i.checked_sub(1))
            .map(|i| self.nodes[parent].children[i]);
        if let Some(node) = self.node_for(child, previous) {
            self.detach(node);
            let index = self.nodes[parent]
                .children
                .iter()
                .position(|&c| c == sibling)
                .unwrap_or(0);
            self.nodes[node].parent = Some(parent);
            self.nodes[parent].children.insert(index, node);
        }
    }

    fn append_doctype(&mut self, name: &str, public_id: &str, system_id: &str) {
        let doctype = self.add(Data::Doctype(
            name.to_owned(),
            public_id.to_owned(),
            system_id.to_owned(),
        ));
        self.append(0, NodeOrText::Node(doctype));
    }

    fn add_attrs_if_missing(&mut self, target: usize, attrs: Vec<Attribute>) {
        if let Data::Element(_, existing, _) = &mut self.nodes[target].data {
            for attr in attrs {
                if !existing.iter().any(|a| a.name == attr.name) {
                    existing.push(attr);
                }
            }
        }
    }

    fn remove_from_parent(&mut self, target: usize) {
        self.detach(target);
    }

    fn reparent_children(&mut self, from: usize, to: usize) {
        for child in std::mem::take(&mut self.nodes[from].children) {
            self.nodes[child].parent = Some(to);
            self.nodes[to].children.push(child);
        }
    }

    fn template_contents(&self, template: usize) -> usize {
        match &self.nodes[template].data {
            Data::Element(_, _, Some(contents)) => *contents,
            _ => template,
        }
    }

    fn parent(&self, node: usize) -> Option<usize> {
        self.nodes[node].parent
    }

    fn set_quirks_mode(&mut self, _: QuirksMode) {}

    fn element_name(&self, node: usize) -> &QualName {
        match &self.nodes[node].data {
            Data::Element(name, _, _) => name,
            _ => unreachable!("the tree builder only asks for element names"),
        }
    }
}

/// One case from a `.dat` file.
struct Case {
    data: String,
    fragment: Option<String>,
    scripting: Option<bool>,
    expected: String,
}

/// A case while its lines are read.
#[derive(Default)]
struct Draft<'a> {
    data: Vec<&'a str>,
    fragment: Option<String>,
    scripting: Option<bool>,
    expected: Vec<&'a str>,
}

impl Draft<'_> {
    fn finish(self) -> Case {
        Case {
            data: self.data.join("\n"),
            fragment: self.fragment,
            scripting: self.scripting,
            expected: self.expected.join("\n").trim_end_matches('\n').to_owned(),
        }
    }
}

fn read_cases(text: &str) -> Vec<Case> {
    let mut cases = Vec::new();
    let mut section = "";
    let mut current: Option<Draft<'_>> = None;
    for line in text.lines() {
        match line {
            "#data" => {
                cases.extend(current.take().map(Draft::finish));
                current = Some(Draft::default());
                section = "data";
                continue;
            }
            "#errors" | "#new-errors" | "#document-fragment" | "#document" => {
                section = line;
                continue;
            }
            "#script-on" | "#script-off" => {
                if let Some(case) = &mut current {
                    case.scripting = Some(line == "#script-on");
                }
                continue;
            }
            _ => {}
        }
        let Some(case) = &mut current else {
            continue;
        };
        match section {
            "data" => case.data.push(line),
            "#document-fragment" => case.fragment = Some(line.trim().to_owned()),
            "#document" => case.expected.push(line),
            _ => {}
        }
    }
    cases.extend(current.map(Draft::finish));
    cases
}

/// Parses a case and prints its tree.
fn run(case: &Case, scripting: bool) -> String {
    let options = ParserOptions {
        encoding_hint: Some("utf-8".to_owned()),
        scripting,
        plain_text: false,
    };
    let mut sink = Sink::new();
    let mut out = String::new();
    match &case.fragment {
        None => {
            let mut parser = Parser::new(options);
            parse_all(&mut parser, &mut sink, case.data.as_bytes());
            sink.dump(0, 0, &mut out);
        }
        Some(context) => {
            let (ns, local) = match context.split_once(' ') {
                Some(("svg", local)) => (ns!("http://www.w3.org/2000/svg"), local),
                Some(("math", local)) => (ns!("http://www.w3.org/1998/Math/MathML"), local),
                _ => (ns!("http://www.w3.org/1999/xhtml"), context.as_str()),
            };
            let context = QualName {
                prefix: None,
                ns,
                local: local.into(),
            };
            let (mut parser, root) = Parser::fragment(&mut sink, options, context);
            parse_all(&mut parser, &mut sink, case.data.as_bytes());
            sink.dump(root, 0, &mut out);
        }
    }
    out.trim_end_matches('\n').to_owned()
}

/// Feeds `data` and ends it, going straight on past every script pause:
/// the suite has no scripts to run.
fn parse_all<S: TreeSink<Handle = usize>>(parser: &mut Parser<usize>, sink: &mut S, data: &[u8]) {
    let mut step = parser.feed(sink, data);
    while let ParseStep::RunScript(_) = step {
        step = parser.resume(sink);
    }
    step = parser.end(sink);
    while let ParseStep::RunScript(_) = step {
        step = parser.resume(sink);
    }
}

#[test]
fn every_tree_construction_case_passes_or_is_a_known_failure() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let known_text = std::fs::read_to_string(root.join("known-failures.txt")).unwrap_or_default();
    let known: BTreeMap<&str, &str> = known_text
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| line.split_once(' ').unwrap_or((line, "")))
        .collect();
    for (id, reason) in &known {
        assert!(
            !reason.trim().is_empty(),
            "known failure {id} has no reason"
        );
    }

    let dir = root.join("html5lib-tests/tree-construction");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .map(|entries| entries.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    files.retain(|path| path.extension().is_some_and(|ext| ext == "dat"));
    files.sort();
    assert!(!files.is_empty(), "no tree construction files in {dir:?}");

    let mut runs = 0;
    let mut unexpected = Vec::new();
    let mut fixed = Vec::new();
    for path in &files {
        let file = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = std::fs::read_to_string(path).unwrap_or_default();
        for (number, case) in read_cases(&text).iter().enumerate() {
            let modes: &[bool] = match case.scripting {
                Some(true) => &[true],
                Some(false) => &[false],
                None => &[false, true],
            };
            for &scripting in modes {
                runs += 1;
                let id = format!(
                    "{file}:{}:{}",
                    number + 1,
                    if scripting { "on" } else { "off" }
                );
                let got = run(case, scripting);
                let passed = got == case.expected;
                match (passed, known.contains_key(id.as_str())) {
                    (false, false) => unexpected.push(format!(
                        "{id}\n#data\n{}\n#expected\n{}\n#got\n{got}\n",
                        case.data, case.expected
                    )),
                    (true, true) => fixed.push(id),
                    _ => {}
                }
            }
        }
    }

    let limit = runs / 50;
    assert!(
        unexpected.is_empty(),
        "{} of {runs} runs failed unexpectedly:\n\n{}",
        unexpected.len(),
        unexpected.join("\n")
    );
    assert!(
        fixed.is_empty(),
        "these known failures pass now; take them off the list: {fixed:?}"
    );
    assert!(
        known.len() <= limit,
        "{} known failures is over 2% of {runs} runs ({limit})",
        known.len()
    );
}
