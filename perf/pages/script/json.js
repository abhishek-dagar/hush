// json.js: the JSON kernel of the script workload page (spec 0007,
// Pinned pages). Parses and stringifies a generated ~1 MB JSON document.
// Deterministic: the same document every run, no randomness, no Date.
(function () {
    'use strict';

    // ~8,000 items of a few fields each: about 1 MB once stringified.
    function buildDoc() {
        const items = [];
        for (let i = 0; i < 8000; i++) {
            items.push({
                id: i,
                name: 'item-' + i + '-padding-padding-padding',
                score: ((i * 2654435761) >>> 0) / 4294967296,
                tags: ['shape', 'glyph', 'paint'].slice(i % 3),
                meta: { depth: i % 7, ok: (i & 1) === 0 },
            });
        }
        return { version: 1, generated: 'perf workload', items: items };
    }

    const doc = buildDoc();
    const text = JSON.stringify(doc);

    // Iteration count frozen once (spec 0007): changing it is a budget
    // change and needs a reason in budgets.toml. Sized so this kernel
    // takes a fraction of the 1.5 s script load budget on the reference
    // machine.
    const ITERATIONS = 20;
    let sink = 0;
    for (let i = 0; i < ITERATIONS; i++) {
        const parsed = JSON.parse(text);
        sink += parsed.items[0].id + parsed.items.length;
        sink += JSON.stringify(parsed).length;
    }
    // Kept alive so the engine cannot fold the work away.
    window.__jsonResult = sink;
})();
