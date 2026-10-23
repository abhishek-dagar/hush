# html5lib-tests (vendored)

The shared HTML parser conformance tests (spec 0004, child 0002, Testing),
copied from https://github.com/html5lib/html5lib-tests under its MIT
license (`LICENSE` here).

**Pinned at** `9329e64694e7835d0dcff9811e22856ef6ad16f9` (2026-06-22).

Why this commit and not the newest:

- `224991e` (2026-06-26) removed `tree-construction/`, which now lives in
  web-platform-tests (`html/syntax/parsing`). `9329e64` is the last commit
  that has both folders.
- `c777c40` (2026-10-01) changed the tokenizer tests for a proposed
  processing instruction token (`<?foo>`), which the HTML standard and
  shipping browsers do not have yet. Before it, `<?foo>` is a bogus comment,
  as it is in every browser today.

Only `tokenizer/`, `tree-construction/` and `LICENSE` are copied. To move
the pin, copy those three again from the new commit and update this file.
