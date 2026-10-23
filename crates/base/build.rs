//! Generates the static atom sets (spec 0002, base).
//!
//! Only the names the engine uses so far are listed. The full HTML and CSS
//! name lists arrive with the parsers; any other name still works as a
//! dynamic atom, just a little slower to create.

use std::env;
use std::io;
use std::path::PathBuf;

use string_cache_codegen::AtomType;

fn main() -> io::Result<()> {
    let out = PathBuf::from(
        env::var_os("OUT_DIR").ok_or_else(|| io::Error::other("OUT_DIR is not set"))?,
    );

    AtomType::new("atoms::LocalName", "local_name!")
        .with_atom_doc("An element or attribute name, without its prefix.")
        .with_static_set_doc("The local names known at build time.")
        .with_macro_doc("A static [`LocalName`] from a string literal.")
        .atoms([
            "html",
            "head",
            "body",
            "div",
            "p",
            "span",
            "section",
            "header",
            "footer",
            "main",
            "article",
            "aside",
            "blockquote",
            "nav",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
            "template",
            "iframe",
            "img",
            "br",
            "ol",
            "li",
            "slot",
            "style",
            "link",
            "href",
            "script",
            "id",
            "class",
            "title",
            "alt",
            "src",
            "width",
            "height",
            "start",
            "value",
            // Focus, form controls and their states (spec 0006).
            "a",
            "button",
            "input",
            "select",
            "textarea",
            "fieldset",
            "legend",
            "disabled",
            "tabindex",
            "type",
            "placeholder",
        ])
        .write_to_file(&out.join("local_name.rs"))?;

    AtomType::new("atoms::Namespace", "ns!")
        .with_atom_doc("A namespace URL.")
        .with_static_set_doc("The namespaces known at build time.")
        .with_macro_doc("A static [`Namespace`] from a string literal.")
        .atoms([
            "http://www.w3.org/1999/xhtml",
            "http://www.w3.org/2000/svg",
            "http://www.w3.org/1998/Math/MathML",
            "http://www.w3.org/1999/xlink",
            "http://www.w3.org/XML/1998/namespace",
            "http://www.w3.org/2000/xmlns/",
        ])
        .write_to_file(&out.join("namespace.rs"))?;

    AtomType::new("atoms::Prefix", "namespace_prefix!")
        .with_atom_doc("A namespace prefix, such as `xlink`.")
        .with_static_set_doc("The prefixes known at build time.")
        .with_macro_doc("A static [`Prefix`] from a string literal.")
        .atoms(["xml", "xlink", "xmlns"])
        .write_to_file(&out.join("prefix.rs"))?;

    AtomType::new("atoms::Atom", "atom!")
        .with_atom_doc("An interned string for `id` values and class names.")
        .with_static_set_doc("The general atoms known at build time.")
        .with_macro_doc("A static [`Atom`] from a string literal.")
        .atoms(["main", "content"])
        .write_to_file(&out.join("atom.rs"))?;

    println!("cargo::rerun-if-changed=build.rs");
    Ok(())
}
