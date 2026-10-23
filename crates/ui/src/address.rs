//! The address's state and the search engines it can use (spec 0008,
//! AC-7 to AC-12, AC-33). Plain data in and out: the field's text, what
//! Enter would do, and where a search goes. `embed` feeds it the window's
//! keys and loads what it returns.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use navigation::{Intent, Security, display_host, interpret};
use url::Url;

use crate::ChromeError;

/// The most text the address takes, in UTF 8 bytes (AC-10).
pub const MAX_ADDRESS_BYTES: usize = 8192;

/// How the address is shown (spec 0008, AC-2): a floating palette, the
/// default, or a field in the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AddressStyle {
    /// A pill in the sidebar opens a palette over the page (AC-6).
    #[default]
    Palette,
    /// The field sits in the sidebar (AC-5).
    Inline,
}

impl AddressStyle {
    /// The `navigation.address_style` value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Palette => "palette",
            Self::Inline => "inline",
        }
    }
}

impl FromStr for AddressStyle {
    type Err = ChromeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "palette" => Ok(Self::Palette),
            "inline" => Ok(Self::Inline),
            _ => Err(ChromeError::UnknownValue {
                name: "navigation.address_style",
                value: s.to_owned(),
            }),
        }
    }
}

/// Which engine the settings window's listbox picks (AC-32): a preset, or
/// Custom, whose template comes from its own field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    /// DuckDuckGo.
    DuckDuckGo,
    /// Brave Search.
    Brave,
    /// Startpage.
    Startpage,
    /// Google.
    Google,
    /// Bing.
    Bing,
    /// Ecosia.
    Ecosia,
    /// A custom template.
    Custom,
}

impl EngineKind {
    /// Every choice, in the listbox's order.
    pub const ALL: [Self; 7] = [
        Self::DuckDuckGo,
        Self::Brave,
        Self::Startpage,
        Self::Google,
        Self::Bing,
        Self::Ecosia,
        Self::Custom,
    ];

    /// The `search.engine` value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DuckDuckGo => "duckduckgo",
            Self::Brave => "brave",
            Self::Startpage => "startpage",
            Self::Google => "google",
            Self::Bing => "bing",
            Self::Ecosia => "ecosia",
            Self::Custom => "custom",
        }
    }

    /// The preset engine, or `None` for Custom, which needs a template.
    pub fn preset(self) -> Option<SearchEngine> {
        Some(match self {
            Self::DuckDuckGo => SearchEngine::DuckDuckGo,
            Self::Brave => SearchEngine::Brave,
            Self::Startpage => SearchEngine::Startpage,
            Self::Google => SearchEngine::Google,
            Self::Bing => SearchEngine::Bing,
            Self::Ecosia => SearchEngine::Ecosia,
            Self::Custom => return None,
        })
    }
}

impl FromStr for EngineKind {
    type Err = ChromeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == s)
            .ok_or_else(|| ChromeError::UnknownValue {
                name: "search.engine",
                value: s.to_owned(),
            })
    }
}

/// A search engine preset, or a custom template (AC-9).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SearchEngine {
    /// DuckDuckGo, the default.
    #[default]
    DuckDuckGo,
    /// Brave Search.
    Brave,
    /// Startpage.
    Startpage,
    /// Google.
    Google,
    /// Bing.
    Bing,
    /// Ecosia.
    Ecosia,
    /// A template from `search.custom_url`, validated: an `http` or
    /// `https` URL with `%s` where the query goes.
    Custom(String),
}

impl SearchEngine {
    /// The presets, in the settings window's order.
    pub const PRESETS: [Self; 6] = [
        Self::DuckDuckGo,
        Self::Brave,
        Self::Startpage,
        Self::Google,
        Self::Bing,
        Self::Ecosia,
    ];

