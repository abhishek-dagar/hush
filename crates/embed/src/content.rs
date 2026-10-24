//! The content process: runs pages. It never opens a window, a socket, or the GPU.

use std::collections::HashMap;
use std::process;
use std::rc::Rc;
use std::time::Instant;

use display_list::PageId;
use ipc::{BrowserChannel, PageNavigationAction, PageNavigationOrigin, ToBrowser, ToContent};
use page::Page;
use tracing::{error, info, info_span, trace, warn};

use crate::EmbedError;

pub(crate) fn run(server_name: &str) -> Result<(), EmbedError> {
    let _span = info_span!("content", pid = process::id()).entered();

    let channel = BrowserChannel::connect(server_name)?;
    channel.send(ToBrowser::Ready { pid: process::id() })?;
    info!("connected to the browser process");

    // Parsed once for the whole process and shared by every page (spec
    // 0004, Key invariants).
    let ua = page::ua_stylesheet();
    let mut pages: HashMap<PageId, Page> = HashMap::new();
    loop {
        // Wait for the browser, but only until the next timer is due
        // (spec 0004, child 0006, Event loop).
        let next_timer = pages.values().filter_map(Page::next_timer).min();
        let received = match next_timer {
            None => channel.recv().map(Some),
            Some(due) => channel.recv_timeout(due.saturating_duration_since(Instant::now())),
        };
        let message = match received {
            Ok(Some(message)) => message,
            Ok(None) => {
                let now = Instant::now();
                for page in pages.values_mut() {
                    page.run_timers(now);
                    after_task(&channel, page)?;
                }
                continue;
            }
            // The browser is gone (crashed or killed), so nobody is left to serve.
            Err(err) => {
                warn!(%err, "browser channel closed, exiting");
                return Ok(());
            }
        };
        let target = match message {
            ToContent::Shutdown => {
                info!("shutting down");
                return Ok(());
            }
            ToContent::CreatePage {
                page,
                viewport,
                scale,
            } => {
                // Empty until its first navigation commits (spec 0004).
                pages.insert(page, Page::blank(page, viewport, scale, Rc::clone(&ua)));
                page
            }
            ToContent::Resize {
                page,
                viewport,
                scale,
            } => {
                if let Some(target) = pages.get_mut(&page) {
                    target.resize(viewport, scale);
                }
                page
            }
            ToContent::BeginFrame { page, frame_time } => {
                if let Some(target) = pages.get_mut(&page) {
                    send_frame(&channel, target, frame_time)?;
                }
                page
            }
            ToContent::ScrollChanged {
                page,
                scroll_id,
                x,
                y,
            } => {
                if let Some(target) = pages.get_mut(&page) {
                    target.scroll_changed(scroll_id, x, y);
                }
                page
            }
            ToContent::Input { page, event } => {
                if let Some(target) = pages.get_mut(&page) {
                    let history_key = history_key(&event);
                    let outcome = target.input(event);
                    // Cmd+Left and Cmd+Right go back and forward when no
                    // text field used them (spec 0008, AC-29).
                    if let Some((key, delta)) = history_key
                        && outcome.unused_key.as_ref() == Some(&key)
                    {
                        channel.send(ToBrowser::HistoryGo {
                            page,
                            navigation: target.navigation(),
                            delta,
                        })?;
                    }
                    send_outcome(&channel, target, outcome)?;
                }
                page
            }
            ToContent::ClosePage { page } => {
                if pages.remove(&page).is_none() {
                    warn!(?page, "close for an unknown page ignored");
                }
                continue;
            }
            ToContent::Navigate { page, navigation } => {
                if let Some(target) = pages.get_mut(&page) {
                    target.navigate(navigation);
                }
                page
            }
            ToContent::CommitNavigation {
                page,
                navigation,
                url,
                charset,
                kind,
                restore_scroll,
                history,
                ..
            } => {
                if let Some(target) = pages.get_mut(&page)
                    && target.commit_navigation(navigation, &url, charset, &kind)
                {
                    target.restore_scroll(navigation, restore_scroll);
                    target.set_history(history);
                }
                page
            }
            ToContent::HistoryChanged {
                page,
                navigation,
                history,
            } => {
                if let Some(target) = pages.get_mut(&page)
                    && target.navigation() == navigation
                {
                    target.set_history(history);
                }
                page
            }
            ToContent::Stop { page, navigation } => {
                if let Some(target) = pages.get_mut(&page) {
                    target.stop(navigation);
                }
                page
            }
            ToContent::SameDocument {
                page,
                navigation,
                url,
                scroll,
                history,
            } => {
                if let Some(target) = pages.get_mut(&page) {
                    target.same_document(navigation, &url, scroll, history);
                }
                page
            }
            ToContent::FetchResponse {
                page,
                navigation,
                fetch,
                status,
                headers,
                final_url,
            } => {
                if let Some(target) = pages.get_mut(&page) {
                    target.fetch_response(navigation, fetch, status, &headers, &final_url);
                }
                page
            }
            ToContent::FetchChunk {
                page,
                navigation,
                fetch,
                data,
            } => {
                if let Some(target) = pages.get_mut(&page) {
                    target.fetch_chunk(navigation, fetch, &data);
                }
                page
            }
            ToContent::FetchDone {
                page,
                navigation,
                fetch,
            } => {
                if let Some(target) = pages.get_mut(&page) {
                    target.fetch_done(navigation, fetch);
                }
                page
            }
            ToContent::FetchFailed {
                page,
                navigation,
                fetch,
                reason,
            } => {
                if let Some(target) = pages.get_mut(&page) {
                    target.fetch_failed(navigation, fetch, &reason);
                }
                page
            }
        };

        let Some(page) = pages.get_mut(&target) else {
            warn!(page = ?target, "message for an unknown page ignored");
            continue;
        };
        // Each message is one task: act on iframe changes, run what it
        // unblocked, sweep, and ask for what it found to load.
        page.end_task();
        after_task(&channel, page)?;
    }
}

