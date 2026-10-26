//! The chrome document (spec 0003, milestones 3 and 4): `frame.html` loads
//! from the embedded assets with scripts off, lays out the Zen frame, turns
//! activations into commands, and runs the keyboard model and the Linux
//! app menu (AC-6, AC-7, AC-8, AC-9, AC-13). `settings.html` shows the
//! settings it is given and turns its controls into the same commands
//! (AC-6, AC-7, AC-12).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use ipc::{FocusDirection, InputEvent, Key, Modifiers, PointerButton};
use page::FontContext;
use ui::{
    Accent, AccentSetting, ButtonLayout, CaptionButton, ChromeCommand, ChromeDocument,
    ChromeOutcome, ChromeSurface, Hit, Os, OsAppearance, PointF, RectF, Settings, SizeF, Theme,
    UiZoom, WindowAction, ZoomStep,
};

/// Cantarell stands in for the system font, so text measures the same on
/// every machine.
fn fonts() -> FontContext {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tools/conformance/fonts/Cantarell-Regular.ttf"
    );
    FontContext::with_fonts([std::fs::read(path).unwrap()])
}

const WINDOW: SizeF = SizeF {
    width: 1280.0,
    height: 800.0,
};

fn load(os: Os) -> ChromeDocument {
    let mut doc =
        ChromeDocument::load(ChromeSurface::Frame, os, WINDOW, 1.0, Some(fonts())).unwrap();
    let appearance = OsAppearance::fallback(os);
    doc.set_theme(&ui::resolve(&Settings::default(), &appearance));
    doc.set_caption(&appearance.button_layout);
    assert!(doc.frame().is_some(), "the first frame draws");
    doc
}

fn center(rect: RectF) -> PointF {
    PointF {
        x: rect.origin.x + rect.size.width / 2.0,
        y: rect.origin.y + rect.size.height / 2.0,
    }
}

fn click(doc: &mut ChromeDocument, at: PointF) -> ChromeOutcome {
    let (x, y) = (at.x, at.y);
    doc.input(InputEvent::PointerMove { x, y });
    let _ = doc.frame();
    let button = PointerButton::Primary;
    doc.input(InputEvent::PointerDown { x, y, button });
    doc.input(InputEvent::PointerUp { x, y, button })
}

fn key(doc: &mut ChromeDocument, key: Key) -> ChromeOutcome {
    let modifiers = Modifiers::default();
    let down = doc.input(InputEvent::Key {
        key: key.clone(),
        modifiers,
        pressed: true,
        repeat: false,
    });
    let up = doc.input(InputEvent::Key {
        key,
        modifiers,
        pressed: false,
        repeat: false,
    });
    let _ = doc.frame();
    ChromeOutcome {
        command: down.command.or(up.command),
        focus_left: down.focus_left.or(up.focus_left),
        cursor: None,
        copied: down.copied.or(up.copied),
    }
}

fn command(outcome: ChromeOutcome) -> Option<ChromeCommand> {
    outcome.command.map(|c| c.unwrap())
}

#[test]
fn the_page_card_sits_beside_the_sidebar_inset_8_px() {
    // AC-8
    let doc = load(Os::Mac);
    let page = doc.layout().page.expect("a page slot");
    assert_eq!(page.origin.x, 240.0, "after the sidebar");
    assert_eq!(
        page.origin.y, 8.0,
        "the same 8 px inset on top as on the side and bottom: on macOS the \
         traffic lights share the sidebar's top row, as in Zen"
    );
    assert_eq!(page.origin.x + page.size.width, 1280.0 - 8.0);
    assert_eq!(page.origin.y + page.size.height, 800.0 - 8.0);
    assert_eq!(doc.layout().page_radius, 8.0);
    assert!(
        doc.layout().caption_buttons.is_empty(),
        "macOS keeps the traffic lights"
    );
    assert_eq!(
        doc.layout().controls.len(),
        4,
        "on macOS only the sidebar's back, forward, reload and address pill \
         (spec 0008, AC-1): no menu button, no closed menu, no hidden overlay"
    );
    assert_eq!(
        doc.layout().hit(PointF { x: 100.0, y: 300.0 }, false),
        Hit::Drag
    );
}

#[test]
fn macos_puts_back_forward_and_reload_in_the_traffic_light_row() {
    // Spec 0008, AC-1, in the Zen frame of spec 0003
    let doc = load(Os::Mac);
    let row = doc
        .layout()
        .title_row
        .expect("the row the traffic lights center on");
    assert_eq!(
        (row.origin.y, row.size.height),
        (0.0, 44.0),
        "a 44 px top row"
    );
    for id in ["nav-back", "nav-forward", "nav-reload"] {
        let button = doc.element_box(id).unwrap();
        let center = button.origin.y + button.size.height / 2.0;
        assert_eq!(center, 22.0, "{id} centered on the row");
        assert!(button.origin.x >= 78.0, "{id} clear of the traffic lights");
        assert!(
            button.origin.x + button.size.width <= 240.0,
            "{id} in the sidebar"
        );
    }
}