    /// The `search.engine` value.
    pub const fn as_str(&self) -> &'static str {
        self.kind().as_str()
    }

    /// Which listbox choice it is.
    pub const fn kind(&self) -> EngineKind {
        match self {
            Self::DuckDuckGo => EngineKind::DuckDuckGo,
            Self::Brave => EngineKind::Brave,
            Self::Startpage => EngineKind::Startpage,
            Self::Google => EngineKind::Google,
            Self::Bing => EngineKind::Bing,
            Self::Ecosia => EngineKind::Ecosia,
            Self::Custom(_) => EngineKind::Custom,
        }
    }

    /// The name the intent row shows: the preset's, or a custom template's
    /// host (spec 0008, Value sourcing).
    pub fn label(&self) -> String {
        match self {
            Self::DuckDuckGo => "DuckDuckGo".to_owned(),
            Self::Brave => "Brave Search".to_owned(),
            Self::Startpage => "Startpage".to_owned(),
            Self::Google => "Google".to_owned(),
            Self::Bing => "Bing".to_owned(),
            Self::Ecosia => "Ecosia".to_owned(),
            Self::Custom(template) => Url::parse(template)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .unwrap_or_else(|| "Custom".to_owned()),
        }
    }

    /// The template with `%s` where the query goes.
    pub fn template(&self) -> &str {
        match self {
            Self::DuckDuckGo => "https://duckduckgo.com/?q=%s",
            Self::Brave => "https://search.brave.com/search?q=%s",
            Self::Startpage => "https://www.startpage.com/sp/search?query=%s",
            Self::Google => "https://www.google.com/search?q=%s",
            Self::Bing => "https://www.bing.com/search?q=%s",
            Self::Ecosia => "https://www.ecosia.org/search?q=%s",
            Self::Custom(template) => template,
        }
    }

    /// Accepts a custom template (AC-32): it must parse as an `http` or
    /// `https` URL and contain `%s`.
    pub fn custom(template: &str) -> Result<Self, ChromeError> {
        let valid = Url::parse(template)
            .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && template.contains("%s"));
        if valid {
            Ok(Self::Custom(template.to_owned()))
        } else {
            Err(ChromeError::UnknownValue {
                name: "search.custom_url",
                value: template.to_owned(),
            })
        }
    }

    /// The search URL for `query` (AC-9): `%s` replaced by the query in
    /// `application/x-www-form-urlencoded` form, so spaces become `+`.
    #[allow(
        clippy::expect_used,
        reason = "the fallback is the constant about:blank, which always parses"
    )]
    pub fn url(&self, query: &str) -> Url {
        let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
        let text = self.template().replace("%s", &encoded);
        Url::parse(&text).unwrap_or_else(|_| {
            // Every template accepted by [`SearchEngine::custom`] parses,
            // and form encoded text cannot break it; this keeps a hand
            // built `Custom` from panicking.
            Url::parse("about:blank").expect("about:blank is always a URL")
        })
    }
}

impl FromStr for SearchEngine {
    type Err = ChromeError;

    /// The presets only: `custom` needs a template, so `ui::Settings` reads
    /// those two keys together (AC-33).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::PRESETS
            .into_iter()
            .find(|engine| engine.as_str() == s)
            .ok_or_else(|| ChromeError::UnknownValue {
                name: "search.engine",
                value: s.to_owned(),
            })
    }
}

impl fmt::Display for SearchEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The address field's state (spec 0008, Data model, `AddressBar`): the
/// text, whether the user is editing it, what Enter would do, and the URL
/// it shows. The text mirrors the DOM input, which owns what is painted.
pub struct AddressBar {
    engine: SearchEngine,
    /// How the field is shown (AC-2).
    style: AddressStyle,
    /// The palette is open (AC-6); always false in inline style.
    open: bool,
    /// The field's text.
    text: String,
    /// The URL shown when the user is not editing: the committed URL, or
    /// the user's own pending one (AC-11).
    shown: Option<Url>,
    /// The field has focus: commits leave the text alone (AC-12).
    editing: bool,
    /// What Enter does for `text`, for the intent row (AC-7).
    intent: Option<Intent>,
    /// The target the last Enter asked for, kept for `address.go`.
    target: Option<Url>,
    /// Whether an absolute path names a file (AC-8): `embed` installs the
    /// real check, so this crate never reads the disk.
    file_exists: Box<dyn Fn(&Path) -> bool>,
}

