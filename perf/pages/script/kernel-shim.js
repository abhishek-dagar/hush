// kernel-shim.js: the only part of the Octane harness the workload page
// needs (spec 0007, Pinned pages). The unmodified Octane kernel files
// register their suites through these two constructors; the driver at the
// end runs the registered benchmark functions. Everything else in
// Octane's base.js (scoring, warmups, the results table) is not part of
// the measurement.
(function () {
    'use strict';

    // One benchmark: its name and the function that runs one pass.
    // Matches the five argument shape the kernels use:
    // (name, doWarmup, doDeterministic, deterministicIterations, run).
    function Benchmark(name, doWarmup, doDeterministic, deterministicIterations, run) {
        this.name = name;
        this.doWarmup = doWarmup;
        this.doDeterministic = doDeterministic;
        this.deterministicIterations = deterministicIterations;
        this.run = run;
    }

    // One suite: the kernel files create one and forget it, so the shim
    // keeps a list for the driver.
    function BenchmarkSuite(name, reference, benchmarks) {
        this.name = name;
        this.reference = reference;
        this.benchmarks = benchmarks;
        BenchmarkSuite.list.push(this);
    }
    BenchmarkSuite.list = [];

    window.Benchmark = Benchmark;
    window.BenchmarkSuite = BenchmarkSuite;
})();