#[test]
fn windows_draws_three_46_px_buttons_flush_top_right() {
    // AC-8
    let doc = load(Os::Windows);
    let buttons = &doc.layout().caption_buttons;
    let order: Vec<_> = buttons.iter().map(|(b, _)| *b).collect();
    assert_eq!(
        order,
        [
            CaptionButton::Minimize,
            CaptionButton::Maximize,
            CaptionButton::Close
        ]
    );
    let (_, close) = buttons[2];
    assert_eq!(close, RectF::new(1280.0 - 46.0, 0.0, 46.0, 32.0));
    let maximize = center(buttons[1].1);
    assert_eq!(
        doc.layout().hit(maximize, true),
        Hit::Caption(CaptionButton::Maximize),
        "the Snap Layouts button"
    );
}

#[test]
fn linux_buttons_follow_the_desktop_layout() {
    // AC-8
    let mut doc = load(Os::Linux);
    doc.set_caption(&ButtonLayout::parse_gnome("close,minimize:"));
    let _ = doc.frame();
    let buttons = &doc.layout().caption_buttons;
    let order: Vec<_> = buttons.iter().map(|(b, _)| *b).collect();
    assert_eq!(order, [CaptionButton::Close, CaptionButton::Minimize]);
    assert!(buttons[0].1.origin.x < 50.0, "on the left");
    assert!(buttons[0].1.origin.x < buttons[1].1.origin.x);
    assert_eq!(buttons[0].1.size.width, 28.0, "round 28 px buttons");
    assert_eq!(
        doc.layout().hit(PointF { x: 2.0, y: 400.0 }, false),
        Hit::Resize(ui::ResizeEdge::West)
    );
}

#[test]
fn a_caption_button_click_is_a_window_command() {
    // AC-13
    let mut doc = load(Os::Linux);
    let close = doc
        .layout()
        .caption_buttons
        .iter()
        .find(|(b, _)| *b == CaptionButton::Close)
        .unwrap()
        .1;
    assert_eq!(
        command(click(&mut doc, center(close))),
        Some(ChromeCommand::Window(WindowAction::Close))
    );
}

#[test]
fn tab_reaches_the_menu_button_and_leaves_for_the_page() {
    // AC-7
    let mut doc = load(Os::Linux);
    assert!(doc.focus_enter(FocusDirection::Forward), "the menu button");
    let enter = key(&mut doc, Key::Enter);
    assert_eq!(command(enter), Some(ChromeCommand::AppMenu));
    // Back, forward, reload and the address (spec 0008, AC-1), then out:
    // caption buttons are not Tab stops.
    for stop in ["back", "forward", "reload", "address"] {
        let out = key(&mut doc, Key::Tab);
        assert_eq!(out.focus_left, None, "{stop}");
    }
    let out = key(&mut doc, Key::Tab);
    assert_eq!(out.focus_left, Some(FocusDirection::Forward));
    assert!(!doc.has_focus());
}

#[test]
fn macos_tab_starts_at_the_back_button() {
    // AC-7; spec 0008, AC-1
    let mut doc = load(Os::Mac);
    assert!(doc.focus_enter(FocusDirection::Forward));
    let enter = key(&mut doc, Key::Enter);
    assert_eq!(
        command(enter),
        None,
        "back is aria-disabled with no history"
    );
    key(&mut doc, Key::Tab);
    key(&mut doc, Key::Tab);
    let enter = key(&mut doc, Key::Enter);
    assert_eq!(command(enter), Some(ChromeCommand::Reload));
}

#[test]
fn the_navigation_row_follows_the_history_and_the_address_shows_the_host() {
    // Spec 0008, AC-1, AC-3
    let mut doc = load(Os::Mac);
    assert_eq!(
        doc.element_text("address-placeholder").as_deref(),
        Some("Search or enter address")
    );
    doc.set_navigation(&ui::NavView {
        can_back: true,
        can_forward: false,
        url: Some(url::Url::parse("https://www.example.com/docs").unwrap()),
        pending: None,
        loading: false,
    });
    assert_eq!(
        doc.element_text("address-host").as_deref(),
        Some("www.example.com")
    );
    assert_eq!(
        doc.element_text("address-icon").as_deref(),
        Some("\u{E007}"),
        "the lock"
    );
    let _ = doc.frame();
    let back = doc.element_box("nav-back").unwrap();
    assert_eq!(
        command(click(&mut doc, center(back))),
        Some(ChromeCommand::Back)
    );
    let forward = doc.element_box("nav-forward").unwrap();
    assert_eq!(
        command(click(&mut doc, center(forward))),
        None,
        "aria-disabled"
    );
    doc.set_navigation(&ui::NavView {
        url: Some(url::Url::parse("about:start").unwrap()),
        ..ui::NavView::default()
    });
    assert_eq!(
        doc.element_text("address-icon").as_deref(),
        Some("\u{E017}"),
        "the globe"
    );
    let _ = doc.frame();
    assert!(
        doc.element_box("address-host").is_none(),
        "hidden for about:start"
    );
    assert!(doc.element_box("address-placeholder").is_some());
}

