//! The menu model as native menus (spec 0003, AC-9): the menu bar on
//! macOS, a popup on Windows. Linux draws its menus in the window, as the
//! chrome document's overlay, so here it does nothing.

#[cfg(any(target_os = "macos", windows))]
pub(crate) use native::NativeMenu;

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) use overlay::NativeMenu;

#[cfg(any(target_os = "macos", windows))]
mod native {
    use std::sync::Mutex;

    use muda::accelerator::{Accelerator, Code, Modifiers};
    use muda::{CheckMenuItem, IsMenuItem, MenuEvent, PredefinedMenuItem, Submenu};
    use tracing::warn;
    use ui::{Menu, MenuItem, NativeItem, Shortcut, ShortcutKey};
    use winit::event_loop::EventLoopProxy;
    #[cfg(windows)]
    use winit::window::Window;

    use crate::browser::UserEvent;
    use crate::chrome_host::ChromeEvent;

    /// The muda menu built from the current model.
    #[derive(Default)]
    pub(crate) struct NativeMenu {
        menu: Option<muda::Menu>,
    }

    impl NativeMenu {
        /// Routes menu activations to the event loop.
        pub(crate) fn attach(&mut self, proxy: EventLoopProxy<UserEvent>) {
            // muda wants a `Sync` handler; the proxy is only `Send`.
            let proxy = Mutex::new(proxy);
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                let Ok(proxy) = proxy.lock() else {
                    return;
                };
                // The loop is gone when this fails; nothing is left to tell.
                let _ = proxy.send_event(UserEvent::Chrome(ChromeEvent::Menu(event.id.0)));
            }));
        }

        /// Rebuilds the menu from `model`, e.g. after a setting changed its checks.
        pub(crate) fn set(&mut self, model: &Menu) {
            let menu = muda::Menu::new();
            for item in &model.items {
                if let Err(err) = append(&menu, item) {
                    warn!(%err, "could not build a menu item");
                }
            }
            #[cfg(target_os = "macos")]
            menu.init_for_nsapp();
            self.menu = Some(menu);
        }

        /// Opens the app menu under the `≡` button, at `anchor` in physical
        /// pixels, or at the cursor without one. Windows only: macOS has a
        /// menu bar instead.
        #[cfg(windows)]
        #[allow(unsafe_code, reason = "muda's popup needs the raw window handle")]
        pub(crate) fn popup(&self, window: &Window, _model: &Menu, anchor: Option<(f32, f32)>) {
            use muda::ContextMenu;
            use muda::dpi::{PhysicalPosition, Position};
            let (Some(menu), Some(hwnd)) = (&self.menu, super::super::platform::hwnd(window))
            else {
                return;
            };
            let at =
                anchor.map(|(x, y)| Position::Physical(PhysicalPosition::new(x as i32, y as i32)));
            // SAFETY: `hwnd` is the live window's handle, read from winit on
            // the event loop thread that owns the window.
            unsafe {
                menu.show_context_menu_for_hwnd(hwnd, at);
            }
        }
    }

    /// Something items can be appended to: the root menu or a submenu.
    trait Container {
        fn add(&self, item: &dyn IsMenuItem) -> muda::Result<()>;
    }

    impl Container for muda::Menu {
        fn add(&self, item: &dyn IsMenuItem) -> muda::Result<()> {
            self.append(item)
        }
    }

    impl Container for Submenu {
        fn add(&self, item: &dyn IsMenuItem) -> muda::Result<()> {
            self.append(item)
        }
    }

    fn append(parent: &dyn Container, item: &MenuItem) -> muda::Result<()> {
        match item {
            MenuItem::Command {
                label,
                command,
                shortcut,
                checked: None,
            } => parent.add(&muda::MenuItem::with_id(
                command.id(),
                label,
                true,
                shortcut.map(accelerator),
            )),
            MenuItem::Command {
                label,
                command,
                shortcut,
                checked: Some(checked),
            } => parent.add(&CheckMenuItem::with_id(
                command.id(),
                label,
                true,
                *checked,
                shortcut.map(accelerator),
            )),
            MenuItem::Submenu { label, items } => {
                let submenu = Submenu::new(label, true);
                for child in items {
                    append(&submenu, child)?;
                }
                parent.add(&submenu)
            }
            MenuItem::Separator => parent.add(&PredefinedMenuItem::separator()),
            MenuItem::Native(native) => parent.add(&predefined(*native)),
        }
    }

    fn predefined(item: NativeItem) -> PredefinedMenuItem {
        match item {
            NativeItem::About => PredefinedMenuItem::about(None, None),
            NativeItem::Services => PredefinedMenuItem::services(None),
            NativeItem::Hide => PredefinedMenuItem::hide(None),
            NativeItem::HideOthers => PredefinedMenuItem::hide_others(None),
            NativeItem::ShowAll => PredefinedMenuItem::show_all(None),
            NativeItem::Quit => PredefinedMenuItem::quit(None),
            NativeItem::Undo => PredefinedMenuItem::undo(None),
            NativeItem::Redo => PredefinedMenuItem::redo(None),
            NativeItem::Cut => PredefinedMenuItem::cut(None),
            NativeItem::Copy => PredefinedMenuItem::copy(None),
            NativeItem::Paste => PredefinedMenuItem::paste(None),
            NativeItem::SelectAll => PredefinedMenuItem::select_all(None),
        }
    }

    fn accelerator(shortcut: Shortcut) -> Accelerator {
        let mut modifiers = Modifiers::empty();
        if shortcut.modifiers.primary {
            modifiers |= if cfg!(target_os = "macos") {
                Modifiers::META
            } else {
                Modifiers::CONTROL
            };
        }
        if shortcut.modifiers.alt {
            modifiers |= Modifiers::ALT;
        }
        if shortcut.modifiers.shift {
            modifiers |= Modifiers::SHIFT;
        }
        let code = match shortcut.key {
            ShortcutKey::Comma => Code::Comma,
            ShortcutKey::Equal => Code::Equal,
            ShortcutKey::Minus => Code::Minus,
            ShortcutKey::Digit0 => Code::Digit0,
            ShortcutKey::Letter('m') => Code::KeyM,
            ShortcutKey::Letter('o') => Code::KeyO,
            ShortcutKey::Letter('s') => Code::KeyS,
            ShortcutKey::Letter('w') => Code::KeyW,
            // The model only uses the letters above; an unknown one gets no
            // key, which muda shows as no shortcut.
            ShortcutKey::Letter(_) => Code::Unidentified,
        };
        Accelerator::new(modifiers, code)
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod overlay {
    use ui::Menu;
    use winit::event_loop::EventLoopProxy;

    use crate::browser::UserEvent;

    /// Linux menus render in the window as the chrome document's overlay,
    /// so there is no native menu to keep.
    #[derive(Default)]
    pub(crate) struct NativeMenu {}

    impl NativeMenu {
        /// Nothing to route: the overlay's rows are chrome commands.
        pub(crate) fn attach(&mut self, _proxy: EventLoopProxy<UserEvent>) {}

        /// The model is read straight from `ChromeHost` for the overlay and
        /// the shortcuts.
        pub(crate) fn set(&mut self, _model: &Menu) {}
    }
}
