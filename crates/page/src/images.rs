//! A document's images (spec 0004, child 0005, Images, AC-29).
//!
//! Each `<img src>` URL and each `background-image` URL in use is one
//! entry, fetched through the browser like every other load and decoded on
//! the page thread when its body ends (slice 1 has no decode thread). A
//! decoded picture is registered with the resource table, so its
//! `AddImage` goes out before the first list that draws it; each `<img>`
//! then takes its size and key, and paint draws backgrounds from
//! [`Images::pictures`]. An entry no longer used releases its key, which
//! the table frees one epoch after the last list that drew it.

use std::collections::HashMap;
use std::rc::Rc;

use base::atoms::{LocalName, QualName};
use display_list::{ImageFormat, ImageKey, SizeF};
use dom::{Document, ImageState, NodeId};
use ipc::FetchId;
use paint::Picture;
use tracing::{debug, warn};

use crate::{FetchIds, ResourceTable};

/// What the page asks the browser for on behalf of its images.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageRequest {
    /// Fetch an image.
    Fetch {
        /// The page's id for the fetch.
        fetch: FetchId,
        /// The absolute URL.
        url: String,
    },
    /// Nothing uses the image any more: stop loading it.
    Cancel {
        /// The fetch to stop.
        fetch: FetchId,
    },
}

#[derive(Debug)]
enum State {
    /// The body so far, and whether the response was a success.
    Loading {
        bytes: Vec<u8>,
        ok: bool,
    },
    Ready {
        key: ImageKey,
        width: u32,
        height: u32,
    },
    Failed,
}

#[derive(Debug)]
struct Entry {
    url: String,
    fetch: FetchId,
    state: State,
}

/// The committed document's images and the fetches for them.
#[derive(Debug, Default)]
pub struct Images {
    entries: Vec<Entry>,
    /// Each connected `<img>`, in tree order, with the absolute URL it
    /// shows (`None` without a usable `src`).
    elements: Vec<(NodeId, Option<String>)>,
    /// The `background-image` URLs the boxes use.
    backgrounds: Vec<String>,
    requests: Vec<ImageRequest>,
}

impl Images {
    /// No images yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes the fetches and cancels to send.
    pub fn take_requests(&mut self) -> Vec<ImageRequest> {
        std::mem::take(&mut self.requests)
    }