/// On macOS, a Cmd+Left or Cmd+Right press, with the history move it means
/// when no text field takes it (spec 0008, AC-29).
fn history_key(event: &ipc::InputEvent) -> Option<(ipc::Key, i32)> {
    let ipc::InputEvent::Key {
        key,
        modifiers,
        pressed: true,
        ..
    } = event
    else {
        return None;
    };
    let command = modifiers.meta && !modifiers.ctrl && !modifiers.alt && !modifiers.shift;
    if !cfg!(target_os = "macos") || !command {
        return None;
    }
    match key {
        ipc::Key::ArrowLeft => Some((key.clone(), -1)),
        ipc::Key::ArrowRight => Some((key.clone(), 1)),
        _ => None,
    }
}

/// Tells the browser what an input event led to (spec 0006, AC-13,
/// AC-15, AC-18): focus leaving the page, a new pointer shape, and a link
/// to follow (spec 0008, AC-17). Other activations and unused keys have
/// no job until feature 14.
fn send_outcome(
    channel: &BrowserChannel,
    target: &Page,
    outcome: page::InputOutcome,
) -> Result<(), EmbedError> {
    let page = target.id();
    if let Some(direction) = outcome.focus_left {
        channel.send(ToBrowser::FocusLeft { page, direction })?;
    }
    if let Some(cursor) = outcome.cursor {
        channel.send(ToBrowser::SetCursor { page, cursor })?;
    }
    // The answer to the browser's `Copy` or `Cut` (spec 0008, AC-15).
    if let Some((request, mut text)) = outcome.copied {
        if text.len() > ipc::MAX_CLIPBOARD_WRITE_BYTES {
            let mut cut = ipc::MAX_CLIPBOARD_WRITE_BYTES;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
        }
        channel.send(ToBrowser::ClipboardWrite {
            page,
            request,
            text,
        })?;
    }
    if let Some(node) = outcome.activated {
        match target.link_url(node) {
            Some(url) => channel.send(ToBrowser::NavigateRequest {
                page,
                navigation: target.navigation(),
                url,
                origin: PageNavigationOrigin::Link,
                action: PageNavigationAction::Push,
            })?,
            None => trace!(?node, "activation dropped until DOM events"),
        }
    }
    // A scroll key the page did not use: the browser scrolls the frame it
    // names, or the viewport when the target is `None` (spec 0009, child
    // 0002, AC-9).
    if let Some((key, target)) = outcome.scroll {
        channel.send(ToBrowser::ScrollKey { page, key, target })?;
    }
    if let Some(key) = outcome.unused_key {
        trace!(?key, "unused key dropped until the keyboard model");
    }
    Ok(())
}

/// Sends what a task left for the browser: the fetches it asked for, the
/// title and favicon if they changed, a new pointer shape, and
/// `NeedsFrame` if the page has work to draw.
fn after_task(channel: &BrowserChannel, page: &mut Page) -> Result<(), EmbedError> {
    for message in page.take_fetch_messages() {
        channel.send(message)?;
    }
    for message in page.take_head_messages() {
        channel.send(message)?;
    }
    if let Some(message) = page.take_load_message() {
        channel.send(message)?;
    }
    // What scripts asked to load (spec 0008, AC-27); the browser decides.
    for message in page.take_navigation_messages() {
        channel.send(message)?;
    }
    // A frame's new hover or style can change the shape (AC-18).
    if let Some(cursor) = page.take_cursor_change() {
        channel.send(ToBrowser::SetCursor {
            page: page.id(),
            cursor,
        })?;
    }
    send_link_hover(channel, page)?;
    if page.take_frame_request() {
        channel.send(ToBrowser::NeedsFrame { page: page.id() })?;
    }
    Ok(())
}

/// Runs the rendering update and answers `BeginFrame`: one list if
/// something changed, `FrameSkipped` otherwise (AC-12). New font and image
/// keys go out before the list that uses them, freed ones after it.
fn send_frame(
    channel: &BrowserChannel,
    page: &mut Page,
    frame_time: f64,
) -> Result<(), EmbedError> {
    let answer = match page.begin_frame(frame_time) {
        None => ToBrowser::FrameSkipped { page: page.id() },
        Some(list) => match page::encode(&list) {
            Ok(data) => ToBrowser::DisplayList {
                page: page.id(),
                navigation: page.navigation(),
                epoch: list.epoch,
                data,
            },
            Err(err) => {
                // Keep the last good frame on screen and try again next frame.
                error!(%err, "dropping a frame");
                page.invalidate();
                ToBrowser::FrameSkipped { page: page.id() }
            }
        },
    };
    let (adds, deletes) = page.take_resource_messages();
    for message in adds.into_iter().chain([answer]).chain(deletes) {
        channel.send(message)?;
    }
    // The style pass may have found background images to load.
    for message in page.take_fetch_messages() {
        channel.send(message)?;
    }
    // The frame's hit test may have moved the pointer onto or off a link.
    send_link_hover(channel, page)?;
    if let Some(cursor) = page.take_cursor_change() {
        channel.send(ToBrowser::SetCursor {
            page: page.id(),
            cursor,
        })?;
    }
    Ok(())
}

/// Tells the browser the link the status bubble shows, when it changed
/// (spec 0008, AC-19).
fn send_link_hover(channel: &BrowserChannel, page: &mut Page) -> Result<(), EmbedError> {
    if let Some(url) = page.take_link_hover() {
        channel.send(ToBrowser::LinkHover {
            page: page.id(),
            url,
        })?;
    }
    Ok(())
}
