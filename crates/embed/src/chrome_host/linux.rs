//! Linux: an undecorated window with the chrome's own buttons and resize
//! edges, and appearance from the XDG Settings portal, which works on every
//! desktop and inside Flatpak (spec 0003, `OsAppearance`).

use std::thread;

use ashpd::desktop::settings::{ColorScheme, Contrast, ReducedMotion, Settings};
use futures_util::StreamExt;
use tracing::{info, warn};
use ui::{ButtonLayout, FrameLayout, Os, OsAppearance, Rgba};
use winit::event_loop::EventLoopProxy;
use winit::window::{Theme, Window, WindowAttributes};

use crate::browser::UserEvent;
use crate::chrome_host::ChromeEvent;

const GNOME_INTERFACE: &str = "org.gnome.desktop.interface";
const GNOME_WM: &str = "org.gnome.desktop.wm.preferences";

/// No title bar or borders; the chrome draws the buttons (spec 0003, AC-8).
pub(super) fn window_attributes(attributes: WindowAttributes) -> WindowAttributes {
    attributes.with_decorations(false)
}

/// What the portal said, read whole on each change.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PortalAppearance {
    /// `None` when the desktop has no preference: winit's theme decides.
    dark: Option<bool>,
    accent: Option<Rgba>,
    high_contrast: bool,
    reduce_motion: bool,
    text_scale: Option<f32>,
    button_layout: Option<ButtonLayout>,
}

/// The last portal values; the listener thread sends new ones.
pub(super) struct Platform {
    portal: Option<PortalAppearance>,
}

impl Platform {
    /// Starts the portal listener. Without a portal (no D-Bus session, an
    /// old desktop) the chrome keeps winit's theme and the fallbacks.
    pub(super) fn attach(_window: &Window, proxy: EventLoopProxy<UserEvent>) -> Self {
        let spawned = thread::Builder::new()
            .name("appearance-portal".into())
            .spawn(move || listen(&proxy));
        if let Err(err) = spawned {
            warn!(%err, "could not start the appearance portal listener");
        }
        Self { portal: None }
    }

    /// Takes the values the listener thread sent.
    pub(super) fn set_portal(&mut self, portal: PortalAppearance) {
        self.portal = Some(portal);
    }

    /// The current OS appearance: the portal's values over winit's theme.
    pub(super) fn read(&self, window: &Window) -> OsAppearance {
        let mut os = OsAppearance::fallback(Os::Linux);
        os.dark = window.theme() == Some(Theme::Dark);
        if let Some(portal) = &self.portal {
            os.dark = portal.dark.unwrap_or(os.dark);
            os.accent = portal.accent;
            os.high_contrast = portal.high_contrast;
            os.reduce_motion = portal.reduce_motion;
            if let Some(scale) = portal.text_scale {
                os.set_text_scale(scale);
            }
            if let Some(layout) = &portal.button_layout {
                os.button_layout = layout.clone();
            }
        }
        os
    }

    /// Linux hit tests in the event loop; nothing to update.
    pub(super) fn set_frame(&self, _layout: FrameLayout, _chrome_scale: f32) {}
}

/// Runs on the listener thread until the portal or the event loop goes away.
fn listen(proxy: &EventLoopProxy<UserEvent>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            warn!(%err, "could not start the appearance portal runtime");
            return;
        }
    };
    let result = runtime.block_on(async {
        let settings = Settings::new().await?;
        let send = |portal| {
            proxy
                .send_event(UserEvent::Chrome(ChromeEvent::Portal(portal)))
                .is_ok()
        };
        if !send(snapshot(&settings).await) {
            return Ok(());
        }
        let mut changes = settings.receive_setting_changed().await?;
        while changes.next().await.is_some() {
            if !send(snapshot(&settings).await) {
                break;
            }
        }
        Ok::<(), ashpd::Error>(())
    });
    match result {
        Ok(()) => info!("appearance portal listener stopped"),
        Err(err) => warn!(%err, "no XDG Settings portal, using the window theme and defaults"),
    }
}

/// Reads every value the chrome uses. A key the desktop lacks keeps its
/// fallback.
async fn snapshot(settings: &Settings) -> PortalAppearance {
    let dark = match settings.color_scheme().await {
        Ok(ColorScheme::PreferDark) => Some(true),
        Ok(ColorScheme::PreferLight) => Some(false),
        _ => None,
    };
    // Channels outside 0 to 1 mean "no accent" (XDG Settings portal).
    let accent = settings.accent_color().await.ok().and_then(|c| {
        let channels = [c.red(), c.green(), c.blue()];
        channels
            .iter()
            .all(|v| (0.0..=1.0).contains(v))
            .then(|| Rgba {
                r: channels[0] as f32,
                g: channels[1] as f32,
                b: channels[2] as f32,
                a: 1.0,
            })
    });
    let high_contrast = matches!(settings.contrast().await, Ok(Contrast::High));
    let reduce_motion = matches!(
        settings.reduced_motion().await,
        Ok(ReducedMotion::ReducedMotion)
    ) || matches!(
        settings
            .read::<bool>(GNOME_INTERFACE, "enable-animations")
            .await,
        Ok(false)
    );
    let text_scale = settings
        .read::<f64>(GNOME_INTERFACE, "text-scaling-factor")
        .await
        .ok()
        .map(|scale| scale as f32);
    let button_layout = settings
        .read::<String>(GNOME_WM, "button-layout")
        .await
        .ok()
        .map(|layout| ButtonLayout::parse_gnome(&layout));
    PortalAppearance {
        dark,
        accent,
        high_contrast,
        reduce_motion,
        text_scale,
        button_layout,
    }
}
