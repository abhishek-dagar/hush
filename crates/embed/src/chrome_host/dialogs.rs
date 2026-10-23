//! Native open and save file dialogs through rfd (spec 0003, AC-10). The
//! dialog starts on the main thread, where macOS attaches it to the window
//! as a sheet; a worker thread waits for the answer so the event loop keeps
//! drawing, then posts it back as a [`ChromeEvent::FileChosen`].

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::thread;

use rfd::{AsyncFileDialog, FileHandle};
use tracing::warn;
use winit::event_loop::EventLoopProxy;
use winit::window::Window;

use super::ChromeEvent;
use crate::browser::UserEvent;

/// Which file dialog to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileDialog {
    /// Pick an existing file to open.
    Open,
    /// Pick where to save a file.
    Save,
}

/// What the user chose in a file dialog.
#[derive(Debug)]
pub(crate) struct FileChoice {
    /// The dialog that answered.
    pub(crate) dialog: FileDialog,
    /// The chosen path, or `None` when the user cancelled.
    pub(crate) path: Option<PathBuf>,
}

/// Shows `dialog` over `window` without blocking the event loop. Returns
/// `false` when the waiting thread could not start, so no answer will come.
pub(crate) fn show(window: &Window, dialog: FileDialog, proxy: EventLoopProxy<UserEvent>) -> bool {
    let builder = AsyncFileDialog::new().set_parent(window);
    let answer: Pin<Box<dyn Future<Output = Option<FileHandle>> + Send>> = match dialog {
        FileDialog::Open => Box::pin(builder.pick_file()),
        FileDialog::Save => Box::pin(builder.save_file()),
    };
    let spawned = thread::Builder::new()
        .name("file-dialog".to_owned())
        .spawn(move || {
            let path = pollster::block_on(answer).map(|file| file.path().to_path_buf());
            let choice = FileChoice { dialog, path };
            // Sending fails only once the event loop has exited, when nobody
            // is waiting for the answer any more.
            let _ = proxy.send_event(UserEvent::Chrome(ChromeEvent::FileChosen(choice)));
        });
    match spawned {
        Ok(_) => true,
        Err(err) => {
            warn!(%err, "could not start the file dialog thread");
            false
        }
    }
}
