//! A document's web fonts: its `@font-face` faces, the fetches for them, and
//! the [`WebFontLayer`] shaping reads (spec 0006, AC-10, AC-11).
//!
//! A face loads the first time styled text needs it: layout reports the
//! pending faces it met, and each starts fetching its first usable `src`.
//! While it loads, text in it is laid out in the fallback and hidden for up
//! to [`BLOCK_PERIOD`], then drawn in the fallback. A face that arrives
//! (even late) or finally fails changes the layer, and the page lays its
//! text out again.

use std::time::{Duration, Instant};

use css::{FontFaceRule, FontStyle};
use ipc::FetchId;
use text::{FaceState, FontData, Slant, WebFace, WebFontLayer};
use tracing::{debug, warn};

use crate::FetchIds;

/// How long text waits, hidden, for its web font (spec 0006, AC-11).
pub const BLOCK_PERIOD: Duration = Duration::from_secs(3);

/// The most `@font-face` rules one document may use; the rest are ignored
/// (spec 0006, AC-11).
pub const MAX_FONT_FACES: usize = 256;

/// The largest font body the page keeps (spec 0006, AC-11). The network
/// enforces the same cap; this guards the page on its own.
const MAX_FONT_BYTES: usize = 10 * 1024 * 1024;

/// What the page asks the browser for on behalf of its web fonts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontRequest {
    /// Fetch a font file.
    Fetch {
        /// The page's id for the fetch.
        fetch: FetchId,
        /// The absolute URL.
        url: String,
    },
    /// The face is gone or its body is over the cap: stop loading it.
    Cancel {
        /// The fetch to stop.
        fetch: FetchId,
    },
}

#[derive(Debug)]
enum State {
    /// No text needed it yet.
    Unloaded,
    /// Fetching `sources[next_source - 1]`. `since` is when the face's
    /// first source started, which its block period counts from.
    Loading {
        fetch: FetchId,
        since: Instant,
        /// The block period ran out: text draws in the fallback.
        expired: bool,
        bytes: Vec<u8>,
        ok: bool,
    },
    Loaded(FontData),
    Failed,
}

#[derive(Debug)]
struct Face {
    /// The id the layer gives shaping, unique within the set's life.
    id: u32,
    rule: FontFaceRule,
    /// The next `src` to try.
    next_source: usize,
    state: State,
}

/// One document's web fonts (spec 0006, Data model, `WebFontSet`).
#[derive(Debug, Default)]
pub struct WebFonts {
    faces: Vec<Face>,
    requests: Vec<FontRequest>,
    /// The layer for the faces' current states, rebuilt when `dirty`.
    layer: WebFontLayer,
    dirty: bool,
    /// The last layer's generation; it keeps counting across documents, as
    /// the page's one `FontContext` sees every layer.
    generation: u64,
    next_id: u32,
    /// The cap was hit and warned about for this document.
    capped: bool,
}

impl WebFonts {
    /// No faces yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The document went away: every face goes with it. Its fetches belong
    /// to the old navigation, so nothing is cancelled.
    pub fn document_replaced(&mut self) {
        self.faces.clear();
        self.requests.clear();
        self.capped = false;
        self.dirty = true;
    }

    /// Takes the fetches and cancels to send.
    pub fn take_requests(&mut self) -> Vec<FontRequest> {
        std::mem::take(&mut self.requests)
    }

    /// Whether `fetch` loads one of the faces.
    pub fn owns(&self, fetch: FetchId) -> bool {
        self.faces
            .iter()
            .any(|face| matches!(face.state, State::Loading { fetch: f, .. } if f == fetch))
    }

