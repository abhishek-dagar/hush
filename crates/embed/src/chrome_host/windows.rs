//! Windows: an undecorated window whose hit test the chrome answers, so
//! the frame drags, resizes and opens Snap Layouts from its own maximize
//! button; appearance from the registry and system parameters
//! (spec 0003, frame anatomy and `OsAppearance`).
//!
//! The chrome document draws all three window buttons. Minimize and close
//! are ordinary chrome controls. Maximize answers `HTMAXBUTTON`, which is
//! what opens Snap Layouts, so the OS owns the pointer there: its clicks
//! and its hover come back through this subclass instead of winit.
#![allow(
    unsafe_code,
    reason = "Win32 window subclass, registry and system parameter calls (spec 0003, frame anatomy)"
)]

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;

use tracing::warn;
use ui::{
    CaptionButton, ChromeCommand, FrameLayout, Hit, Os, OsAppearance, PointF, ResizeEdge, Rgba,
    WindowAction,
};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWM_WINDOW_CORNER_PREFERENCE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows_sys::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTLEFT, HTMAXBUTTON, HTRIGHT,
    HTTOP, HTTOPLEFT, HTTOPRIGHT, IsZoomed, SPI_GETCLIENTAREAANIMATION, SPI_GETHIGHCONTRAST,
    SystemParametersInfoW, WM_NCDESTROY, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN,
    WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_SETTINGCHANGE, WM_SYSCOLORCHANGE,
};
use winit::event_loop::EventLoopProxy;
use winit::platform::windows::WindowAttributesExtWindows;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Theme, Window, WindowAttributes};

use crate::browser::UserEvent;
use crate::chrome_host::ChromeEvent;

/// Our subclass's id on the window.
const SUBCLASS_ID: usize = 0x0003;

/// No title bar or borders, but keep DWM's shadow (spec 0003, AC-8).
pub(super) fn window_attributes(attributes: WindowAttributes) -> WindowAttributes {
    attributes
        .with_decorations(false)
        .with_undecorated_shadow(true)
}

/// The window's `HWND`, as muda wants it.
pub(super) fn hwnd(window: &Window) -> Option<isize> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

/// What the subclass procedure shares with the event loop. Both run on
/// the window's thread, so `Cell` and `RefCell` are enough.
struct Shared {
    proxy: EventLoopProxy<UserEvent>,
    /// The frame in chrome px, and chrome px to physical pixels.
    frame: RefCell<Option<(FrameLayout, f32)>>,
    /// The maximize button is pressed and not released yet.
    pressed: Cell<bool>,
    /// The pointer is over the maximize button.
    over_maximize: Cell<bool>,
    /// `WM_NCMOUSELEAVE` is asked for.
    tracking: Cell<bool>,
    /// Cleared when the subclass is removed, so it is removed once.
    attached: Cell<bool>,
}

impl Shared {
    fn post(&self, event: ChromeEvent) {
        // The loop is gone when this fails; nothing is left to tell.
        let _ = self.proxy.send_event(UserEvent::Chrome(event));
    }
}

/// The window subclass; removed when dropped.
pub(super) struct Platform {
    hwnd: Option<HWND>,
    shared: Rc<Shared>,
}

impl Platform {
    /// Subclasses the window and asks DWM for rounded corners.
    pub(super) fn attach(window: &Window, proxy: EventLoopProxy<UserEvent>) -> Self {
        let shared = Rc::new(Shared {
            proxy,
            frame: RefCell::new(None),
            pressed: Cell::new(false),
            over_maximize: Cell::new(false),
            tracking: Cell::new(false),
            attached: Cell::new(false),
        });
        let Some(hwnd) = hwnd(window).map(|h| h as HWND) else {
            warn!("no Win32 window handle, the frame will not drag or snap");
            return Self { hwnd: None, shared };
        };

        let preference: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
        // SAFETY: `hwnd` is the live window; the pointer and size describe
        // `preference`, which outlives the call. Windows 10 rejects the
        // attribute, which only means square corners.
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                (&raw const preference).cast::<c_void>(),
                size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
        }