#[test]
fn the_linux_menu_opens_works_by_keyboard_and_escape_returns_focus() {
    // AC-9
    let mut doc = load(Os::Linux);
    assert!(doc.focus_enter(FocusDirection::Forward));
    let menu = ui::app_menu(&Settings::default(), ui::Accent::Blue, Os::Linux, "Browser");
    assert!(doc.open_menu(&menu));
    let _ = doc.frame();
    assert!(doc.menu_open());
    assert!(doc.has_focus(), "the first row");

    // The first row is System under Appearance; Down moves to Light.
    let _ = key(&mut doc, Key::ArrowDown);
    let chosen = key(&mut doc, Key::Enter);
    assert_eq!(command(chosen), Some(ChromeCommand::SetTheme(Theme::Light)));

    // Type ahead: S jumps to the next row starting with S, which after
    // Light is the accent's System row.
    let _ = key(&mut doc, Key::Character("s".into()));
    let accent = key(&mut doc, Key::Enter);
    assert_eq!(
        command(accent),
        Some(ChromeCommand::SetAccent(ui::AccentSetting::System))
    );

    // End is Close Window; one up is Settings.
    let _ = key(&mut doc, Key::End);
    let _ = key(&mut doc, Key::ArrowUp);
    let settings = key(&mut doc, Key::Enter);
    assert_eq!(command(settings), Some(ChromeCommand::OpenSettings));

    // Escape closes and puts focus back on the menu button.
    let _ = key(&mut doc, Key::Escape);
    assert!(!doc.menu_open());
    let again = key(&mut doc, Key::Enter);
    assert_eq!(
        command(again),
        Some(ChromeCommand::AppMenu),
        "focus is back on the button"
    );
}

#[test]
fn a_click_outside_the_menu_closes_it() {
    // AC-9
    let mut doc = load(Os::Linux);
    let menu = ui::app_menu(&Settings::default(), ui::Accent::Blue, Os::Linux, "Browser");
    assert!(doc.open_menu(&menu));
    let _ = doc.frame();
    assert_eq!(
        doc.layout().hit(PointF { x: 20.0, y: 60.0 }, false),
        Hit::Chrome,
        "the open menu is the chrome's"
    );
    let outcome = click(&mut doc, PointF { x: 900.0, y: 500.0 });
    assert!(outcome.command.is_none());
    assert!(!doc.menu_open());
}

#[test]
fn at_200_percent_the_frame_still_fits_and_the_card_shrinks() {
    // AC-6: 1280 by 800 window pixels at zoom 2 are 640 by 400 chrome px.
    let mut doc = load(Os::Windows);
    doc.resize(
        SizeF {
            width: 640.0,
            height: 400.0,
        },
        2.0,
    );
    let _ = doc.frame();
    let layout = doc.layout();
    let page = layout.page.unwrap();
    assert!(page.size.width > 300.0, "{page:?}");
    let close = layout.caption_buttons[2].1;
    assert_eq!(
        close.origin.x + close.size.width,
        640.0,
        "still flush right"
    );
    for (i, (_, a)) in layout.caption_buttons.iter().enumerate() {
        for (_, b) in &layout.caption_buttons[i + 1..] {
            assert!(
                a.origin.x + a.size.width <= b.origin.x,
                "{a:?} overlaps {b:?}"
            );
        }
    }
}

#[test]
fn the_menu_button_draws_its_icon_from_the_embedded_font() {
    // AC-1, spec 0006 AC-11: the icon font is served on the first frame,
    // with no block period.
    let mut doc =
        ChromeDocument::load(ChromeSurface::Frame, Os::Linux, WINDOW, 1.0, Some(fonts())).unwrap();
    let appearance = OsAppearance::fallback(Os::Linux);
    doc.set_theme(&ui::resolve(&Settings::default(), &appearance));
    let frame = doc.frame().expect("the first frame draws");
    let icons_len = ui::asset("chrome://ui/icons.ttf").unwrap().bytes.len();
    let icons = frame
        .adds
        .iter()
        .find_map(|add| match add {
            ipc::ToBrowser::AddFont {
                key,
                source: ipc::FontSource::Bytes(bytes),
                ..
            } if bytes.len() == icons_len => Some(*key),
            _ => None,
        })
        .expect("icons.ttf is registered with the first frame");
    let drawn = frame.list.items.iter().any(|item| {
        matches!(&item.kind, display_list::ItemKind::Text { font, glyphs, .. }
            if *font == icons && !glyphs.is_empty())
    });
    assert!(drawn, "the menu glyph draws in icons.ttf");
}

// The settings window (spec 0003, milestone 4).

