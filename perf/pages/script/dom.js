// dom.js: the DOM kernel of the script workload page (spec 0007, Pinned
// pages). Builds 5,000 nodes using only the slice 1 DOM API:
// createElement, createTextNode, appendChild, setAttribute, textContent
// and classList. Deterministic: the same tree every run.
(function () {
    'use strict';

    const host = document.getElementById('dom-host');

    function buildTree(root, round) {
        // 50 sections of 100 nodes: 5,000 elements plus a text node each.
        for (let section = 0; section < 50; section++) {
            const list = document.createElement('ul');
            list.setAttribute('data-round', String(round));
            list.setAttribute('data-section', String(section));
            list.className = 'kernel-list section-' + (section % 5);
            for (let item = 0; item < 100; item++) {
                const entry = document.createElement('li');
                entry.className = 'kernel-item row-' + (item % 4);
                entry.setAttribute('data-index', String(item));
                const label = document.createTextNode(
                    'r' + round + ' s' + section + ' i' + item
                );
                entry.appendChild(label);
                list.appendChild(entry);
            }
            root.appendChild(list);
        }
    }

    // Iteration count frozen once (spec 0007): changing it is a budget
    // change and needs a reason in budgets.toml. Each round builds a fresh
    // 5,000 node tree, counts it, then drops it.
    const ROUNDS = 10;
    let counted = 0;
    for (let round = 0; round < ROUNDS; round++) {
        const scratch = document.createElement('div');
        buildTree(scratch, round);
        counted += scratch.childNodes.length;
        const lists = scratch.querySelectorAll('ul');
        counted += lists.length;
    }
    // One tree stays in the page, so the layout has something new to do.
    buildTree(host, ROUNDS);

    // Kept alive so the engine cannot fold the work away.
    window.__domResult = counted;
})();
