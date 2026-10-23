//! The app shell for desktop apps built with web tech: a thin entry point over `embed`.

#![forbid(unsafe_code)]

fn main() -> anyhow::Result<()> {
    embed::run(embed::Config {
        title: "Shell".to_owned(),
        app_id: "rust-browser-shell".to_owned(),
        // The app's own start page arrives with feature 9.
        start_url: None,
        chrome_gallery: false,
    })?;
    Ok(())
}