/// The settings window's first layout: 480 chrome px wide (the window is
/// sized from it), with `settings` shown and a blue OS accent.
fn load_settings(os: Os, settings: &Settings, appearance: &OsAppearance) -> ChromeDocument {
    let theme = ui::resolve(settings, appearance);
    let viewport = SizeF {
        width: 480.0,
        height: 560.0,
    };
    let mut doc = ChromeDocument::load(
        ChromeSurface::Settings,
        os,
        viewport,
        theme.zoom,
        Some(fonts()),
    )
    .unwrap();
    doc.set_theme(&theme);
    doc.set_caption(&appearance.button_layout);
    doc.set_settings(settings, Accent::Blue);
    assert!(doc.frame().is_some(), "the first frame draws");
    doc
}

fn settings_window(os: Os) -> ChromeDocument {
    load_settings(os, &Settings::default(), &OsAppearance::fallback(os))
}

fn element(doc: &ChromeDocument, id: &str) -> RectF {
    doc.element_box(id)
        .unwrap_or_else(|| panic!("no #{id} in the settings window"))
}

/// Every control of the settings window, in `Tab` order.
const SETTINGS_CONTROLS: [&str; 15] = [
    "theme-system",
    "theme-light",
    "theme-dark",
    "accent-system",
    "accent-blue",
    "accent-purple",
    "accent-pink",
    "accent-red",
    "accent-orange",
    "accent-yellow",
    "accent-green",
    "accent-graphite",
    "zoom-out",
    "zoom-in",
    "zoom-reset",
];

#[test]
fn the_settings_window_is_sheet_wide_and_as_tall_as_its_content() {
    // AC-12
    let doc = settings_window(Os::Linux);
    let size = doc.natural_size().expect("the settings window has a size");
    assert_eq!(size.width, 480.0, "--sheet-width");
    let reset = element(&doc, "zoom-reset");
    assert!(
        reset.origin.y + reset.size.height < size.height,
        "{reset:?} inside {size:?}"
    );
    assert!(
        size.height < 760.0,
        "shorter than the first guess: {size:?}"
    );
    let layout = doc.layout();
    assert!(layout.page.is_none(), "no page card");
    assert_eq!(layout.resize_border, 0.0, "it keeps one size");
    let buttons: Vec<_> = layout.caption_buttons.iter().map(|(b, _)| *b).collect();
    assert_eq!(buttons, [CaptionButton::Close], "close only");
    let title = PointF { x: 100.0, y: 16.0 };
    assert_eq!(layout.hit(title, false), Hit::Drag, "the caption drags");
    assert_eq!(
        load(Os::Linux).natural_size(),
        None,
        "the frame fills its window"
    );
}

#[test]
fn the_settings_window_shows_the_settings_it_is_given() {
    // AC-12
    let mut doc = settings_window(Os::Mac);
    assert_eq!(
        doc.element_text("accent-label").as_deref(),
        Some("System (Blue)")
    );
    assert_eq!(doc.element_text("zoom-value").as_deref(), Some("100%"));

    let settings = Settings {
        theme: Theme::Dark,
        accent: AccentSetting::System,
        ui_zoom: UiZoom::nearest(200),
        ..Settings::default()
    };
    doc.set_settings(&settings, Accent::Green);
    let _ = doc.frame();
    assert_eq!(
        doc.element_text("accent-label").as_deref(),
        Some("System (Green)")
    );
    assert_eq!(doc.element_text("zoom-value").as_deref(), Some("200%"));

    // The checked radio is its group's Tab stop: Dark, then System.
    assert!(doc.focus_enter(FocusDirection::Forward));
    let theme = key(&mut doc, Key::Space);
    assert_eq!(command(theme), Some(ChromeCommand::SetTheme(Theme::Dark)));
    let _ = key(&mut doc, Key::Tab);
    let accent = key(&mut doc, Key::Space);
    assert_eq!(
        command(accent),
        Some(ChromeCommand::SetAccent(AccentSetting::System))
    );

    let preset = Settings {
        accent: AccentSetting::Preset(Accent::Orange),
        ..settings
    };
    doc.set_settings(&preset, Accent::Green);
    let _ = doc.frame();
    assert_eq!(doc.element_text("accent-label").as_deref(), Some("Orange"));
}

#[test]
fn clicks_in_the_settings_window_are_commands() {
    // AC-12, AC-13
    let mut doc = settings_window(Os::Windows);
    let clicks = [
        ("theme-dark", ChromeCommand::SetTheme(Theme::Dark)),
        (
            "accent-green",
            ChromeCommand::SetAccent(AccentSetting::Preset(Accent::Green)),
        ),
        (
            "accent-system",
            ChromeCommand::SetAccent(AccentSetting::System),
        ),
        ("zoom-in", ChromeCommand::Zoom(ZoomStep::In)),
        ("zoom-out", ChromeCommand::Zoom(ZoomStep::Out)),
    ];
    for (id, expected) in clicks {
        let at = center(element(&doc, id));
        assert_eq!(command(click(&mut doc, at)), Some(expected), "#{id}");
    }
    let close = doc.layout().caption_buttons[0].1;
    assert_eq!(
        command(click(&mut doc, center(close))),
        Some(ChromeCommand::Window(WindowAction::Close))
    );
}