    /// The document's `@font-face` rules now, in cascade order. A face whose
    /// rule is unchanged keeps its state; a removed face stops loading.
    /// Returns whether the set changed, so text must be laid out again.
    pub fn set_rules(&mut self, rules: &[&FontFaceRule]) -> bool {
        let rules = if rules.len() > MAX_FONT_FACES {
            if !self.capped {
                self.capped = true;
                warn!(
                    rules = rules.len(),
                    cap = MAX_FONT_FACES,
                    "too many @font-face rules; the rest are ignored"
                );
            }
            &rules[..MAX_FONT_FACES]
        } else {
            rules
        };
        let same = rules.len() == self.faces.len()
            && rules
                .iter()
                .zip(&self.faces)
                .all(|(rule, face)| **rule == face.rule);
        if same {
            return false;
        }
        let mut old = std::mem::take(&mut self.faces);
        for rule in rules {
            let face = match old.iter().position(|face| face.rule == **rule) {
                Some(at) => old.swap_remove(at),
                None => {
                    let id = self.next_id;
                    self.next_id = self.next_id.wrapping_add(1);
                    Face {
                        id,
                        rule: (*rule).clone(),
                        next_source: 0,
                        state: State::Unloaded,
                    }
                }
            };
            self.faces.push(face);
        }
        for face in old {
            if let State::Loading { fetch, .. } = face.state {
                self.requests.push(FontRequest::Cancel { fetch });
            }
        }
        self.dirty = true;
        true
    }

    /// The layer shaping reads, rebuilt with a new generation after any
    /// change.
    pub fn layer(&mut self) -> &WebFontLayer {
        if self.dirty {
            self.dirty = false;
            self.generation = self.generation.wrapping_add(1).max(1);
            let mut layer = WebFontLayer::new(self.generation);
            for face in &self.faces {
                let state = match &face.state {
                    State::Unloaded | State::Loading { expired: false, .. } => FaceState::Pending,
                    State::Loading { expired: true, .. } | State::Failed => FaceState::Fallback,
                    State::Loaded(data) => FaceState::Ready(data.clone()),
                };
                layer.add(
                    &face.rule.family,
                    WebFace {
                        id: face.id,
                        weight: face.rule.weight,
                        slant: slant(face.rule.style),
                        state,
                    },
                );
            }
            self.layer = layer;
        }
        &self.layer
    }

    /// Layout met these pending faces: each one not loading yet starts on
    /// its first usable source (spec 0006, Font trigger).
    pub fn need(&mut self, ids: &[u32], fetch_ids: &mut FetchIds, now: Instant) {
        for &id in ids {
            let Some(at) = self.faces.iter().position(|face| face.id == id) else {
                continue;
            };
            if matches!(self.faces[at].state, State::Unloaded) {
                self.start_next(at, now, fetch_ids);
            }
        }
    }

    /// Starts the face's next usable source, keeping its block period, or
    /// fails it for good when none is left.
    fn start_next(&mut self, at: usize, now: Instant, fetch_ids: &mut FetchIds) {
        let Some(face) = self.faces.get_mut(at) else {
            return;
        };
        let (since, expired) = match face.state {
            State::Loading { since, expired, .. } => (since, expired),
            _ => (now, false),
        };
        while let Some(source) = face.rule.sources.get(face.next_source) {
            face.next_source += 1;
            if source.is_unsupported() {
                debug!(url = &*source.url, "font source skipped for its format");
                continue;
            }
            let fetch = fetch_ids.next_id();
            debug!(url = &*source.url, fetch = fetch.0, "font requested");
            self.requests.push(FontRequest::Fetch {
                fetch,
                url: source.url.to_string(),
            });
            face.state = State::Loading {
                fetch,
                since,
                expired,
                bytes: Vec::new(),
                ok: true,
            };
            return;
        }
        warn!(
            family = &*face.rule.family,
            "web font failed; using the fallback"
        );
        face.state = State::Failed;
        self.dirty = true;
    }

    /// The index of the face `fetch` loads.
    fn loading(&self, fetch: FetchId) -> Option<usize> {
        self.faces
            .iter()
            .position(|face| matches!(face.state, State::Loading { fetch: f, .. } if f == fetch))
    }

    /// A font's headers arrived. Only a 2xx answer is read.
    pub fn fetch_response(&mut self, fetch: FetchId, status: u16) {
        let Some(face) = self.loading(fetch).and_then(|at| self.faces.get_mut(at)) else {
            return;
        };
        if let State::Loading { ok, .. } = &mut face.state
            && !(200..300).contains(&status)
        {
            debug!(
                family = &*face.rule.family,
                status, "font source answered with an error"
            );
            *ok = false;
        }
    }