impl AddressBar {
    /// An address with `engine`, showing nothing yet.
    pub fn new(engine: SearchEngine) -> Self {
        Self {
            engine,
            style: AddressStyle::default(),
            open: false,
            text: String::new(),
            shown: None,
            editing: false,
            intent: None,
            target: None,
            file_exists: Box::new(|_| false),
        }
    }

    /// The engine searches go through.
    pub fn engine(&self) -> &SearchEngine {
        &self.engine
    }

    /// How the address is shown (AC-2).
    pub fn style(&self) -> AddressStyle {
        self.style
    }

    /// Changes the style (AC-2). Leaving palette style closes its palette.
    pub fn set_style(&mut self, style: AddressStyle) {
        self.style = style;
        if style == AddressStyle::Inline {
            self.open = false;
        }
    }

    /// Whether the palette is open (AC-6).
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the palette (AC-6): the field starts editing, so the full URL
    /// shows selected. Inline style has no palette to open.
    pub fn open(&mut self) {
        if self.style == AddressStyle::Palette {
            self.open = true;
            self.begin_edit();
        }
    }

    /// Closes the palette and, with `revert`, puts the shown URL back
    /// (AC-6, AC-12).
    pub fn close(&mut self, revert: bool) {
        self.open = false;
        self.end_edit(revert);
    }

    /// The host the pill shows (AC-1, AC-6): the shown URL's display host.
    pub fn pill_host(&self) -> String {
        shown_text(self.shown.as_ref())
    }

    /// Uses `engine` for the next search; the intent row follows if the
    /// text stays the same.
    pub fn set_engine(&mut self, engine: SearchEngine) {
        self.engine = engine;
        self.refresh();
    }

    /// Installs the file check AC-8 needs for absolute paths.
    pub fn set_file_check(&mut self, file_exists: Box<dyn Fn(&Path) -> bool>) {
        self.file_exists = file_exists;
        self.refresh();
    }

    /// The field's text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the user is editing, so commits leave the text alone.
    pub fn editing(&self) -> bool {
        self.editing
    }

    /// What Enter would do for the current text, for tests and the row.
    pub fn intent(&self) -> Option<&Intent> {
        self.intent.as_ref()
    }

    /// The intent row's line (AC-7), or `None` while the text is empty.
    pub fn intent_text(&self) -> Option<String> {
        match self.intent.as_ref()? {
            Intent::Go(url) => {
                let shown = display_host(url);
                Some(format!("Go to {}{}", shown.host, shown.rest))
            }
            Intent::Search(text) => Some(format!("Search {} for \"{text}\"", self.engine.label())),
        }
    }

    /// The security icon for what is shown (AC-3): the shown URL while the
    /// field is idle, else what Enter would load. The globe stands for a
    /// search and for no text at all.
    pub fn security(&self) -> Security {
        let url = if self.editing {
            match &self.intent {
                Some(Intent::Go(url)) => Some(url),
                _ => None,
            }
        } else {
            self.shown.as_ref()
        };
        url.map_or(Security::Globe, |url| display_host(url).icon)
    }

    /// The URL the field shows while the user is not editing.
    pub fn shown(&self) -> Option<&Url> {
        self.shown.as_ref()
    }

    /// Shows `url` as the committed URL, unless the user is editing (AC-3,
    /// AC-12).
    pub fn show(&mut self, url: Option<Url>) {
        self.show_shown(url, false);
    }

    /// Shows the URL the user just asked for while it loads (AC-11): the
    /// full URL, not the display host, so the field keeps what was typed
    /// until the document commits.
    pub fn show_pending(&mut self, url: Option<Url>) {
        self.show_shown(url, true);
    }