#[test]
fn size_buttons_that_would_change_nothing_are_disabled() {
    // AC-6, AC-12
    let mut doc = settings_window(Os::Linux);
    let reset = center(element(&doc, "zoom-reset"));
    assert!(click(&mut doc, reset).command.is_none(), "already 100%");

    for (percent, stuck, live) in [(200, "zoom-in", "zoom-out"), (80, "zoom-out", "zoom-in")] {
        let settings = Settings {
            ui_zoom: UiZoom::nearest(percent),
            ..Settings::default()
        };
        doc.set_settings(&settings, Accent::Blue);
        let _ = doc.frame();
        let at = center(element(&doc, stuck));
        assert!(
            click(&mut doc, at).command.is_none(),
            "#{stuck} at {percent}%"
        );
        let at = center(element(&doc, live));
        assert!(
            click(&mut doc, at).command.is_some(),
            "#{live} at {percent}%"
        );
        let at = center(element(&doc, "zoom-reset"));
        assert_eq!(
            command(click(&mut doc, at)),
            Some(ChromeCommand::Zoom(ZoomStep::Reset))
        );
    }
}

#[test]
fn the_keyboard_works_the_whole_settings_window() {
    // AC-7: one stop per radio group, arrows move and select, then the
    // size buttons, then out (close is not a Tab stop, like the frame's).
    let mut doc = settings_window(Os::Linux);
    assert!(doc.focus_enter(FocusDirection::Forward));
    let light = key(&mut doc, Key::ArrowRight);
    assert_eq!(command(light), Some(ChromeCommand::SetTheme(Theme::Light)));

    let _ = key(&mut doc, Key::Tab);
    let blue = key(&mut doc, Key::ArrowRight);
    assert_eq!(
        command(blue),
        Some(ChromeCommand::SetAccent(AccentSetting::Preset(
            Accent::Blue
        )))
    );

    let _ = key(&mut doc, Key::Tab);
    let smaller = key(&mut doc, Key::Enter);
    assert_eq!(command(smaller), Some(ChromeCommand::Zoom(ZoomStep::Out)));
    let _ = key(&mut doc, Key::Tab);
    let bigger = key(&mut doc, Key::Space);
    assert_eq!(command(bigger), Some(ChromeCommand::Zoom(ZoomStep::In)));
    let _ = key(&mut doc, Key::Tab);
    assert!(doc.has_focus(), "Reset stays focusable while disabled");
    assert!(key(&mut doc, Key::Enter).command.is_none());

    // The address style, then the search engine list (spec 0008, AC-32).
    let _ = key(&mut doc, Key::Tab);
    let inline = key(&mut doc, Key::ArrowRight);
    assert_eq!(
        command(inline),
        Some(ChromeCommand::SetAddressStyle(ui::AddressStyle::Inline))
    );
    let _ = key(&mut doc, Key::Tab);
    let _ = key(&mut doc, Key::ArrowDown);
    let brave = key(&mut doc, Key::Enter);
    assert_eq!(
        command(brave),
        Some(ChromeCommand::SetSearchEngine(ui::EngineKind::Brave))
    );

    let out = key(&mut doc, Key::Tab);
    assert_eq!(out.focus_left, Some(FocusDirection::Forward));
}

#[test]
fn escape_closes_the_settings_window() {
    // AC-7
    let mut doc = settings_window(Os::Windows);
    assert!(doc.focus_enter(FocusDirection::Forward));
    let escape = key(&mut doc, Key::Escape);
    assert_eq!(
        command(escape),
        Some(ChromeCommand::Window(WindowAction::Close))
    );
    let mut frame = load(Os::Windows);
    assert!(frame.focus_enter(FocusDirection::Forward));
    assert!(
        key(&mut frame, Key::Escape).command.is_none(),
        "the browser window stays"
    );
}

#[test]
fn at_200_percent_with_large_text_nothing_clips_or_overlaps() {
    // AC-6: the window grows with the interface size, so its layout keeps
    // 480 chrome px; OS text at 150% makes everything bigger again.
    let settings = Settings {
        ui_zoom: UiZoom::nearest(200),
        ..Settings::default()
    };
    let mut appearance = OsAppearance::fallback(Os::Windows);
    appearance.set_text_scale(1.5);
    let doc = load_settings(Os::Windows, &settings, &appearance);
    let size = doc.natural_size().unwrap();
    let boxes: Vec<(&str, RectF)> = SETTINGS_CONTROLS
        .iter()
        .map(|id| (*id, element(&doc, id)))
        .collect();
    for (id, rect) in &boxes {
        assert!(rect.origin.x >= 0.0, "#{id} {rect:?}");
        assert!(
            rect.origin.x + rect.size.width <= size.width,
            "#{id} {rect:?}"
        );
        assert!(
            rect.origin.y + rect.size.height <= size.height,
            "#{id} {rect:?}"
        );
    }
    for (i, (a_id, a)) in boxes.iter().enumerate() {
        for (b_id, b) in &boxes[i + 1..] {
            let apart = a.origin.x + a.size.width <= b.origin.x
                || b.origin.x + b.size.width <= a.origin.x
                || a.origin.y + a.size.height <= b.origin.y
                || b.origin.y + b.size.height <= a.origin.y;
            assert!(apart, "#{a_id} {a:?} overlaps #{b_id} {b:?}");
        }
    }
}