    /// Body bytes of a font. Past the cap the body is dropped and the fetch
    /// stopped; the source then fails when it ends.
    pub fn fetch_chunk(&mut self, fetch: FetchId, data: &[u8]) {
        let Some(face) = self.loading(fetch).and_then(|at| self.faces.get_mut(at)) else {
            return;
        };
        let State::Loading {
            bytes,
            ok: ok @ true,
            ..
        } = &mut face.state
        else {
            return;
        };
        if bytes.len() + data.len() > MAX_FONT_BYTES {
            *ok = false;
            *bytes = Vec::new();
            self.requests.push(FontRequest::Cancel { fetch });
            return;
        }
        bytes.extend_from_slice(data);
    }

    /// A font's body ended: a TrueType or OpenType file loads the face;
    /// anything else moves on to the next source. Returns whether the layer
    /// changed.
    pub fn fetch_done(&mut self, fetch: FetchId, fetch_ids: &mut FetchIds, now: Instant) -> bool {
        let Some(at) = self.loading(fetch) else {
            return false;
        };
        let Some(face) = self.faces.get_mut(at) else {
            return false;
        };
        let loaded = match &mut face.state {
            State::Loading {
                bytes, ok: true, ..
            } => FontData::parse(std::mem::take(bytes)),
            _ => None,
        };
        match loaded {
            Some(data) => {
                debug!(family = &*face.rule.family, "web font loaded");
                face.state = State::Loaded(data);
                self.dirty = true;
            }
            None => {
                debug!(
                    family = &*face.rule.family,
                    "font source unusable; trying the next"
                );
                self.start_next(at, now, fetch_ids);
            }
        }
        self.dirty
    }

    /// A font fetch failed: the next source, or the fallback for good.
    /// Returns whether the layer changed.
    pub fn fetch_failed(&mut self, fetch: FetchId, fetch_ids: &mut FetchIds, now: Instant) -> bool {
        let Some(at) = self.loading(fetch) else {
            return false;
        };
        self.start_next(at, now, fetch_ids);
        self.dirty
    }

    /// When the next block period ends, for the page's timer.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.faces
            .iter()
            .filter_map(|face| match face.state {
                State::Loading {
                    since,
                    expired: false,
                    ..
                } => Some(since + BLOCK_PERIOD),
                _ => None,
            })
            .min()
    }

    /// Ends every block period due by `now`: their text draws in the
    /// fallback from the next frame. Returns whether any ended.
    pub fn expire(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for face in &mut self.faces {
            if let State::Loading { since, expired, .. } = &mut face.state
                && !*expired
                && now >= *since + BLOCK_PERIOD
            {
                debug!(
                    family = &*face.rule.family,
                    "font block period over; drawing the fallback"
                );
                *expired = true;
                changed = true;
            }
        }
        self.dirty |= changed;
        changed
    }
}

fn slant(style: FontStyle) -> Slant {
    match style {
        FontStyle::Normal => Slant::Normal,
        FontStyle::Italic => Slant::Italic,
        FontStyle::Oblique => Slant::Oblique,
    }
}

#[cfg(test)]
mod tests {
    use css::{Origin, parse_stylesheet};

    use super::*;

    fn rules(css: &str) -> Vec<FontFaceRule> {
        parse_stylesheet(css, Origin::Author, Some("https://example.com/"))
            .font_faces()
            .to_vec()
    }

    fn fetches(fonts: &mut WebFonts) -> Vec<(FetchId, String)> {
        fonts
            .take_requests()
            .into_iter()
            .filter_map(|request| match request {
                FontRequest::Fetch { fetch, url } => Some((fetch, url)),
                FontRequest::Cancel { .. } => None,
            })
            .collect()
    }

    fn pending_ids(fonts: &mut WebFonts) -> Vec<u32> {
        let layer = fonts.layer().clone();
        let mut context = text::FontContext::with_fonts([include_bytes!(
            "../../../tools/conformance/fonts/Cantarell-Regular.ttf"
        )
        .to_vec()]);
        let families = [base::atoms::Atom::from("X")];
        let descriptor = text::FontDescriptor {
            families: &families,
            weight: 400,
            slant: Slant::Normal,
        };
        context.shape("a", &descriptor, base::Au::from_px(10.0), &layer);
        context.take_needed_faces()
    }

