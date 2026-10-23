//! macOS: a transparent, full size titlebar that keeps the traffic lights,
//! and appearance from AppKit (spec 0003, `OsAppearance`).
#![allow(
    unsafe_code,
    reason = "AppKit notification statics and observers (spec 0003, OS appearance)"
)]

use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSColor, NSColorSpace, NSSystemColorsDidChangeNotification, NSWorkspace,
    NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol};
use ui::{FrameLayout, Os, OsAppearance, Rgba};
use winit::event_loop::EventLoopProxy;
use winit::platform::macos::WindowAttributesExtMacOS;
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

/// The notification observers; removed when dropped.
pub(super) struct Platform {
    observers: Vec<(Retained<NSNotificationCenter>, Observer)>,
}

impl Platform {
    /// Watches for accent and accessibility changes. Dark mode arrives as
    /// winit's `ThemeChanged`.
    pub(super) fn attach(_window: &Window, proxy: EventLoopProxy<UserEvent>) -> Self {
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
        Self { observers }
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

    /// macOS hit tests natively; nothing to update.
    pub(super) fn set_frame(&self, _layout: FrameLayout, _chrome_scale: f32) {}
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
