//! The OS clipboard (spec 0008, AC-15). One handle is opened when the
//! browser starts and held for the whole run: on X11 the clipboard's
//! contents belong to the process that set them, so dropping the handle
//! would lose what the user copied.
//!
//! Every failure logs one `warn` for the session and changes nothing. The
//! clipboard is read only when the user presses the paste key, and written
//! only for a copy or cut the browser itself sent.

use tracing::warn;

/// The OS clipboard, opened once.
pub(crate) struct Clipboard {
    /// `None` when the OS refused to open it at startup.
    inner: Option<arboard::Clipboard>,
    /// A failure was already logged: AC-15 asks for one, not per key.
    warned: bool,
}

impl Clipboard {
    /// Opens the OS clipboard. A failure is logged; copy and paste stay off.
    pub(crate) fn new() -> Self {
        match arboard::Clipboard::new() {
            Ok(inner) => Self {
                inner: Some(inner),
                warned: false,
            },
            Err(err) => {
                warn!(%err, "clipboard unavailable; copy and paste are off");
                Self {
                    inner: None,
                    warned: true,
                }
            }
        }
    }

    /// The clipboard's text, if it holds text and can be read.
    pub(crate) fn read(&mut self) -> Option<String> {
        let inner = self.inner.as_mut()?;
        match inner.get_text() {
            Ok(text) => Some(text),
            Err(err) => {
                self.warn(err);
                None
            }
        }
    }

    /// Puts `text` on the clipboard.
    pub(crate) fn write(&mut self, text: &str) {
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        if let Err(err) = inner.set_text(text.to_owned()) {
            self.warn(err);
        }
    }

    fn warn(&mut self, err: arboard::Error) {
        if !self.warned {
            self.warned = true;
            warn!(%err, "clipboard unavailable; copy and paste are off");
        }
    }
}
