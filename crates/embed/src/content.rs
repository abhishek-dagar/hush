//! The content process: runs pages. It never opens a window, a socket, or the GPU.

use std::collections::HashMap;
use std::process;
use std::rc::Rc;
use std::time::{Duration, Instant};

use display_list::PageId;
use ipc::{
    BrowserChannel, MemoryCategory, PageNavigationAction, PageNavigationOrigin, ToBrowser,
    ToContent,
};
use page::{Page, ReadyState};
use tracing::{error, info, info_span, trace, warn};

use crate::EmbedError;

/// How long the content process waits with no task, timer or event before
/// it runs one garbage collection pass (spec 0032, AC-9).
const IDLE_GC_AFTER: Duration = Duration::from_secs(5);

/// One page's perf marks in the content process (spec 0007, Value
/// sourcing). Only tracked in perf mode.
#[derive(Default)]
struct PagePerf {
    /// When this process handled the navigate command for the page.
    nav_started: Option<Instant>,
    /// Navigation start to `load`, in µs: recorded the first time after a
    /// task that `ready_state()` is `Complete`.
    nav_to_load_us: Option<u64>,
}

pub(crate) fn run(server_name: &str, perf: bool) -> Result<(), EmbedError> {
    let _span = info_span!("content", pid = process::id()).entered();

    let channel = BrowserChannel::connect(server_name)?;
    channel.send(ToBrowser::Ready { pid: process::id() })?;
    info!("connected to the browser process");
    if perf {
        // Full perf behaviour (marks, samples) arrives with the runtime
        // metrics (spec 0007, Build plan tasks 4 to 6).
        info!("content process in perf mode");
    }

    // Parsed once for the whole process and shared by every page (spec
    // 0004, Key invariants).
    let ua = page::ua_stylesheet();
    let mut pages: HashMap<PageId, Page> = HashMap::new();
    // Perf marks per page (spec 0007); only filled in perf mode.
    let mut perf_marks: HashMap<PageId, PagePerf> = HashMap::new();
    // The script engine's wall time, added up across every page (spec
    // 0007, Script timing); only present in perf mode.
    let script_timer = perf.then(std::sync::Arc::<crate::perf::ScriptTimer>::default);
    // The last task, timer or event, for the idle garbage collection (spec
    // 0032, AC-9).
    let mut idle_since = Instant::now();
    let mut idle_gc_done = false;
    loop {
        // Wait for the browser, but only until the next timer is due, or
        // until five idle seconds have passed (spec 0004, child 0006, Event
        // loop; spec 0032, AC-9).
        let next_timer = pages.values().filter_map(Page::next_timer).min();
        let idle_due = (!idle_gc_done).then_some(idle_since + IDLE_GC_AFTER);
        let wake = match (next_timer, idle_due) {
            (Some(timer), Some(idle)) => Some(timer.min(idle)),
            (timer, idle) => timer.or(idle),
        };
        let received = match wake {
            Some(wake) => channel.recv_timeout(wake.saturating_duration_since(Instant::now())),
            // Nothing due: wait for the browser (spec 0032, AC-9).
            None => channel.recv().map(Some),
        };
        let message = match received {
            Ok(Some(message)) => {
                // A message is a task: the idle count starts over.
                idle_since = Instant::now();
                idle_gc_done = false;
                message
            }
            Ok(None) => {
                let now = Instant::now();
                if next_timer.is_some_and(|due| due <= now) {
                    for page in pages.values_mut() {
                        page.run_timers(now);
                        after_task(&channel, page)?;
                    }
                    // A timer is a task too.
                    idle_since = now;
                    idle_gc_done = false;
                }
                if now >= idle_since + IDLE_GC_AFTER && !idle_gc_done {
                    // One pass per idle period (spec 0032, AC-9): the log
                    // line makes it observable, and the next one waits for a
                    // task to start the idle clock over.
                    info!("content idle: one garbage collection pass");
                    for page in pages.values_mut() {
                        page.collect_garbage();
                    }
                    idle_gc_done = true;
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
            ToContent::PerfSample => {
                if perf {
                    // The scenarios drive one page; answer with the marks
                    // that page has (spec 0007, IPC messages).
                    let marks = perf_marks
                        .values()
                        .find(|mark| mark.nav_to_load_us.is_some());
                    let nav_to_load_us = marks.and_then(|mark| mark.nav_to_load_us);
                    let script_us = script_timer.as_ref().map_or(0, |timer| timer.total_us());
                    channel.send(ToBrowser::PerfSample {
                        nav_to_load_us,
                        script_us,
                        footprint_bytes: crate::perf::footprint_bytes(),
                    })?;
                } else {
                    // A browser that never launched us with `--perf` has no
                    // business asking (spec 0007, IPC messages).
                    warn!("PerfSample ignored: not launched with --perf");
                }
                continue;
            }
            ToContent::ReportMemory => {
                let mut counters = Vec::new();
                if let Some(bytes) = crate::perf::footprint_bytes() {
                    counters.push((MemoryCategory::Footprint, bytes));
                }
                let mut js_heap = 0;
                let mut nodes = 0;
                for page in pages.values_mut() {
                    js_heap += page.script_heap_bytes().unwrap_or(0);
                    nodes += page.node_count();
                }
                counters.push((MemoryCategory::JsHeap, js_heap));
                counters.push((MemoryCategory::DomNodes, nodes));
                channel.send(ToBrowser::MemoryReport { counters })?;
                continue;
            }
            ToContent::CreatePage {
                page,
                viewport,
                scale,
            } => {
                // Empty until its first navigation commits (spec 0004).
                let mut fresh = Page::blank(page, viewport, scale, Rc::clone(&ua));
                if let Some(timer) = &script_timer {
                    // Every script call's wall time adds up for the perf
                    // report (spec 0007, Script timing).
                    let hook = std::sync::Arc::clone(timer);
                    fresh.set_script_wrap(Some(Box::new(move |engine| hook.wrap(engine))));
                }
                if perf {
                    perf_marks.insert(page, PagePerf::default());
                }
                pages.insert(page, fresh);
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
                // Navigation start (spec 0007, Value sourcing): when this
                // process handles the navigate command for the page. A new
                // navigation restarts the measurement.
                if let Some(mark) = perf_marks.get_mut(&page) {
                    mark.nav_started = Some(Instant::now());
                    mark.nav_to_load_us = None;
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
        // The `load` mark (spec 0007, Value sourcing): the first time after
        // a task that the page's ready state is `Complete`.
        if perf
            && let Some(mark) = perf_marks.get_mut(&page.id())
            && mark.nav_to_load_us.is_none()
            && let Some(started) = mark.nav_started
            && page.ready_state() == ReadyState::Complete
        {
            mark.nav_to_load_us = Some(started.elapsed().as_micros() as u64);
        }
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
