// regex.js: the regular expression kernel of the script workload page
// (spec 0007, Pinned pages). Runs a set of patterns over generated text.
// Deterministic: the same text every run.
(function () {
    'use strict';

    // ~280 kB of repeating sentences with varying numbers.
    let text = '';
    for (let i = 0; i < 4000; i++) {
        text += 'The renderer painted frame ' + i + ' with ' + (i % 17) +
            ' glyphs at offset ' + ((i * 37) % 1000) + ' on display ' +
            (i % 3) + '. ';
    }

    const patterns = [
        /frame \d+/g,
        /\bwith \d+ glyphs\b/g,
        /offset \d{1,3}/g,
        /display [0-2]/g,
        /painted frame/g,
        /frame (\d+) with (\d+) glyphs at offset (\d+)/g,
    ];

    // Iteration count frozen once (spec 0007): changing it is a budget
    // change and needs a reason in budgets.toml.
    const PASSES = 8;
    let hits = 0;
    for (let pass = 0; pass < PASSES; pass++) {
        for (const pattern of patterns) {
            pattern.lastIndex = 0;
            while (pattern.exec(text) !== null) {
                hits++;
            }
        }
    }
    // Kept alive so the engine cannot fold the work away.
    window.__regexResult = hits;
})();