    /// Whether an image is still loading, which holds `load` (AC-33).
    pub fn loading(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| matches!(entry.state, State::Loading { .. }))
    }

    /// The image page's picture comes from the navigation's own body
    /// (spec 0035, AC-6): `url`'s entry starts loading under
    /// [`FetchId::DOCUMENT`], so the template's `<img src>` finds it and
    /// nothing is fetched. The entry stays until the document goes.
    pub fn adopt_document(&mut self, url: &str) {
        let url = url::Url::parse(url).map_or_else(|_| url.to_owned(), String::from);
        self.entries.push(Entry {
            url,
            fetch: FetchId::DOCUMENT,
            state: State::Loading {
                bytes: Vec::new(),
                ok: true,
            },
        });
    }

    /// How many body bytes `fetch` received so far, while it loads.
    pub fn received(&self, fetch: FetchId) -> Option<usize> {
        self.entries.iter().find_map(|entry| match &entry.state {
            State::Loading { bytes, .. } if entry.fetch == fetch => Some(bytes.len()),
            _ => None,
        })
    }

    /// The decoded size of `fetch`'s picture, once it is shown.
    pub fn natural_size(&self, fetch: FetchId) -> Option<(u32, u32)> {
        self.entries.iter().find_map(|entry| match entry.state {
            State::Ready { width, height, .. } if entry.fetch == fetch => Some((width, height)),
            _ => None,
        })
    }

    /// Whether `fetch` loads one of the images.
    pub fn owns(&self, fetch: FetchId) -> bool {
        self.entries.iter().any(|entry| entry.fetch == fetch)
    }

    /// Finds the document's `<img>` elements again: new URLs start loading,
    /// unused ones stop, and every element shows its image's state.
    pub fn refresh_elements(
        &mut self,
        doc: &mut Document,
        ids: &mut FetchIds,
        resources: &mut ResourceTable,
    ) {
        let base = url::Url::parse(doc.url()).ok();
        let src = QualName::attribute(LocalName::from("src"));
        // In tree order.
        let mut found = Vec::new();
        let mut stack = vec![doc.root()];
        while let Some(node) = stack.pop() {
            if doc.image_state(node).is_some() {
                found.push(node);
            }
            let children: Vec<NodeId> = doc.children(node).collect();
            stack.extend(children.into_iter().rev());
        }
        self.elements = found
            .into_iter()
            .map(|node| {
                let url = doc
                    .node(node)
                    .and_then(|n| n.as_element())
                    .and_then(|element| element.attribute(&src))
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .and_then(|value| base.as_ref()?.join(value).ok())
                    .map(String::from);
                (node, url)
            })
            .collect();
        self.sync(ids, resources);
        self.apply(doc);
    }

    /// The `background-image` URLs the boxes use now. Returns whether the
    /// set changed.
    pub fn set_backgrounds(
        &mut self,
        mut urls: Vec<String>,
        ids: &mut FetchIds,
        resources: &mut ResourceTable,
    ) -> bool {
        urls.sort_unstable();
        urls.dedup();
        if urls == self.backgrounds {
            return false;
        }
        self.backgrounds = urls;
        self.sync(ids, resources);
        true
    }

    /// Starts loading every URL in use that has no entry yet, and drops the
    /// entries nothing uses any more.
    fn sync(&mut self, ids: &mut FetchIds, resources: &mut ResourceTable) {
        let wanted: Vec<&String> = self
            .elements
            .iter()
            .filter_map(|(_, url)| url.as_ref())
            .chain(&self.backgrounds)
            .collect();
        let requests = &mut self.requests;
        self.entries.retain(|entry| {
            // The adopted document body is never fetched, so never dropped.
            if entry.fetch == FetchId::DOCUMENT || wanted.contains(&&entry.url) {
                return true;
            }
            match entry.state {
                State::Loading { .. } => requests.push(ImageRequest::Cancel { fetch: entry.fetch }),
                State::Ready { key, .. } => resources.release_image(key),
                State::Failed => {}
            }
            false
        });
        for url in wanted {
            if self.entries.iter().any(|entry| entry.url == *url) {
                continue;
            }
            let fetch = ids.next_id();
            debug!(url, fetch = fetch.0, "image requested");
            self.requests.push(ImageRequest::Fetch {
                fetch,
                url: url.clone(),
            });
            self.entries.push(Entry {
                url: url.clone(),
                fetch,
                state: State::Loading {
                    bytes: Vec::new(),
                    ok: true,
                },
            });
        }
    }

    /// Gives every `<img>` its image's state.
    fn apply(&self, doc: &mut Document) {
        for (node, url) in &self.elements {
            let state = match url {
                None => ImageState::Failed,
                Some(url) => match self.entries.iter().find(|entry| entry.url == *url) {
                    Some(Entry {
                        state: State::Ready { key, width, height },
                        ..
                    }) => ImageState::Ready {
                        width: *width,
                        height: *height,
                        key: key.0,
                    },
                    Some(Entry {
                        state: State::Failed,
                        ..
                    }) => ImageState::Failed,
                    _ => ImageState::Pending,
                },
            };
            // An element the script already freed has nothing to show.
            let _ = doc.set_image_state(*node, state);
        }
    }

    fn entry_mut(&mut self, fetch: FetchId) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|entry| entry.fetch == fetch)
    }

    /// An image's headers arrived. Only a 2xx answer is decoded.
    pub fn fetch_response(&mut self, fetch: FetchId, status: u16) {
        if let Some(Entry {
            url,
            state: State::Loading { ok, .. },
            ..
        }) = self.entry_mut(fetch)
            && !(200..300).contains(&status)
        {
            warn!(url, status, "image skipped");
            *ok = false;
        }
    }

    /// Body bytes of an image.
    pub fn fetch_chunk(&mut self, fetch: FetchId, data: &[u8]) {
        if let Some(Entry {
            state: State::Loading { bytes, ok: true },
            ..
        }) = self.entry_mut(fetch)
        {
            bytes.extend_from_slice(data);
        }
    }

    /// An image's body ended: decode it, register it, and show it. Format
    /// comes from the magic bytes, never the `Content-Type`.
    pub fn fetch_done(
        &mut self,
        doc: &mut Document,
        fetch: FetchId,
        resources: &mut ResourceTable,
    ) {
        let Some(entry) = self.entry_mut(fetch) else {
            return;
        };
        let (bytes, ok) = match std::mem::replace(&mut entry.state, State::Failed) {
            State::Loading { bytes, ok } => (bytes, ok),
            state => {
                entry.state = state;
                return;
            }
        };
        let decoded = if ok {
            image::decode(&bytes, &image::Limits::default()).map_err(|err| err.to_string())
        } else {
            Err("not a success response".into())
        };
        entry.state = match decoded {
            Ok(picture) => {
                debug!(
                    url = entry.url,
                    picture.width, picture.height, "image decoded"
                );
                let size = (picture.width, picture.height);
                let key = resources.add_image(size, ImageFormat::Rgba8, picture.pixels, bytes);
                State::Ready {
                    key,
                    width: picture.width,
                    height: picture.height,
                }
            }
            Err(err) => {
                warn!(url = entry.url, %err, "image failed");
                State::Failed
            }
        };
        self.apply(doc);
    }

    /// An image failed to load: its elements show their `alt` text.
    pub fn fetch_failed(&mut self, doc: &mut Document, fetch: FetchId) {
        if let Some(entry) = self.entry_mut(fetch) {
            entry.state = State::Failed;
            self.apply(doc);
        }
    }

    /// The decoded pictures by URL, for paint's backgrounds.
    pub fn pictures(&self) -> HashMap<Rc<str>, Picture> {
        self.entries
            .iter()
            .filter_map(|entry| match entry.state {
                State::Ready { key, width, height } => Some((
                    Rc::from(entry.url.as_str()),
                    Picture {
                        key,
                        size: SizeF {
                            width: width as f32,
                            height: height as f32,
                        },
                    },
                )),
                _ => None,
            })
            .collect()
    }

    /// The document is going away: every picture's key is released.
    pub fn release_all(self, resources: &mut ResourceTable) {
        for entry in self.entries {
            if let State::Ready { key, .. } = entry.state {
                resources.release_image(key);
            }
        }
    }
}
