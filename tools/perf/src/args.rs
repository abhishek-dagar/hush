//! Command line parsing (spec 0007, Runner surface).
//!
//! `cargo perf`, `cargo perf --accept`, `cargo perf --only <metric>`.

use lexopt::prelude::*;

/// The parsed command line.
pub struct Args {
    /// When true, write `baseline.json` from this run if no row fails.
    pub accept: bool,
    /// When set, measure only this metric (by name).
    pub only: Option<String>,
}

impl Args {
    /// Parses `cargo perf`'s arguments.
    pub fn parse() -> anyhow::Result<Self> {
        let mut accept = false;
        let mut only: Option<String> = None;
        let mut parser = lexopt::Parser::from_env();
        while let Some(arg) = parser.next()? {
            match arg {
                Long("accept") => accept = true,
                Long("only") => {
                    only = Some(parser.value()?.string()?);
                }
                Long("help") | Short('h') => {
                    print_help();
                    std::process::exit(0);
                }
                _ => return Err(arg.unexpected().into()),
            }
        }
        Ok(Self { accept, only })
    }
}

fn print_help() {
    println!("cargo perf - performance budget gate (spec 0007)");
    println!();
    println!("Usage: cargo perf [--accept] [--only <metric>] [--help]");
    println!();
    println!("Options:");
    println!("  --accept   Run the gate and, when no row fails, write baseline.json.");
    println!("  --only     Measure only the named metric (never writes baseline).");
    println!("  --help     Show this help.");
    println!();
    println!("Exit codes: 0 all good, 1 a budget failed, 2 the gate could not run.");
    println!();
    println!("Metrics: bundle_size, cold_start, memory_total, memory_content,");
    println!("         scroll_frame_cpu_p95, script_load");
}