    fn show_shown(&mut self, url: Option<Url>, full: bool) {
        self.shown = url;
        if !self.editing {
            self.text = match (&self.shown, full) {
                (Some(url), true) => url.as_str().to_owned(),
                _ => shown_text(self.shown.as_ref()),
            };
            self.refresh();
        }
    }

    /// Focus arrived in the field: the full URL shows, ready to be read as
    /// one edit (AC-5).
    pub fn begin_edit(&mut self) {
        self.editing = true;
        self.target = None;
        if let Some(url) = &self.shown
            && !matches!(url.as_str(), "about:start" | "about:blank")
        {
            self.text = url.as_str().to_owned();
        }
        self.refresh();
    }

    /// The text changed under the user's hand: read it again (AC-7).
    pub fn edit(&mut self, text: &str) {
        self.text = text.to_owned();
        self.editing = true;
        self.refresh();
    }

    /// Focus left the field. With `revert`, the shown URL replaces
    /// whatever was typed (AC-12).
    pub fn end_edit(&mut self, revert: bool) {
        self.editing = false;
        if revert {
            self.text = shown_text(self.shown.as_ref());
        }
        self.refresh();
    }

    /// Enter was pressed: the URL to load, or `None` for empty text
    /// (AC-8). The field then shows the URL asked for while it loads
    /// (AC-11).
    pub fn enter(&mut self) -> Option<Url> {
        let target = match self.intent.clone()? {
            Intent::Go(url) => url,
            Intent::Search(query) => self.engine.url(&query),
        };
        self.target = Some(target.clone());
        self.text = target.as_str().to_owned();
        self.editing = false;
        self.shown = Some(target.clone());
        self.refresh();
        Some(target)
    }

    /// The target of the last [`AddressBar::enter`], used once by the
    /// `address.go` command the same key event asks for.
    pub fn take_target(&mut self) -> Option<Url> {
        self.target.take()
    }

    /// Reads `text` again for the intent row.
    fn refresh(&mut self) {
        self.intent = interpret(&self.text, &self.file_exists);
    }
}