#[test]
#[cfg(debug_assertions)]
fn the_gallery_buttons_open_the_file_dialogs() {
    // AC-10: the gallery ships in dev builds only. A window taller than
    // the gallery, so nothing scrolls.
    let tall = SizeF {
        width: 1280.0,
        height: 20_000.0,
    };
    let mut doc =
        ChromeDocument::load(ChromeSurface::Gallery, Os::Linux, tall, 1.0, Some(fonts())).unwrap();
    let _ = doc.frame();
    for (id, expected) in [
        ("dev-open-file", ChromeCommand::DevOpenFile),
        ("dev-save-file", ChromeCommand::DevSaveFile),
    ] {
        let at = center(doc.element_box(id).unwrap());
        assert_eq!(command(click(&mut doc, at)), Some(expected), "#{id}");
    }
}

fn url(text: &str) -> url::Url {
    url::Url::parse(text).unwrap()
}

/// A frame showing `committed`, idle.
fn showing(doc: &mut ChromeDocument, committed: &str) {
    doc.set_navigation(&ui::NavView {
        url: Some(url(committed)),
        ..ui::NavView::default()
    });
    let _ = doc.frame();
}

fn type_text(doc: &mut ChromeDocument, text: &str) {
    doc.input(InputEvent::Text { text: text.into() });
    let _ = doc.frame();
}

#[test]
fn the_pill_opens_the_palette_over_the_card_with_the_url_selected() {
    // Spec 0008, AC-2, AC-6, AC-7
    let mut doc = load(Os::Mac);
    showing(&mut doc, "https://www.example.com/docs");
    assert_eq!(
        doc.element_text("address-host").as_deref(),
        Some("www.example.com")
    );
    let pill = doc
        .element_box("address-pill")
        .expect("palette style is the default");
    assert!(doc.element_box("address-inline").is_none());
    assert_eq!(
        command(click(&mut doc, center(pill))),
        Some(ChromeCommand::OpenAddress)
    );
    doc.open_address();
    let frame = doc.frame();
    assert!(doc.palette_open());
    assert_eq!(doc.address().text(), "https://www.example.com/docs");
    let palette = doc.element_box("palette").expect("the palette shows");
    let width = (WINDOW.width - 32.0).min(600.0);
    assert_eq!(palette.size.width, width);
    assert_eq!(
        palette.origin.x,
        (WINDOW.width - width) / 2.0,
        "centered across the window"
    );
    assert_eq!(
        palette.origin.y,
        (WINDOW.height - palette.size.height) / 2.0,
        "and down it"
    );
    let overlay = frame.and_then(|f| f.overlay).expect("an overlay list");
    assert!(
        !overlay.items.is_empty(),
        "the palette paints in the overlay"
    );

    // Typing replaces the selected URL, and the intent row follows.
    type_text(&mut doc, "rust borrow");
    assert_eq!(
        doc.element_text("palette-intent-text").as_deref(),
        Some("Search DuckDuckGo for \"rust borrow\"")
    );
    let enter = key(&mut doc, Key::Enter);
    assert_eq!(command(enter), Some(ChromeCommand::AddressGo));
    let target = doc.address_go().unwrap();
    assert_eq!(target.as_str(), "https://duckduckgo.com/?q=rust+borrow");
    assert!(!doc.palette_open(), "Enter closes it");
}

#[test]
fn escape_or_a_press_on_the_card_closes_the_palette_and_reverts() {
    // Spec 0008, AC-6
    let mut doc = load(Os::Mac);
    showing(&mut doc, "https://a.test/");
    assert!(doc.focus_enter(FocusDirection::Forward));
    let _ = doc.frame();
    doc.open_address();
    let _ = doc.frame();
    type_text(&mut doc, "typed");
    let card = doc.layout().page.unwrap();
    assert_eq!(
        doc.layout().hit(center(card), false),
        Hit::Chrome,
        "the page gets no pointer input while it is open"
    );
    key(&mut doc, Key::Escape);
    assert!(!doc.palette_open());
    assert!(doc.has_focus(), "focus goes back to where it was");
    assert_eq!(doc.address().text(), "a.test", "the committed URL is back");
    assert_eq!(doc.layout().hit(center(card), false), Hit::Page);

    doc.open_address();
    let _ = doc.frame();
    type_text(&mut doc, "typed");
    // The card's bottom left: the palette sits in the middle of the window.
    let corner = PointF {
        x: card.origin.x + 20.0,
        y: card.origin.y + card.size.height - 20.0,
    };
    click(&mut doc, corner);
    assert!(!doc.palette_open(), "a press outside closes it");
    assert_eq!(doc.address().text(), "a.test");
}