    #[test]
    fn a_face_loads_on_first_need_and_skips_woff_sources() {
        let mut fonts = WebFonts::new();
        let faces =
            rules("@font-face { font-family: X; src: url(x.woff2) format('woff2'), url(x.ttf) }");
        assert!(fonts.set_rules(&faces.iter().collect::<Vec<_>>()));
        assert!(
            !fonts.set_rules(&faces.iter().collect::<Vec<_>>()),
            "same rules"
        );
        assert!(
            fonts.take_requests().is_empty(),
            "nothing fetched before text needs it"
        );

        let mut ids = FetchIds::default();
        let now = Instant::now();
        let needed = pending_ids(&mut fonts);
        assert_eq!(needed.len(), 1);
        fonts.need(&needed, &mut ids, now);
        let fetched = fetches(&mut fonts);
        assert_eq!(fetched.len(), 1);
        assert_eq!(fetched[0].1, "https://example.com/x.ttf");
        fonts.need(&needed, &mut ids, now);
        assert!(fonts.take_requests().is_empty(), "already loading");
        assert_eq!(fonts.next_deadline(), Some(now + BLOCK_PERIOD));
    }

    #[test]
    fn a_bad_source_moves_on_then_falls_back_for_good() {
        // Spec 0006, AC-11: next `src`, then the fallback, with one warn.
        let mut fonts = WebFonts::new();
        let faces = rules("@font-face { font-family: X; src: url(a.ttf), url(b.ttf) }");
        fonts.set_rules(&faces.iter().collect::<Vec<_>>());
        let mut ids = FetchIds::default();
        let now = Instant::now();
        let needed = pending_ids(&mut fonts);
        fonts.need(&needed, &mut ids, now);
        let (first, _) = fetches(&mut fonts)[0].clone();

        // A WOFF2 file served without a hint fails the magic bytes check.
        fonts.fetch_response(first, 200);
        fonts.fetch_chunk(first, b"wOF2 not a ttf");
        fonts.fetch_done(first, &mut ids, now);
        let (second, url) = fetches(&mut fonts)[0].clone();
        assert_eq!(url, "https://example.com/b.ttf");
        assert_eq!(
            fonts.next_deadline(),
            Some(now + BLOCK_PERIOD),
            "the block period counts from the first source"
        );

        assert!(fonts.fetch_failed(second, &mut ids, now));
        assert!(fetches(&mut fonts).is_empty());
        assert_eq!(fonts.next_deadline(), None);
        assert!(
            pending_ids(&mut fonts).is_empty(),
            "failed faces are not pending"
        );
    }

    #[test]
    fn the_block_period_ends_once() {
        let mut fonts = WebFonts::new();
        let faces = rules("@font-face { font-family: X; src: url(a.ttf) }");
        fonts.set_rules(&faces.iter().collect::<Vec<_>>());
        let mut ids = FetchIds::default();
        let now = Instant::now();
        let needed = pending_ids(&mut fonts);
        fonts.need(&needed, &mut ids, now);
        assert!(!fonts.expire(now + Duration::from_secs(2)));
        assert!(fonts.expire(now + BLOCK_PERIOD));
        assert!(!fonts.expire(now + BLOCK_PERIOD * 2), "only once");
        assert_eq!(fonts.next_deadline(), None);
        assert!(
            pending_ids(&mut fonts).is_empty(),
            "drawn in the fallback now"
        );
    }

    #[test]
    fn over_the_cap_the_rest_are_ignored_and_removed_faces_stop_loading() {
        let mut css = String::new();
        for i in 0..MAX_FONT_FACES + 4 {
            css.push_str(&format!(
                "@font-face {{ font-family: F{i}; src: url(f.ttf) }}"
            ));
        }
        let many = rules(&css);
        let mut fonts = WebFonts::new();
        fonts.set_rules(&many.iter().collect::<Vec<_>>());
        assert_eq!(fonts.faces.len(), MAX_FONT_FACES);

        let one = rules("@font-face { font-family: X; src: url(a.ttf) }");
        let mut fonts = WebFonts::new();
        fonts.set_rules(&one.iter().collect::<Vec<_>>());
        let mut ids = FetchIds::default();
        let needed = pending_ids(&mut fonts);
        fonts.need(&needed, &mut ids, Instant::now());
        let (fetch, _) = fetches(&mut fonts)[0].clone();
        assert!(fonts.set_rules(&[]));
        assert_eq!(fonts.take_requests(), [FontRequest::Cancel { fetch }]);
    }
}