/// What the field shows for `url` while the user is not editing: the
/// display host (AC-3), which is a file's name or an `about:` URL in full;
/// empty for no URL and the start and blank pages, which show their
/// placeholder instead.
fn shown_text(url: Option<&Url>) -> String {
    match url {
        Some(url) if !matches!(url.as_str(), "about:start" | "about:blank") => {
            display_host(url).host
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(name: &str) -> SearchEngine {
        name.parse().unwrap()
    }

    #[test]
    fn presets_are_the_ac_list_in_order() {
        // AC-9
        let names: Vec<_> = SearchEngine::PRESETS.iter().map(|e| e.as_str()).collect();
        assert_eq!(
            names,
            [
                "duckduckgo",
                "brave",
                "startpage",
                "google",
                "bing",
                "ecosia"
            ]
        );
        assert_eq!(
            SearchEngine::default().template(),
            "https://duckduckgo.com/?q=%s"
        );
        assert_eq!(
            engine("startpage").template(),
            "https://www.startpage.com/sp/search?query=%s"
        );
    }

    #[test]
    fn a_search_encodes_the_query_as_a_form() {
        // AC-9: spaces become `+`, everything else percent encoded.
        let url = engine("duckduckgo").url("rust borrow checker");
        assert_eq!(
            url.as_str(),
            "https://duckduckgo.com/?q=rust+borrow+checker"
        );
        let url = engine("google").url("a&b=c/é");
        assert_eq!(
            url.as_str(),
            "https://www.google.com/search?q=a%26b%3Dc%2F%C3%A9"
        );
    }

    #[test]
    fn a_custom_template_is_http_or_https_with_a_placeholder() {
        // AC-32, AC-33
        let custom = SearchEngine::custom("https://kagi.com/search?q=%s").unwrap();
        assert_eq!(custom.as_str(), "custom");
        assert_eq!(custom.label(), "kagi.com");
        assert_eq!(custom.url("a b").as_str(), "https://kagi.com/search?q=a+b");
        assert!(SearchEngine::custom("ftp://kagi.com/?q=%s").is_err());
        assert!(SearchEngine::custom("https://kagi.com/search").is_err());
        assert!(SearchEngine::custom("not a url %s").is_err());
    }

    #[test]
    fn the_intent_row_reads_go_and_search() {
        // AC-7, AC-8
        let mut address = AddressBar::new(SearchEngine::default());
        address.edit("example.com");
        assert_eq!(address.intent_text().as_deref(), Some("Go to example.com"));
        assert_eq!(address.security(), Security::Lock);
        address.edit("rust borrow checker");
        assert_eq!(
            address.intent_text().as_deref(),
            Some("Search DuckDuckGo for \"rust borrow checker\"")
        );
        assert_eq!(address.security(), Security::Globe);
        address.edit("");
        assert_eq!(address.intent_text(), None);
    }

    #[test]
    fn enter_turns_the_intent_into_a_url_and_shows_it() {
        // AC-8, AC-9, AC-11
        let mut address = AddressBar::new(SearchEngine::default());
        address.show(Some(Url::parse("https://a.test/").unwrap()));
        address.begin_edit();
        address.edit("example.com/a");
        let target = address.enter().unwrap();
        assert_eq!(target.as_str(), "https://example.com/a");
        assert_eq!(address.text(), "https://example.com/a");
        assert_eq!(address.take_target(), Some(target));
        assert_eq!(address.take_target(), None, "used once");
        assert!(!address.editing());

        address.begin_edit();
        address.edit("?rust");
        assert_eq!(
            address.enter().unwrap().as_str(),
            "https://duckduckgo.com/?q=rust"
        );
        address.begin_edit();
        address.edit("  ");
        assert_eq!(address.enter(), None, "empty does nothing");
    }

    #[test]
    fn a_pending_user_load_shows_the_full_url_until_it_commits() {
        // AC-11: the URL asked for while it loads, the display host after.
        let mut address = AddressBar::new(SearchEngine::default());
        address.show(Some(Url::parse("https://a.test/").unwrap()));
        address.begin_edit();
        address.edit("https://example.com/docs");
        address.enter();
        address.show_pending(Some(Url::parse("https://example.com/docs").unwrap()));
        assert_eq!(address.text(), "https://example.com/docs");
        address.show(Some(Url::parse("https://example.com/docs").unwrap()));
        assert_eq!(address.text(), "example.com", "the committed display host");
    }

    #[test]
    fn a_commit_does_not_change_the_text_while_editing() {
        // AC-12
        let mut address = AddressBar::new(SearchEngine::default());
        address.show(Some(Url::parse("https://a.test/").unwrap()));
        assert_eq!(address.text(), "a.test", "the display host while idle");
        address.begin_edit();
        assert_eq!(address.text(), "https://a.test/", "the full URL on focus");
        address.edit("typing");
        address.show(Some(Url::parse("https://b.test/").unwrap()));
        assert_eq!(address.text(), "typing", "the commit is ignored");
        address.end_edit(false);
        assert_eq!(address.text(), "typing");
        address.end_edit(true);
        assert_eq!(address.text(), "b.test", "the new URL shows");
        assert_eq!(address.security(), Security::Lock);
        address.show(Some(Url::parse("http://b.test/").unwrap()));
        assert_eq!(address.security(), Security::Warning, "http warns");
    }

    #[test]
    fn a_shown_start_page_shows_nothing() {
        // AC-1, AC-11
        let mut address = AddressBar::new(SearchEngine::default());
        address.show(Some(Url::parse("about:start").unwrap()));
        assert_eq!(address.text(), "");
        assert_eq!(address.intent_text(), None);
    }
}