#[test]
fn a_commit_while_typing_leaves_the_text_alone() {
    // Spec 0008, AC-12, Edge cases: the palette stays open.
    let mut doc = load(Os::Mac);
    showing(&mut doc, "https://a.test/");
    doc.open_address();
    let _ = doc.frame();
    type_text(&mut doc, "half typ");
    showing(&mut doc, "https://b.test/");
    assert!(doc.palette_open());
    assert_eq!(doc.address().text(), "half typ");
    key(&mut doc, Key::Escape);
    assert_eq!(
        doc.address().text(),
        "b.test",
        "the new URL once editing ends"
    );
}

#[test]
fn reload_turns_into_stop_and_the_bar_shows_while_loading() {
    // Spec 0008, AC-25
    let mut doc = load(Os::Mac);
    doc.set_navigation(&ui::NavView {
        url: Some(url("https://a.test/")),
        loading: true,
        ..ui::NavView::default()
    });
    let _ = doc.frame();
    assert_eq!(
        doc.element_text("nav-reload-icon").as_deref(),
        Some("\u{E003}")
    );
    let reload = doc.element_box("nav-reload").unwrap();
    assert_eq!(
        command(click(&mut doc, center(reload))),
        Some(ChromeCommand::Stop)
    );
    let card = doc.layout().page.unwrap();
    let bar = doc.element_box("loading-bar").expect("the bar shows");
    assert_eq!(
        (bar.size.width, bar.size.height),
        (160.0, 4.0),
        "a small track"
    );
    assert_eq!(bar.origin.y, card.origin.y + 12.0, "a little below the top");
    assert_eq!(
        bar.origin.x,
        card.origin.x + (card.size.width - 160.0) / 2.0,
        "centered over the card"
    );
    // The shimmer sweeps across it as time goes on.
    assert!(doc.tick_loading(0.0) || doc.element_box("loading-shimmer").is_some());
    let _ = doc.frame();
    let start = doc.element_box("loading-shimmer").unwrap().origin.x;
    assert!(doc.tick_loading(600.0));
    let _ = doc.frame();
    let middle = doc.element_box("loading-shimmer").unwrap().origin.x;
    assert!(middle > start, "{middle} after {start}");
    showing(&mut doc, "https://a.test/");
    assert!(doc.element_box("loading-bar").is_none());
    let reload = doc.element_box("nav-reload").unwrap();
    assert_eq!(
        command(click(&mut doc, center(reload))),
        Some(ChromeCommand::Reload)
    );
}

#[test]
fn the_status_bubble_shows_the_link_bottom_left_of_the_card() {
    // Spec 0008, AC-19
    let mut doc = load(Os::Mac);
    doc.set_link_hover(Some("https://www.example.com/a/b?c"));
    let _ = doc.frame();
    assert_eq!(
        doc.element_text("status-text").as_deref(),
        Some("www.example.com/a/b?c")
    );
    let card = doc.layout().page.unwrap();
    let bubble = doc.element_box("status-bubble").expect("it shows");
    assert!(bubble.origin.x >= card.origin.x && bubble.origin.x < card.origin.x + 16.0);
    let bottom = bubble.origin.y + bubble.size.height;
    let card_bottom = card.origin.y + card.size.height;
    assert!(
        bottom <= card_bottom && bottom > card_bottom - 16.0,
        "{bottom} vs {card_bottom}"
    );
    assert!(bubble.size.width <= card.size.width / 2.0);
    doc.set_link_hover(None);
    let _ = doc.frame();
    assert!(doc.element_box("status-bubble").is_none());
}

#[test]
fn the_inline_style_puts_a_field_in_the_sidebar() {
    // Spec 0008, AC-2, AC-5, AC-7
    let mut doc = load(Os::Mac);
    showing(&mut doc, "https://www.example.com/docs");
    doc.set_address_style(ui::AddressStyle::Inline);
    let _ = doc.frame();
    assert!(doc.element_box("address-pill").is_none());
    assert!(doc.element_box("address-inline").is_some());
    assert_eq!(
        doc.address().text(),
        "www.example.com",
        "the host while idle"
    );
    doc.open_address();
    let _ = doc.frame();
    assert!(doc.address().editing());
    assert_eq!(doc.address().text(), "https://www.example.com/docs");
    assert!(doc.element_box("address-intent").is_some());
    type_text(&mut doc, "example.org");
    assert_eq!(
        doc.element_text("address-intent-text").as_deref(),
        Some("Go to example.org")
    );
    key(&mut doc, Key::Escape);
    assert!(!doc.address().editing());
    assert!(!doc.has_focus(), "focus goes to the page");
    assert!(doc.element_box("address-intent").is_none());
}

