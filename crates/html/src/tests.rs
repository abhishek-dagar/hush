//! Parser tests over a plain tree sink: what pages rely on, plus the
//! property tests for AC-17. The html5lib suites live beside the tokenizer
//! and the tree builder.

use base::atoms::QualName;

use super::*;

/// A plain tree: node 0 is the document.
#[derive(Default)]
struct TestSink {
    nodes: Vec<TestNode>,
    quirks: QuirksMode,
}

enum TestData {
    Document,
    Doctype(String),
    Element(QualName, Vec<Attribute>),
    Text(String),
    Comment(String),
}

struct TestNode {
    parent: Option<usize>,
    children: Vec<usize>,
    data: TestData,
}

impl TestSink {
    fn new() -> Self {
        Self {
            nodes: vec![TestNode {
                parent: None,
                children: Vec::new(),
                data: TestData::Document,
            }],
            quirks: QuirksMode::default(),
        }
    }

    fn add(&mut self, data: TestData) -> usize {
        self.nodes.push(TestNode {
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

    /// The tree in the html5lib dump format: `| ` and two spaces per level.
    fn dump(&self) -> String {
        let mut out = String::new();
        let mut stack: Vec<(usize, usize)> = self.nodes[0]
            .children
            .iter()
            .rev()
            .map(|&c| (c, 0))
            .collect();
        while let Some((id, depth)) = stack.pop() {
            let node = &self.nodes[id];
            let line = match &node.data {
                TestData::Document => String::new(),
                TestData::Doctype(name) => format!("<!DOCTYPE {name}>"),
                TestData::Element(name, attrs) => {
                    let mut line = format!("<{}>", name.local);
                    for attr in attrs {
                        line.push_str(&format!(" {}=\"{}\"", attr.name.local, attr.value));
                    }
                    line
                }
                TestData::Text(text) => format!("\"{text}\""),
                TestData::Comment(text) => format!("<!-- {text} -->"),
            };
            out.push_str(&format!("| {}{line}\n", "  ".repeat(depth)));
            stack.extend(node.children.iter().rev().map(|&c| (c, depth + 1)));
        }
        out
    }

    fn depth(&self) -> usize {
        let mut deepest = 0;
        let mut stack = vec![(0usize, 0usize)];
        while let Some((id, depth)) = stack.pop() {
            deepest = deepest.max(depth);
            stack.extend(self.nodes[id].children.iter().map(|&c| (c, depth + 1)));
        }
        deepest
    }
}

impl TreeSink for TestSink {
    type Handle = usize;

    fn document(&self) -> usize {
        0
    }

    fn create_element(&mut self, name: QualName, attrs: Vec<Attribute>, _: CreateFlags) -> usize {
        self.add(TestData::Element(name, attrs))
    }

    fn create_comment(&mut self, text: &str) -> usize {
        self.add(TestData::Comment(text.into()))
    }

    fn append(&mut self, parent: usize, child: NodeOrText<'_, usize>) {
        let child = match child {
            NodeOrText::Node(node) => node,
            NodeOrText::Text(text) => {
                if let Some(&last) = self.nodes[parent].children.last()
                    && let TestData::Text(existing) = &mut self.nodes[last].data
                {
                    existing.push_str(text);
                    return;
                }
                self.add(TestData::Text(text.into()))
            }
        };
        self.detach(child);
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.push(child);
    }

    fn insert_before(&mut self, sibling: usize, child: NodeOrText<'_, usize>) {
        let Some(parent) = self.nodes[sibling].parent else {
            return;
        };
        let child = match child {
            NodeOrText::Node(node) => node,
            NodeOrText::Text(text) => {
                // Text joins a text node just before the sibling.
                let children = &self.nodes[parent].children;
                let previous = children
                    .iter()
                    .position(|&c| c == sibling)
                    .and_then(|i| i.checked_sub(1))
                    .map(|i| children[i]);
                if let Some(previous) = previous
                    && let TestData::Text(existing) = &mut self.nodes[previous].data
                {
                    existing.push_str(text);
                    return;
                }
                self.add(TestData::Text(text.into()))
            }
        };
        self.detach(child);
        let at = self.nodes[parent]
            .children
            .iter()
            .position(|&c| c == sibling)
            .unwrap_or(0);
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.insert(at, child);
    }

    fn append_doctype(&mut self, name: &str, _: &str, _: &str) {
        let doctype = self.add(TestData::Doctype(name.into()));
        self.append(0, NodeOrText::Node(doctype));
    }

    fn add_attrs_if_missing(&mut self, target: usize, attrs: Vec<Attribute>) {
        if let TestData::Element(_, existing) = &mut self.nodes[target].data {
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
        template
    }

    fn parent(&self, node: usize) -> Option<usize> {
        self.nodes[node].parent
    }

    fn set_quirks_mode(&mut self, mode: QuirksMode) {
        self.quirks = mode;
    }

    fn element_name(&self, node: usize) -> &QualName {
        match &self.nodes[node].data {
            TestData::Element(name, _) => name,
            _ => panic!("not an element"),
        }
    }
}

fn parse_chunks(chunks: &[&[u8]]) -> TestSink {
    let mut sink = TestSink::new();
    let mut parser = Parser::new(ParserOptions::default());
    for chunk in chunks {
        assert_eq!(parser.feed(&mut sink, chunk), ParseStep::Continue);
    }
    assert_eq!(parser.end(&mut sink), ParseStep::Done);
    sink
}

fn parse(html: &str) -> String {
    parse_chunks(&[html.as_bytes()]).dump()
}

#[test]
fn scripting_pauses_after_each_script_until_resumed() {
    // AC-31
    // A charset hint, so nothing waits for the `<meta>` prescan.
    let options = || ParserOptions {
        scripting: true,
        encoding_hint: Some("utf-8".into()),
        ..ParserOptions::default()
    };
    let mut sink = TestSink::new();
    let mut parser = Parser::new(options());
    let step = parser.feed(&mut sink, b"<p>a<script>run()</script><p>b");
    let ParseStep::RunScript(script) = step else {
        panic!("expected a pause, got {step:?}: {}", sink.dump());
    };
    assert_eq!(&*sink.element_name(script).local, "script");
    let paused = sink.dump();
    assert!(
        paused.contains("run()") && !paused.contains("\"b\""),
        "{paused}"
    );

    // Input fed while paused only queues.
    assert_eq!(
        parser.feed(&mut sink, b"<p>c"),
        ParseStep::RunScript(script)
    );
    assert_eq!(parser.end(&mut sink), ParseStep::RunScript(script));
    assert_eq!(parser.paused_at(), Some(script));
    assert!(!sink.dump().contains("\"c\""));

    assert_eq!(parser.resume(&mut sink), ParseStep::Done);
    let done = sink.dump();
    assert!(done.contains("\"b\"") && done.contains("\"c\""), "{done}");

    // A script the input ends inside never pauses; nor does anything with
    // scripting off.
    let mut sink = TestSink::new();
    let mut parser = Parser::new(options());
    assert_eq!(parser.feed(&mut sink, b"<script>cut"), ParseStep::Continue);
    assert_eq!(parser.end(&mut sink), ParseStep::Done);
    assert!(parse("<script>x</script><p>y").contains("\"y\""));
}

#[test]
fn a_bare_fragment_gets_html_head_and_body() {
    assert_eq!(
        parse("Hello <b>world</b>"),
        "| <html>\n|   <head>\n|   <body>\n|     \"Hello \"\n|     <b>\n|       \"world\"\n"
    );
}

#[test]
fn head_elements_stay_in_head_and_raw_text_stays_text() {
    let tree = parse(
        "<!DOCTYPE html><html lang=en><title>A &amp; B</title><style>p > a { }</style>\
         <script>if (a<b) x()</script><p>text",
    );
    assert_eq!(
        tree,
        "| <!DOCTYPE html>\n| <html> lang=\"en\"\n|   <head>\n|     <title>\n|       \"A & B\"\n\
         |     <style>\n|       \"p > a { }\"\n|     <script>\n|       \"if (a<b) x()\"\n\
         |   <body>\n|     <p>\n|       \"text\"\n"
    );
}

#[test]
fn implied_end_tags_close_paragraphs_list_items_and_headings() {
    let tree = parse("<p>one<p>two<div>three</div><ul><li>a<li>b</ul><h1>x<h2>y");
    assert_eq!(
        tree,
        "| <html>\n|   <head>\n|   <body>\n|     <p>\n|       \"one\"\n|     <p>\n|       \"two\"\n\
         |     <div>\n|       \"three\"\n|     <ul>\n|       <li>\n|         \"a\"\n\
         |       <li>\n|         \"b\"\n|     <h1>\n|       \"x\"\n|     <h2>\n|       \"y\"\n"
    );
}

#[test]
fn void_elements_and_stray_end_tags() {
    let tree = parse("<p>a<br>b<img src=x></span>c</p></p>");
    assert_eq!(
        tree,
        "| <html>\n|   <head>\n|   <body>\n|     <p>\n|       \"a\"\n|       <br>\n\
         |       \"b\"\n|       <img> src=\"x\"\n|       \"c\"\n|     <p>\n"
    );
}

#[test]
fn every_byte_split_builds_the_same_tree() {
    // AC-5: bytes may stop anywhere, even inside a UTF-8 sequence.
    let page = "<!DOCTYPE html><title>Café</title><p class=x>naïve &amp; <b>bold</b></p>\
                <style>a{}</style><!-- note --><ul><li>one<li>two</ul>";
    let whole = parse(page);
    let bytes = page.as_bytes();
    for split in 1..bytes.len() {
        let (a, b) = bytes.split_at(split);
        assert_eq!(parse_chunks(&[a, b]).dump(), whole, "split at byte {split}");
    }
}

#[test]
fn deep_nesting_stops_at_the_depth_cap() {
    // AC-17: elements past the cap attach at the last level.
    let html = "<div>".repeat(MAX_DEPTH + 100);
    let sink = parse_chunks(&[html.as_bytes()]);
    // The document, then `html` at depth 1 and `body` at 2 count toward
    // the open element stack, which holds at most MAX_DEPTH elements.
    assert_eq!(sink.depth(), MAX_DEPTH + 1);
}

#[test]
fn broken_input_never_panics() {
    // AC-17; the property test below covers arbitrary input.
    for input in [
        "<",
        "</",
        "<!",
        "<!-",
        "&",
        "&#",
        "&#x",
        "<a <b c=\"",
        "<table><tr><td>x</p></table></body></html><p>after",
        "\0<p>\0</p>",
        "<title>never closed",
        "<script>never closed",
        "</html></body>text",
        "<!DOCTYPE",
    ] {
        let _ = parse(input);
    }
}

#[test]
fn the_header_charset_decodes_the_bytes_and_a_bom_wins() {
    let mut sink = TestSink::new();
    let mut parser = Parser::new(ParserOptions {
        encoding_hint: Some("windows-1252".into()),
        ..ParserOptions::default()
    });
    parser.feed(&mut sink, b"<p>caf\xE9");
    parser.end(&mut sink);
    assert_eq!(parser.encoding(), "windows-1252");
    assert!(sink.dump().contains("\"café\""));

    let mut sink = TestSink::new();
    let mut parser = Parser::new(ParserOptions {
        encoding_hint: Some("windows-1252".into()),
        ..ParserOptions::default()
    });
    parser.feed(&mut sink, b"\xEF\xBB\xBF<p>caf\xC3\xA9");
    parser.end(&mut sink);
    assert_eq!(parser.encoding(), "UTF-8");
    assert!(sink.dump().contains("\"café\""));
}

/// Parses `bytes` in two chunks with the header charset `hint`, and gives
/// the encoding picked plus the tree.
fn decode(hint: Option<&str>, bytes: &[u8]) -> (&'static str, String) {
    let mut sink = TestSink::new();
    let mut parser = Parser::new(ParserOptions {
        encoding_hint: hint.map(Into::into),
        ..ParserOptions::default()
    });
    let (first, second) = bytes.split_at(bytes.len() / 2);
    parser.feed(&mut sink, first);
    parser.feed(&mut sink, second);
    parser.end(&mut sink);
    (parser.encoding(), sink.dump())
}

#[test]
fn the_encoding_is_picked_in_the_order_of_ac_6() {
    // "привет" in each step's encoding; no other encoding in the chain
    // decodes these bytes to it.
    // 1. A UTF-16LE byte order mark wins over the header and the meta.
    let mut utf16 = vec![0xFF, 0xFE];
    for unit in "<meta charset=koi8-r><p>привет".encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }
    let (encoding, tree) = decode(Some("windows-1251"), &utf16);
    assert_eq!(encoding, "UTF-16LE");
    assert!(tree.contains("\"привет\""), "{tree}");

    // 2. The header charset wins over the meta.
    let (koi8, _, _) = encoding_rs::KOI8_R.encode("<meta charset=windows-1251><p>привет");
    let (encoding, tree) = decode(Some("koi8-r"), &koi8);
    assert_eq!(encoding, "KOI8-R");
    assert!(tree.contains("\"привет\""), "{tree}");

    // 3. With no header charset, the meta prescan decides, even when the
    // meta arrives in the second chunk.
    let mut page = b"<!DOCTYPE html><!-- padding -->".to_vec();
    page.extend_from_slice(&koi8);
    let (encoding, tree) = decode(None, &page);
    assert_eq!(encoding, "windows-1251");
    let (cp1251, _, _) = encoding_rs::WINDOWS_1251.encode("<p>привет");
    let mut page = b"<meta charset=windows-1251>".to_vec();
    page.extend_from_slice(&cp1251);
    let (encoding, tree2) = decode(None, &page);
    assert_eq!(encoding, "windows-1251");
    assert!(tree2.contains("\"привет\""), "{tree2}");
    assert!(
        !tree.contains("\"привет\""),
        "the meta, not the bytes, decided"
    );

    // 4. Nothing declared: windows-1252.
    let (encoding, tree) = decode(None, b"<p>caf\xE9 \x80");
    assert_eq!(encoding, "windows-1252");
    assert!(tree.contains("\"café €\""), "{tree}");
}

#[test]
fn an_unknown_header_charset_falls_through_to_the_prescan() {
    let (encoding, _) = decode(
        Some("no-such-charset"),
        b"<meta charset=utf-8><p>caf\xC3\xA9",
    );
    assert_eq!(encoding, "UTF-8");
}

#[test]
fn plain_text_is_one_pre_and_markup_stays_text() {
    let mut sink = TestSink::new();
    let mut parser = Parser::new(ParserOptions {
        encoding_hint: Some("utf-8".into()),
        plain_text: true,
        ..ParserOptions::default()
    });
    parser.feed(&mut sink, b"\nfirst <b>line</b>\n");
    parser.feed(&mut sink, b"second &amp; last");
    parser.end(&mut sink);
    assert_eq!(
        sink.dump(),
        "| <html>\n|   <head>\n|   <body>\n|     <pre>\n\
         |       \"\nfirst <b>line</b>\nsecond &amp; last\"\n"
    );
}

#[test]
fn newlines_are_normalized_and_the_one_after_pre_is_dropped() {
    let tree = parse("<pre>\r\nline\r\nnext\rlast</pre>");
    assert!(tree.contains("\"line\nnext\nlast\""), "{tree}");
}

/// Pieces of markup that reach the tricky parts of the tree builder:
/// tables, formatting elements, foreign content, templates and raw text.
const PIECES: &[&str] = &[
    "<table>",
    "</table>",
    "<tr>",
    "<td>",
    "</td>",
    "<caption>",
    "<colgroup>",
    "<col>",
    "<tbody>",
    "<b>",
    "</b>",
    "<i>",
    "</i>",
    "<a href=x>",
    "</a>",
    "<nobr>",
    "<p>",
    "</p>",
    "<div>",
    "</div>",
    "<li>",
    "<ul>",
    "<select>",
    "<option>",
    "</select>",
    "<svg>",
    "</svg>",
    "<math>",
    "<mi>",
    "<foreignObject>",
    "<template>",
    "</template>",
    "<frameset>",
    "<noscript>",
    "<style>",
    "</style>",
    "<script>",
    "</script>",
    "<textarea>",
    "<title>",
    "<pre>",
    "<plaintext>",
    "<![CDATA[",
    "]]>",
    "<!--",
    "-->",
    "<!DOCTYPE html>",
    "&amp",
    "&#x",
    "x",
    " ",
    "\n",
    "\0",
    "<",
    "</",
    ">",
    "/>",
    "\"",
    "'",
    "=",
];

fn markup() -> impl proptest::strategy::Strategy<Value = Vec<u8>> {
    use proptest::prelude::*;
    prop_oneof![
        proptest::collection::vec(any::<u8>(), 0..400),
        proptest::collection::vec(proptest::sample::select(PIECES), 0..80)
            .prop_map(|pieces| pieces.concat().into_bytes()),
    ]
}

/// Splits `bytes` at the given cut points, in order, deduplicated.
fn split_at<'a>(bytes: &'a [u8], cuts: &[usize]) -> Vec<&'a [u8]> {
    let mut cuts: Vec<usize> = cuts.iter().map(|c| c % (bytes.len() + 1)).collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut chunks = Vec::new();
    let mut start = 0;
    for cut in cuts {
        chunks.push(&bytes[start..cut]);
        start = cut;
    }
    chunks.push(&bytes[start..]);
    chunks
}

proptest::proptest! {
    /// AC-17: any bytes, cut anywhere, parse without a panic, stay within
    /// the depth cap, and build the same tree as the whole input at once.
    #[test]
    fn any_bytes_in_any_chunks_build_the_same_tree(
        bytes in markup(),
        cuts in proptest::collection::vec(proptest::prelude::any::<usize>(), 0..8),
    ) {
        let whole = parse_chunks(&[&bytes]);
        let chunked = parse_chunks(&split_at(&bytes, &cuts));
        proptest::prop_assert_eq!(whole.dump(), chunked.dump());
        proptest::prop_assert!(whole.depth() <= MAX_DEPTH + 1);
    }
}
