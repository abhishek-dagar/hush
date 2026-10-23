//! The desktop browser: a thin entry point over `embed`.

#![forbid(unsafe_code)]

/// Opens the chrome's control gallery instead of the frame (spec 0003,
/// configuration). Dev builds only.
const GALLERY_FLAG: &str = "--chrome-gallery";

fn main() -> anyhow::Result<()> {
    // The content process's own flags never reach here: `run` checks them
    // first.
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let chrome_gallery = cfg!(debug_assertions) && args.iter().any(|a| a == GALLERY_FLAG);
    args.retain(|a| a != GALLERY_FLAG);
    embed::run(embed::Config {
        title: "Browser".to_owned(),
        app_id: "rust-browser".to_owned(),
        // The first other argument is the page to open (spec 0004, AC-1).
        start_url: args.into_iter().next(),
        chrome_gallery,
    })?;
    Ok(())
}
