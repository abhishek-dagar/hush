//! macOS: a transparent, full size titlebar that keeps the traffic lights,
//! centered on the sidebar's top row as in Zen, and appearance from AppKit
//! (spec 0003, `OsAppearance`).
#![allow(
    unsafe_code,
    reason = "AppKit notification statics, observers and the window's views (spec 0003)"
)]

use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSColor, NSColorSpace, NSSystemColorsDidChangeNotification, NSView, NSWindow, NSWindowButton,
    NSWorkspace, NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol};
use ui::{FrameLayout, Os, OsAppearance, Rgba};
use winit::event_loop::EventLoopProxy;
use winit::platform::macos::WindowAttributesExtMacOS;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Theme, Window, WindowAttributes};

use crate::browser::UserEvent;
use crate::chrome_host::ChromeEvent;

/// No title bar: the content runs under a transparent titlebar, and the
/// native traffic lights stay (spec 0003, AC-8).
pub(super) fn window_attributes(attributes: WindowAttributes) -> WindowAttributes {
    attributes
        .with_titlebar_transparent(true)
        .with_title_hidden(true)
        .with_fullsize_content_view(true)
}

type Observer = Retained<ProtocolObject<dyn NSObjectProtocol>>;

/// The notification observers, removed when dropped, and the window whose
/// traffic lights it places.
pub(super) struct Platform {
    observers: Vec<(Retained<NSNotificationCenter>, Observer)>,
    window: Option<Retained<NSWindow>>,
}

impl Platform {
    /// Watches for accent and accessibility changes. Dark mode arrives as
    /// winit's `ThemeChanged`.
    pub(super) fn attach(window: &Window, proxy: EventLoopProxy<UserEvent>) -> Self {
        let ns_window = ns_window(window);
        let block = RcBlock::new(move |_: NonNull<NSNotification>| {
            // The loop is gone when this fails; nothing is left to tell.
            let _ = proxy.send_event(UserEvent::Chrome(ChromeEvent::OsChanged));
        });
        let default_center = NSNotificationCenter::defaultCenter();
        let workspace_center = NSWorkspace::sharedWorkspace().notificationCenter();
        // SAFETY: both names are AppKit's own constant strings, valid for
        // the life of the process.
        let names = unsafe {
            [
                (default_center, NSSystemColorsDidChangeNotification),
                (
                    workspace_center,
                    NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification,
                ),
            ]
        };
        let observers = names
            .into_iter()
            .map(|(center, name)| {
                // SAFETY: the block only sends on a thread safe proxy, so it
                // is fine on whatever thread posts. A `None` queue runs it
                // on the posting thread.
                let observer = unsafe {
                    center.addObserverForName_object_queue_usingBlock(
                        Some(name),
                        None,
                        None,
                        &block,
                    )
                };
                (center, observer)
            })
            .collect();
        Self {
            observers,
            window: ns_window,
        }
    }

    /// The current OS appearance.
    pub(super) fn read(&self, window: &Window) -> OsAppearance {
        let mut os = OsAppearance::fallback(Os::Mac);
        os.dark = window.theme() == Some(Theme::Dark);
        os.accent = NSColor::controlAccentColor()
            .colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())
            .map(|c| Rgba {
                r: c.redComponent() as f32,
                g: c.greenComponent() as f32,
                b: c.blueComponent() as f32,
                a: 1.0,
            });
        let workspace = NSWorkspace::sharedWorkspace();
        os.high_contrast = workspace.accessibilityDisplayShouldIncreaseContrast();
        os.reduce_motion = workspace.accessibilityDisplayShouldReduceMotion();
        os
    }

    /// Centers the traffic lights vertically on the sidebar's top row
    /// (`layout.title_row`), the way Electron's `trafficLightPosition`
    /// does: the titlebar view holding them grows to reach the row, and
    /// each button moves down inside it. Hit testing stays native. Called
    /// after every chrome layout, so a resize that resets them is undone.
    pub(super) fn set_frame(&self, layout: FrameLayout, chrome_scale: f32) {
        let (Some(window), Some(row)) = (&self.window, layout.title_row) else {
            return;
        };
        let Some(close) = window.standardWindowButton(NSWindowButton::CloseButton) else {
            return;
        };
        // SAFETY: AppKit view accessors on the main thread, where the
        // event loop calls this; the views are the window's own.
        let Some(titlebar) = (unsafe { close.superview().and_then(|v| v.superview()) }) else {
            return;
        };
        // Chrome px to window points: device px per chrome px over device
        // px per point.
        let points = f64::from(chrome_scale) / window.backingScaleFactor();
        let row_center = f64::from(row.origin.y + row.size.height / 2.0) * points;
        let button_height = close.frame().size.height;
        // The container is as tall as the buttons hang down; a container
        // origin is from the window's bottom.
        let height = (row_center + button_height / 2.0).ceil();
        let mut frame = titlebar.frame();
        frame.size.height = height;
        frame.origin.y = window.frame().size.height - height;
        titlebar.setFrame(frame);
        for kind in [
            NSWindowButton::CloseButton,
            NSWindowButton::MiniaturizeButton,
            NSWindowButton::ZoomButton,
        ] {
            if let Some(button) = window.standardWindowButton(kind) {
                let mut origin = button.frame().origin;
                origin.y = height - row_center - button_height / 2.0;
                button.setFrameOrigin(origin);
            }
        }
    }
}

/// The AppKit window behind winit's, from its content view.
fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return None;
    };
    // SAFETY: winit hands out its live NSView; retaining it keeps it valid
    // for the call, and we only ask it for its window.
    let view = unsafe { Retained::retain(appkit.ns_view.as_ptr().cast::<NSView>()) }?;
    view.window()
}

impl Drop for Platform {
    fn drop(&mut self) {
        for (center, observer) in &self.observers {
            // SAFETY: `observer` was returned by this center's
            // `addObserverForName…` and is removed once.
            unsafe { center.removeObserver(observer.as_ref()) };
        }
    }
}