        let data = Rc::into_raw(Rc::clone(&shared)) as usize;
        // SAFETY: `subclass_proc` matches `SUBCLASSPROC`; `data` is a leaked
        // `Rc<Shared>` the procedure releases on `WM_NCDESTROY`, or `Drop`
        // does when it removes the subclass first.
        let ok = unsafe { SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, data) };
        if ok == 0 {
            // SAFETY: the subclass was not installed, so this is the only
            // owner of the leaked count.
            drop(unsafe { Rc::from_raw(data as *const Shared) });
            warn!("could not subclass the window, the frame will not drag or snap");
            return Self { hwnd: None, shared };
        }
        shared.attached.set(true);
        Self {
            hwnd: Some(hwnd),
            shared,
        }
    }

    /// The current OS appearance.
    pub(super) fn read(&self, window: &Window) -> OsAppearance {
        let mut os = OsAppearance::fallback(Os::Windows);
        os.dark = window.theme() == Some(Theme::Dark);
        // ABGR: the low byte is red.
        os.accent = registry_dword(r"Software\Microsoft\Windows\DWM", "AccentColor")
            .map(|abgr| Rgba::rgb8(abgr as u8, (abgr >> 8) as u8, (abgr >> 16) as u8));
        os.high_contrast = high_contrast();
        os.reduce_motion = !client_area_animation();
        if let Some(percent) =
            registry_dword(r"Software\Microsoft\Accessibility", "TextScaleFactor")
        {
            os.set_text_scale(percent as f32 / 100.0);
        }
        os
    }

    /// The frame the hit test answers from.
    pub(super) fn set_frame(&self, layout: FrameLayout, chrome_scale: f32) {
        *self.shared.frame.borrow_mut() = Some((layout, chrome_scale));
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        let Some(hwnd) = self.hwnd else {
            return;
        };
        if self.shared.attached.replace(false) {
            // SAFETY: the subclass is still installed (`attached`), so the
            // window is alive and the leaked count has not been released.
            unsafe {
                RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID);
                drop(Rc::from_raw(Rc::as_ptr(&self.shared)));
            }
        }
    }
}

/// Answers the hit test from the frame, turns maximize clicks into
/// commands, reports the pointer on and off maximize, and reports
/// appearance changes.
unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    // SAFETY: `data` is the `Rc<Shared>` leaked in `attach`; it stays alive
    // until this procedure is removed below or by `Drop`.
    let shared = unsafe { &*(data as *const Shared) };
    let maximize_hit = |code: WPARAM| code as u32 == HTMAXBUTTON;
    match message {
        WM_NCHITTEST => {
            if let Some(code) = hit_test(hwnd, lparam, shared) {
                return code as LRESULT;
            }
        }
        // DefWindowProc would draw a classic button; the chrome draws its own.
        WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if maximize_hit(wparam) => {
            shared.pressed.set(true);
            return 0;
        }
        WM_NCLBUTTONUP if maximize_hit(wparam) => {
            if shared.pressed.replace(false) {
                let action = WindowAction::Maximize;
                shared.post(ChromeEvent::Command(ChromeCommand::Window(action)));
            }
            return 0;
        }
        WM_NCMOUSEMOVE => {
            let over = wparam as u32 == HTMAXBUTTON;
            if shared.over_maximize.replace(over) != over {
                shared.post(ChromeEvent::MaximizeHover(over));
            }
            if !shared.tracking.get() {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE | TME_NONCLIENT,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                // SAFETY: `track` is a TRACKMOUSEEVENT with `cbSize` set,
                // for the live window this procedure runs for.
                let ok = unsafe { TrackMouseEvent(&mut track) };
                shared.tracking.set(ok != 0);
            }
        }
        WM_NCMOUSELEAVE => {
            shared.tracking.set(false);
            if shared.over_maximize.replace(false) {
                shared.post(ChromeEvent::MaximizeHover(false));
            }
        }
        WM_SETTINGCHANGE | WM_SYSCOLORCHANGE => shared.post(ChromeEvent::OsChanged),
        // Removing our own subclass from inside it is allowed. `attached`
        // makes the leaked count drop once: here, or in `Drop`, not both.
        WM_NCDESTROY if shared.attached.replace(false) => {
            // SAFETY: the subclass is still installed and its count not yet
            // released (`attached` was true).
            unsafe {
                RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID);
                drop(Rc::from_raw(data as *const Shared));
            }
            // SAFETY: forwarding the message to the next procedure.
            return unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
        }
        _ => {}
    }
    // SAFETY: forwarding the message to the next procedure in the chain.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

