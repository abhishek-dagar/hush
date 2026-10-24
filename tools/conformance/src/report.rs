//! Reporting: summary table, `results.json`, failure PNGs (spec 0005, AC-12).

use std::path::{Path, PathBuf};

use anyhow::Result;

/// The target directory: `CARGO_TARGET_DIR` or `target`.
pub fn target_dir() -> PathBuf {
    std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("target"))
}

/// The conformance output directory.
pub fn conformance_dir() -> PathBuf {
    target_dir().join("conformance")
}

/// Clears and recreates the conformance output directory at the start of a run.
pub fn clear_output() -> Result<()> {
    let dir = conformance_dir();
    std::fs::create_dir_all(&dir)?;
    let images = dir.join("images");
    if images.exists() {
        std::fs::remove_dir_all(&images)?;
    }
    std::fs::create_dir_all(&images)?;
    let results = dir.join("results.json");
    if results.exists() {
        std::fs::remove_file(&results)?;
    }
    Ok(())
}

/// Writes `results.json`.
pub fn write_results(path: &Path, value: &serde_json::Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(value)?;
    std::fs::write(path, text)?;
    Ok(())
}

/// Saves an RGBA8 buffer as a PNG.
pub fn save_png(path: &Path, pixels: &[u8], width: u32, height: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(path)?;
    let w = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(pixels)?;
    writer.finish()?;
    Ok(())
}

/// Prints the summary table.
pub fn print_summary(rows: &[AreaSummary]) {
    println!(
        "{:<20} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>7}",
        "area", "total", "pass", "fail", "crash", "timeout", "error", "skip", "pass%"
    );
    for r in rows {
        println!(
            "{:<20} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6.1}%",
            r.name, r.total, r.pass, r.fail, r.crash, r.timeout, r.error, r.skip, r.pass_rate
        );
    }
}

#[derive(Debug, Clone)]
pub struct AreaSummary {
    pub name: String,
    pub total: usize,
    pub pass: usize,
    pub fail: usize,
    pub crash: usize,
    pub timeout: usize,
    pub error: usize,
    pub skip: usize,
    pub pass_rate: f64,
}