#[test]
fn a_press_anywhere_outside_the_palette_closes_it() {
    // Spec 0008, AC-6: the sidebar and any button count, not only the card.
    let mut doc = load(Os::Mac);
    showing(&mut doc, "https://a.test/");
    doc.open_address();
    let _ = doc.frame();
    let inside = center(doc.element_box("palette").unwrap());
    assert!(!doc.dismiss_palette(inside), "a press inside keeps it");
    assert!(doc.palette_open());
    assert!(
        doc.dismiss_palette(PointF { x: 100.0, y: 600.0 }),
        "the empty sidebar"
    );
    assert!(!doc.palette_open());

    doc.open_address();
    let _ = doc.frame();
    doc.input(InputEvent::PointerDown {
        x: 100.0,
        y: 600.0,
        button: PointerButton::Secondary,
    });
    assert!(!doc.palette_open(), "any button");
}

#[test]
fn f6_lands_on_the_pill_or_the_inline_field() {
    // Spec 0008, AC-31
    let mut doc = load(Os::Mac);
    showing(&mut doc, "https://a.test/");
    doc.focus_sidebar();
    assert!(doc.has_focus(), "the pill");
    assert!(!doc.palette_open(), "focus only, the palette stays shut");
    let enter = key(&mut doc, Key::Enter);
    assert_eq!(command(enter), Some(ChromeCommand::OpenAddress));
    doc.blur();
    doc.set_address_style(ui::AddressStyle::Inline);
    let _ = doc.frame();
    doc.focus_sidebar();
    assert!(doc.address().editing(), "the field, URL selected");
}

#[test]
fn the_settings_window_picks_the_address_style_and_the_engine() {
    // Spec 0008, AC-32
    let mut doc =
        ChromeDocument::load(ChromeSurface::Settings, Os::Mac, WINDOW, 1.0, Some(fonts())).unwrap();
    let settings = Settings::default();
    doc.set_settings(&settings, Accent::Blue);
    let _ = doc.frame();
    let inline = doc
        .element_box("address-style-inline")
        .expect("the segmented control");
    assert_eq!(
        command(click(&mut doc, center(inline))),
        Some(ChromeCommand::SetAddressStyle(ui::AddressStyle::Inline))
    );
    assert!(
        doc.element_box("search-custom").is_none(),
        "hidden until Custom"
    );
    let options = doc.element_box("search-engine").expect("the listbox");
    let custom = PointF {
        x: options.origin.x + 20.0,
        y: options.origin.y + options.size.height - 8.0,
    };
    assert_eq!(
        command(click(&mut doc, custom)),
        Some(ChromeCommand::SetSearchEngine(ui::EngineKind::Custom))
    );
    doc.show_custom_search(None, false);
    let _ = doc.frame();
    let field = doc
        .element_box("search-custom-field")
        .expect("the URL field");
    click(&mut doc, center(field));
    type_text(&mut doc, "ftp://x/%s");
    assert_eq!(
        doc.input_value("search-custom-field").as_deref(),
        Some("ftp://x/%s")
    );
    let enter = key(&mut doc, Key::Enter);
    assert_eq!(command(enter), Some(ChromeCommand::SaveCustomSearch));
    doc.show_custom_search(None, true);
    let _ = doc.frame();
    assert!(
        doc.element_box("search-custom-error").is_some(),
        "the error line"
    );
}

#[test]
fn copy_cut_and_paste_work_in_the_inline_address() {
    // Spec 0008, AC-15
    let mut doc = load(Os::Mac);
    doc.set_address_style(ui::AddressStyle::Inline);
    showing(&mut doc, "https://www.example.com/docs");
    doc.open_address();
    let _ = doc.frame();
    assert!(doc.has_focus(), "the field has focus");
    let copied = doc.input(InputEvent::Copy { request: 0 }).copied;
    assert_eq!(copied.as_deref(), Some("https://www.example.com/docs"));
    let cut = doc.input(InputEvent::Cut { request: 0 }).copied;
    assert_eq!(cut.as_deref(), Some("https://www.example.com/docs"));
    let _ = doc.frame();
    assert_eq!(doc.address().text(), "");
    doc.input(InputEvent::Paste {
        text: "rust.org".into(),
    });
    let _ = doc.frame();
    assert_eq!(doc.address().text(), "rust.org");
}

#[test]
fn a_click_on_empty_sidebar_space_takes_focus_from_the_inline_address() {
    // Spec 0008, AC-5
    let mut doc = load(Os::Mac);
    doc.set_address_style(ui::AddressStyle::Inline);
    showing(&mut doc, "https://www.example.com/docs");
    doc.open_address();
    let _ = doc.frame();
    assert!(doc.has_focus());
    click(&mut doc, PointF { x: 100.0, y: 500.0 });
    let _ = doc.frame();
    assert!(!doc.has_focus(), "the field lets go of focus");
    assert_eq!(
        doc.address().text(),
        "www.example.com",
        "and shows the host again"
    );
}