/// The hit code for the point in `lparam` (screen coordinates), or `None`
/// before the first frame is known.
fn hit_test(hwnd: HWND, lparam: LPARAM, shared: &Shared) -> Option<u32> {
    let frame = shared.frame.borrow();
    let (layout, chrome_scale) = frame.as_ref()?;
    // The low and high words, sign extended: points left of or above the
    // primary monitor are negative.
    let mut point = POINT {
        x: i32::from(lparam as u16 as i16),
        y: i32::from((lparam >> 16) as u16 as i16),
    };
    // SAFETY: `hwnd` is the live window this procedure runs for, and
    // `point` is a valid, writable POINT.
    if unsafe { ScreenToClient(hwnd, &mut point) } == 0 {
        return None;
    }
    let point = PointF {
        x: point.x as f32 / chrome_scale,
        y: point.y as f32 / chrome_scale,
    };
    // SAFETY: `hwnd` is the live window.
    let maximized = unsafe { IsZoomed(hwnd) } != 0;
    Some(match layout.hit(point, maximized) {
        Hit::Resize(ResizeEdge::North) => HTTOP,
        Hit::Resize(ResizeEdge::South) => HTBOTTOM,
        Hit::Resize(ResizeEdge::East) => HTRIGHT,
        Hit::Resize(ResizeEdge::West) => HTLEFT,
        Hit::Resize(ResizeEdge::NorthEast) => HTTOPRIGHT,
        Hit::Resize(ResizeEdge::NorthWest) => HTTOPLEFT,
        Hit::Resize(ResizeEdge::SouthEast) => HTBOTTOMRIGHT,
        Hit::Resize(ResizeEdge::SouthWest) => HTBOTTOMLEFT,
        // `HTMAXBUTTON` is what opens Snap Layouts on hover (Windows 11).
        Hit::Caption(CaptionButton::Maximize) => HTMAXBUTTON,
        // The chrome document's own controls, minimize and close among
        // them, take the pointer like the page does.
        Hit::Caption(CaptionButton::Minimize | CaptionButton::Close) | Hit::Chrome | Hit::Page => {
            HTCLIENT
        }
        Hit::Drag => HTCAPTION,
    })
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A `REG_DWORD` under `HKEY_CURRENT_USER`, or `None` if it is not there.
fn registry_dword(key: &str, value: &str) -> Option<u32> {
    let (key, value) = (wide(key), wide(value));
    let mut data = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: both names are NUL terminated UTF-16; `data` and `size`
    // describe a writable u32, and RRF_RT_REG_DWORD writes at most that.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&raw mut data).cast::<c_void>(),
            &mut size,
        )
    };
    (status == 0).then_some(data)
}

fn high_contrast() -> bool {
    let mut info = HIGHCONTRASTW {
        cbSize: size_of::<HIGHCONTRASTW>() as u32,
        dwFlags: 0,
        lpszDefaultScheme: std::ptr::null_mut(),
    };
    // SAFETY: `info` is a HIGHCONTRASTW with `cbSize` set, as the call asks.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            info.cbSize,
            (&raw mut info).cast::<c_void>(),
            0,
        )
    };
    ok != 0 && info.dwFlags & HCF_HIGHCONTRASTON != 0
}

/// Whether Windows animates controls; off means "reduce motion".
fn client_area_animation() -> bool {
    let mut enabled = 1i32;
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL into `enabled`.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&raw mut enabled).cast::<c_void>(),
            0,
        )
    };
    ok == 0 || enabled != 0
}
