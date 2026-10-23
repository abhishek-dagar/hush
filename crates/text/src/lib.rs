//! Text: finds system fonts, reads their metrics and shapes runs of text
//! into positioned glyphs (spec 0004, child 0004, The `text` crate).
//!
//! Runs in the content process, on the page thread. Fonts are found with
//! `fontique`, measured with `skrifa` and shaped with `harfrust`. Each face
//! gets a [`FontKey`] the first time it shapes something, and its bytes are
//! queued for `AddFont`, so the render side gets every font before the
//! first list that names it (spec 0004, AC-26).
//!
//! A run of text is itemized by font: each character takes the first font in
//! its `font-family` list, then the generic fallback, that has a glyph for it
//! (AC-25). There is no synthetic bold or italic: a missing face falls back
//! to the nearest weight and style `fontique` offers, and a variable face is
//! set to the requested weight through its variation axes.
//!
//! A document's `@font-face` faces arrive as a [`WebFontLayer`] the caller
//! passes to every [`FontContext::shape`] and [`FontContext::metrics`] call,
//! never registered in the shared collection, so one document's family
//! names never reach another's text (spec 0006, Font scope).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::ops::{Deref, Range};
use std::rc::Rc;

use base::Au;
use base::atoms::Atom;
pub use display_list::FontKey;
use fontique::{
    Attributes, Blob, CharmapIndex, Collection, CollectionOptions, FontStyle as QueryStyle,
    FontWeight, FontWidth, GenericFamily, QueryFamily, QueryFont, SourceCache,
};
use harfrust::{Direction, ShapeOptions, ShaperData, ShaperInstance, UnicodeBuffer};
use skrifa::MetadataProvider;
use skrifa::instance::{LocationRef, Size};
use skrifa::raw::types::Tag;
use tracing::{debug, warn};

/// A face's vertical metrics, in CSS px at the size a run was shaped at.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LineMetrics {
    /// From the baseline up to the top of the em box's line area.
    pub ascent: Au,
    /// From the baseline down to the bottom, as a positive length.
    pub descent: Au,
    /// The extra space the font asks for between lines.
    pub line_gap: Au,
    /// From the baseline up to the top of an underline; negative is below
    /// the baseline, where underlines go.
    pub underline_offset: Au,
    /// How thick an underline is.
    pub underline_thickness: Au,
}

/// One shaped glyph. Lengths are CSS px at the run's size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapedGlyph {
    /// The glyph id in the run's font.
    pub id: u32,
    /// The byte offset in the shaped text (the whole text passed to
    /// [`FontContext::shape`], not just this run) of the first character
    /// this glyph draws (its cluster).
    pub cluster: u32,
    /// How far the pen moves after the glyph.
    pub advance: Au,
    /// A horizontal nudge that does not move the pen.
    pub x_offset: Au,
    /// A vertical nudge, positive up, that does not move the pen.
    pub y_offset: Au,
}

/// A run of text shaped in one font at one size.
#[derive(Debug, Clone, PartialEq)]
pub struct ShapedRun {
    /// The font, as display lists name it.
    pub font: FontKey,
    /// The font size.
    pub size: Au,
    /// The bytes of the shaped text this run covers.
    pub range: Range<usize>,
    /// The glyphs in visual order (left to right in slice 1).
    pub glyphs: Vec<ShapedGlyph>,
    /// The font's metrics at this size.
    pub metrics: LineMetrics,
    /// Laid out in a fallback while a web face earlier in the family list
    /// is still loading inside its block period: paint skips it (spec 0006,
    /// AC-11).
    pub hidden_font: bool,
}

impl ShapedRun {
    /// The run's total advance.
    pub fn width(&self) -> Au {
        self.glyphs
            .iter()
            .fold(Au::ZERO, |width, glyph| width + glyph.advance)
    }
}

/// `font-style`, as far as picking a face goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Slant {
    /// Upright.
    #[default]
    Normal,
    /// `italic`.
    Italic,
    /// `oblique`.
    Oblique,
}

/// Which fonts a run asks for: the computed `font-family`, `font-weight`
/// and `font-style`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontDescriptor<'a> {
    /// Family names in order of preference. The generic families are their
    /// lowercase keywords (`serif`, `sans-serif`, `monospace`, `cursive`,
    /// `fantasy`, `system-ui`), as `css` keeps them.
    pub families: &'a [Atom],
    /// 1 to 1000.
    pub weight: u16,
    /// Upright, italic or oblique.
    pub slant: Slant,
}

/// A font file's bytes, shared with the font collection without copying.
#[derive(Debug, Clone)]
pub struct FontData(Blob<u8>);

impl FontData {
    /// A downloaded font file, if it is one the engine loads: TrueType or
    /// OpenType by its first four bytes, and readable by `skrifa` (spec
    /// 0006, State transitions). WOFF, WOFF2 and anything else give `None`.
    pub fn parse(bytes: Vec<u8>) -> Option<Self> {
        let magic = bytes.get(..4)?;
        if !matches!(magic, [0, 1, 0, 0] | b"OTTO" | b"true") {
            return None;
        }
        skrifa::FontRef::new(&bytes).ok()?;
        Some(Self(Blob::from(bytes)))
    }
}

/// A web face's load state as shaping sees it (spec 0006, State
/// transitions).
#[derive(Debug, Clone)]
pub enum FaceState {
    /// Not loaded yet, inside its block period: text that would use it is
    /// laid out in the next font and not drawn.
    Pending,
    /// Loaded.
    Ready(FontData),
    /// Failed, or past its block period: the family is skipped.
    Fallback,
}

/// One `@font-face` face in a [`WebFontLayer`].
#[derive(Debug, Clone)]
pub struct WebFace {
    /// The caller's id for the face, reported back by
    /// [`FontContext::take_needed_faces`] when a pending face is needed.
    pub id: u32,
    /// The weight range it covers, low to high.
    pub weight: (u16, u16),
    /// Its style.
    pub slant: Slant,
    /// Its load state.
    pub state: FaceState,
}

/// One document's `@font-face` faces by family (spec 0006, Data model).
/// Build a new one, with a new generation, whenever a face or its state
/// changes; the context drops what it resolved against an older one.
#[derive(Debug, Clone, Default)]
pub struct WebFontLayer {
    /// 0 is the empty layer.
    generation: u64,
    /// By family name, ASCII lowercased.
    families: HashMap<String, Vec<WebFace>>,
}

impl WebFontLayer {
    /// An empty layer. `generation` must differ from the last layer's
    /// passed to the same context, and not be 0.
    pub fn new(generation: u64) -> Self {
        Self {
            generation,
            families: HashMap::new(),
        }
    }

    /// Adds a face to `family`. A later face with the same weight and style
    /// replaces an earlier one, as a later rule wins.
    pub fn add(&mut self, family: &str, face: WebFace) {
        let faces = self
            .families
            .entry(family.to_ascii_lowercase())
            .or_default();
        faces.retain(|f| (f.weight, f.slant) != (face.weight, face.slant));
        faces.push(face);
    }

    /// Whether it holds no faces.
    pub fn is_empty(&self) -> bool {
        self.families.is_empty()
    }

    fn faces(&self, family: &str) -> Option<&[WebFace]> {
        if self.families.is_empty() {
            return None;
        }
        self.families
            .get(&family.to_ascii_lowercase())
            .map(Vec::as_slice)
    }
}

/// The face of a family `weight` and `slant` pick, by the CSS Fonts 4 font
/// matching rules: style first, then weight (spec 0006, Value sourcing,
/// Face choice). On a tie the later face wins.
fn match_face(faces: &[WebFace], weight: u16, slant: Slant) -> Option<&WebFace> {
    let order = match slant {
        Slant::Italic => [Slant::Italic, Slant::Oblique, Slant::Normal],
        Slant::Oblique => [Slant::Oblique, Slant::Italic, Slant::Normal],
        Slant::Normal => [Slant::Normal, Slant::Oblique, Slant::Italic],
    };
    let style = order
        .into_iter()
        .find(|&s| faces.iter().any(|face| face.slant == s))?;
    let rank = |face: &WebFace| -> (u8, u16) {
        let (low, high) = face.weight;
        if (low..=high).contains(&weight) {
            return (0, 0);
        }
        let above = low > weight;
        let distance = if above { low - weight } else { weight - high };
        let group = if (400..=500).contains(&weight) {
            match above {
                true if low <= 500 => 1,
                false => 2,
                true => 3,
            }
        } else if weight < 400 {
            if above { 2 } else { 1 }
        } else if above {
            1
        } else {
            2
        };
        (group, distance)
    };
    faces
        .iter()
        .filter(|face| face.slant == style)
        .fold(None, |best: Option<&WebFace>, face| match best {
            Some(b) if rank(b) < rank(face) => Some(b),
            _ => Some(face),
        })
}

impl Deref for FontData {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

/// A face that got its key since the last [`FontContext::take_new_fonts`]:
/// send it with `AddFont` before the next list.
#[derive(Debug, Clone)]
pub struct NewFont {
    /// The key lists name it by.
    pub key: FontKey,
    /// The font file.
    pub data: FontData,
    /// The face's index in a collection file, 0 otherwise.
    pub index: u32,
    /// Variation axis settings (tag, user space value) for a variable face,
    /// such as `wght` for a bold instance. Empty for a static face.
    pub variations: Vec<(u32, f32)>,
}

/// A face in use: its bytes, key, variation instance, metrics in font
/// units, and the shaper's cached tables.
struct FontEntry {
    key: FontKey,
    data: Blob<u8>,
    index: u32,
    /// `fontique`'s cached cmap location; `None` for a web face, which looks
    /// its cmap up through `skrifa`.
    charmap: Option<CharmapIndex>,
    /// Whether it came from a document's [`WebFontLayer`].
    web: bool,
    variations: Vec<(Tag, f32)>,
    units_per_em: f32,
    ascent: f32,
    descent: f32,
    line_gap: f32,
    /// The `post` table's underline offset (to its top) and thickness.
    underline: Option<(f32, f32)>,
    shaper: ShaperData,
    instance: ShaperInstance,
}

impl FontEntry {
    fn has_char(&self, c: char) -> bool {
        let glyph = match &self.charmap {
            Some(charmap) => charmap
                .charmap(self.data.as_ref())
                .and_then(|map| map.map(c)),
            None => skrifa::FontRef::from_index(self.data.as_ref(), self.index)
                .ok()
                .and_then(|font| font.charmap().map(c))
                .map(|glyph| glyph.to_u32()),
        };
        glyph.is_some_and(|glyph| glyph != 0)
    }

    fn metrics(&self, size: Au) -> LineMetrics {
        let px = size.to_f32_px();
        let scale = px / self.units_per_em;
        let scaled = |units: f32| Au::from_px(units * scale);
        let descent = scaled(self.descent);
        let (underline_offset, underline_thickness) = match self.underline {
            Some((offset, thickness)) if thickness > 0.0 => (scaled(offset), scaled(thickness)),
            // No `post` table: 1/14 em thick, centered halfway down the
            // descent (spec 0004, Value sourcing: Paint).
            _ => {
                let thickness = Au::from_px(px / 14.0);
                (
                    -(descent.scale_by(0.5)) + thickness.scale_by(0.5),
                    thickness,
                )
            }
        };
        LineMetrics {
            ascent: scaled(self.ascent),
            descent,
            line_gap: scaled(self.line_gap),
            underline_offset,
            underline_thickness,
        }
    }
}

/// The system font collection, opened on first use because scanning the
/// system's fonts takes a while.
struct System {
    collection: Collection,
    source_cache: SourceCache,
}

/// What a family list resolved to: the family names, weight and slant.
type ResolveKey = (Vec<Atom>, u16, Slant);

/// One place in a resolved family list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// A font, by index in `FontContext::fonts`.
    Font(usize),
    /// A web face still loading inside its block period: it is taken to
    /// cover every character, so text past it is hidden.
    Pending,
}

/// What a family resolves to before its fonts get entries.
enum Pick {
    System(QueryFont),
    Web(FontData, Vec<(Tag, f32)>),
    Pending,
}

/// Identifies one face instance: the file, the face in it, and the
/// variation settings (as bits, so they hash).
type EntryKey = (u64, u32, Vec<(u32, u32)>);

/// The page's fonts: looks them up, keys them and shapes text with them.
/// One per page in slice 1 (spec 0004, child 0004).
pub struct FontContext {
    system: Option<System>,
    /// `None` for a web face its document no longer holds.
    fonts: Vec<Option<FontEntry>>,
    /// Each face instance's index in `fonts`.
    by_face: HashMap<EntryKey, usize>,
    /// Each family list's fonts in order, against the layer of
    /// `layer_generation`.
    resolved: HashMap<ResolveKey, Rc<[Slot]>>,
    /// The generation of the last [`WebFontLayer`] seen.
    layer_generation: u64,
    /// Pending web faces a family list needed since the last
    /// `take_needed_faces`, by their ids.
    needed: Vec<u32>,
    /// Fonts keyed since the last `take_new_fonts`, by index into `fonts`.
    new_fonts: Vec<usize>,
    next_key: u32,
}

impl Default for FontContext {
    fn default() -> Self {
        Self::new()
    }
}

impl FontContext {
    /// A context over the system's fonts. Nothing is scanned until the
    /// first run is shaped.
    pub fn new() -> Self {
        Self {
            system: None,
            fonts: Vec::new(),
            by_face: HashMap::new(),
            resolved: HashMap::new(),
            layer_generation: 0,
            needed: Vec::new(),
            new_fonts: Vec::new(),
            next_key: 0,
        }
    }

    /// A context that sees only `fonts` (font file bytes), none of the
    /// system's: every generic family resolves to them, so text lays out
    /// the same on every machine. For tests and the corpus snapshots.
    pub fn with_fonts(fonts: impl IntoIterator<Item = Vec<u8>>) -> Self {
        Self::isolated(fonts.into_iter().map(|data| (data, true)))
    }

    /// A context that sees only `named` (font file bytes, each found by its
    /// own family name) and `fallback`, none of the system's: every generic
    /// family, and so every family the page names but `named` lacks,
    /// resolves to `fallback` alone. The conformance harness's Ahem plus
    /// Cantarell set (spec 0005, AC-5).
    pub fn with_families(fallback: Vec<u8>, named: impl IntoIterator<Item = Vec<u8>>) -> Self {
        let named = named.into_iter().map(|data| (data, false));
        Self::isolated(named.chain([(fallback, true)]))
    }

    /// A context over `fonts` only, each paired with whether its families
    /// stand in for every generic family.
    fn isolated(fonts: impl IntoIterator<Item = (Vec<u8>, bool)>) -> Self {
        let mut collection = Collection::new(CollectionOptions {
            shared: false,
            system_fonts: false,
        });
        let mut families = Vec::new();
        for (data, generic) in fonts {
            for (family, _) in collection.register_fonts(Blob::from(data), None) {
                if generic && !families.contains(&family) {
                    families.push(family);
                }
            }
        }
        for generic in [
            GenericFamily::Serif,
            GenericFamily::SansSerif,
            GenericFamily::Monospace,
            GenericFamily::Cursive,
            GenericFamily::Fantasy,
            GenericFamily::SystemUi,
        ] {
            collection.set_generic_families(generic, families.iter().copied());
        }
        Self {
            system: Some(System {
                collection,
                source_cache: SourceCache::default(),
            }),
            ..Self::new()
        }
    }

    /// Shapes `text` at `size` in the fonts `font` asks for, with `web` the
    /// document's web faces, one run per stretch of characters that take
    /// the same font. Empty when the system has no usable font at all, so
    /// the text cannot be drawn.
    pub fn shape(
        &mut self,
        text: &str,
        font: &FontDescriptor<'_>,
        size: Au,
        web: &WebFontLayer,
    ) -> Vec<ShapedRun> {
        let slots = self.resolve(font, web);
        let Some(first) = first_slot(&slots) else {
            return Vec::new();
        };
        let mut runs: Vec<((usize, bool), Range<usize>)> = Vec::new();
        for (at, c) in text.char_indices() {
            let end = at + c.len_utf8();
            if let Some((_, range)) = runs.last_mut()
                && extends_cluster(c)
            {
                range.end = end;
                continue;
            }
            let chosen = self
                .choose(&slots, c)
                // No font has it: the first font draws its `.notdef` box.
                .unwrap_or(first);
            match runs.last_mut() {
                Some((font, range)) if *font == chosen => range.end = end,
                _ => runs.push((chosen, at..end)),
            }
        }
        runs.into_iter()
            .filter_map(|((font, hidden), range)| {
                let mut run = self.shape_run(font, text, range, size)?;
                run.hidden_font = hidden;
                Some(run)
            })
            .collect()
    }

    /// The metrics of the first available font for `font` at `size`, for a
    /// line's strut even when it holds no text. `None` without a usable font.
    pub fn metrics(
        &mut self,
        font: &FontDescriptor<'_>,
        size: Au,
        web: &WebFontLayer,
    ) -> Option<LineMetrics> {
        let slots = self.resolve(font, web);
        let (index, _) = first_slot(&slots)?;
        Some(self.entry(index)?.metrics(size))
    }

    /// The ids of the pending web faces text needed since the last call, so
    /// the caller can start loading them (spec 0006, Font trigger).
    pub fn take_needed_faces(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.needed)
    }

    /// Drops every web face, when the document that held them goes away.
    /// Returns their keys, to release with `DeleteFont`.
    pub fn forget_web_fonts(&mut self) -> Vec<FontKey> {
        let mut keys = Vec::new();
        for slot in &mut self.fonts {
            if slot.as_ref().is_some_and(|entry| entry.web)
                && let Some(entry) = slot.take()
            {
                keys.push(entry.key);
            }
        }
        let fonts = &self.fonts;
        self.by_face
            .retain(|_, index| fonts.get(*index).is_some_and(Option::is_some));
        self.new_fonts
            .retain(|&index| fonts.get(index).is_some_and(Option::is_some));
        self.resolved.clear();
        self.needed.clear();
        keys
    }

    fn entry(&self, index: usize) -> Option<&FontEntry> {
        self.fonts.get(index)?.as_ref()
    }

    /// The font for `c`: the first in `slots` with a glyph for it, and
    /// whether a pending web face came before it.
    fn choose(&self, slots: &[Slot], c: char) -> Option<(usize, bool)> {
        let mut hidden = false;
        for slot in slots {
            match *slot {
                Slot::Pending => hidden = true,
                Slot::Font(index) => {
                    if self.entry(index).is_some_and(|entry| entry.has_char(c)) {
                        return Some((index, hidden));
                    }
                }
            }
        }
        None
    }

    /// The faces keyed since the last call, to register with `AddFont`.
    pub fn take_new_fonts(&mut self) -> Vec<NewFont> {
        std::mem::take(&mut self.new_fonts)
            .into_iter()
            .filter_map(|i| self.entry(i))
            .map(|entry| NewFont {
                key: entry.key,
                data: FontData(entry.data.clone()),
                index: entry.index,
                variations: entry
                    .variations
                    .iter()
                    .map(|(tag, value)| (u32::from_be_bytes(tag.to_be_bytes()), *value))
                    .collect(),
            })
            .collect()
    }

    fn shape_run(
        &self,
        index: usize,
        text: &str,
        range: Range<usize>,
        size: Au,
    ) -> Option<ShapedRun> {
        let entry = self.entry(index)?;
        let font = skrifa::FontRef::from_index(entry.data.as_ref(), entry.index).ok()?;
        let shaper = entry
            .shaper
            .shaper(&font)
            .instance(Some(&entry.instance))
            .build();
        let mut buffer = UnicodeBuffer::new();
        buffer.push_str(text.get(range.clone())?);
        // Bidi reordering waits for feature 10: every run is set left to right.
        buffer.set_direction(Direction::LeftToRight);
        buffer.guess_segment_properties();
        let shaped = shaper.shape(buffer, ShapeOptions::new());

        let px_per_unit = size.to_f32_px() / entry.units_per_em;
        let au = |units: i32| Au::from_px(units as f32 * px_per_unit);
        let start = u32::try_from(range.start).unwrap_or(u32::MAX);
        let glyphs = shaped
            .glyph_infos()
            .iter()
            .zip(shaped.glyph_positions())
            .map(|(info, position)| ShapedGlyph {
                id: info.glyph_id,
                cluster: info.cluster.saturating_add(start),
                advance: au(position.x_advance),
                x_offset: au(position.x_offset),
                y_offset: au(position.y_offset),
            })
            .collect();
        Some(ShapedRun {
            font: entry.key,
            size,
            range,
            glyphs,
            metrics: entry.metrics(size),
            hidden_font: false,
        })
    }

    /// The fonts for a family list, best first: for each family, its
    /// matching web face when `web` defines the family (which then hides an
    /// installed font of that name), else the system's face; then the
    /// `serif` fallback when the list names no generic family. Looked up
    /// once per list, weight and slant, until the layer changes.
    fn resolve(&mut self, font: &FontDescriptor<'_>, web: &WebFontLayer) -> Rc<[Slot]> {
        if web.generation != self.layer_generation {
            self.layer_generation = web.generation;
            self.resolved.clear();
        }
        let key = (font.families.to_vec(), font.weight, font.slant);
        if let Some(found) = self.resolved.get(&key) {
            return Rc::clone(found);
        }
        let attributes = Attributes::new(
            FontWidth::NORMAL,
            match font.slant {
                Slant::Normal => QueryStyle::Normal,
                Slant::Italic => QueryStyle::Italic,
                Slant::Oblique => QueryStyle::Oblique(None),
            },
            FontWeight::new(f32::from(font.weight)),
        );
        let has_generic = font.families.iter().any(|name| generic(name).is_some());

        let system = self.system.get_or_insert_with(|| System {
            collection: Collection::new(CollectionOptions {
                shared: false,
                system_fonts: true,
            }),
            source_cache: SourceCache::default(),
        });
        let mut picks: Vec<Pick> = Vec::new();
        for name in font.families {
            if let Some(generic) = generic(name) {
                picks.extend(
                    first_font(system, QueryFamily::Generic(generic), attributes).map(Pick::System),
                );
                continue;
            }
            let Some(faces) = web.faces(name) else {
                picks.extend(
                    first_font(system, QueryFamily::Named(name), attributes).map(Pick::System),
                );
                continue;
            };
            let Some(face) = match_face(faces, font.weight, font.slant) else {
                continue;
            };
            match &face.state {
                FaceState::Ready(data) => {
                    let weight = font.weight.clamp(face.weight.0, face.weight.1);
                    picks.push(Pick::Web(data.clone(), weight_variation(data, weight)));
                }
                FaceState::Pending => {
                    if !self.needed.contains(&face.id) {
                        self.needed.push(face.id);
                    }
                    picks.push(Pick::Pending);
                }
                FaceState::Fallback => {}
            }
        }
        if !has_generic {
            picks.extend(
                first_font(
                    system,
                    QueryFamily::Generic(GenericFamily::Serif),
                    attributes,
                )
                .map(Pick::System),
            );
        }
        if !picks.iter().any(|pick| !matches!(pick, Pick::Pending)) {
            // A system without the generic (a bare Linux box may lack a
            // serif): any usable font beats drawing nothing.
            picks.extend(
                [GenericFamily::SansSerif, GenericFamily::SystemUi]
                    .into_iter()
                    .find_map(|generic| {
                        first_font(system, QueryFamily::Generic(generic), attributes)
                    })
                    .map(Pick::System),
            );
        }

        let mut slots: Vec<Slot> = Vec::new();
        for pick in picks {
            let slot = match pick {
                Pick::System(query_font) => self.entry_for(query_font).map(Slot::Font),
                Pick::Web(data, variations) => self.web_entry(data, variations).map(Slot::Font),
                Pick::Pending => Some(Slot::Pending),
            };
            if let Some(slot) = slot
                && (slot == Slot::Pending || !slots.contains(&slot))
            {
                slots.push(slot);
            }
        }
        if first_slot(&slots).is_none() {
            warn!("no system font found; text will not be drawn");
        }
        let slots: Rc<[Slot]> = slots.into();
        self.resolved.insert(key, Rc::clone(&slots));
        slots
    }

    /// The entry for a loaded web face at `variations`, made and keyed on
    /// first use.
    fn web_entry(&mut self, data: FontData, variations: Vec<(Tag, f32)>) -> Option<usize> {
        let face = (data.0.id(), 0, variation_bits(&variations));
        if let Some(&index) = self.by_face.get(&face) {
            return Some(index);
        }
        let mut entry = self.make_entry(data.0, 0, None, variations)?;
        entry.web = true;
        debug!(key = entry.key.0, "web font loaded");
        Some(self.push_entry(face, entry))
    }

    /// The entry for a face instance, made and keyed on first use. `None`
    /// when the bytes are not a font.
    fn entry_for(&mut self, font: QueryFont) -> Option<usize> {
        let variations: Vec<(Tag, f32)> = font.synthesis.variation_settings().to_vec();
        let face = (font.blob.id(), font.index, variation_bits(&variations));
        if let Some(&index) = self.by_face.get(&face) {
            return Some(index);
        }
        let entry = self.make_entry(font.blob, font.index, Some(font.charmap_index), variations)?;
        debug!(
            key = entry.key.0,
            index = entry.index,
            variations = entry.variations.len(),
            "font loaded"
        );
        Some(self.push_entry(face, entry))
    }

    /// Stores a new entry and queues it for `AddFont`.
    fn push_entry(&mut self, face: EntryKey, entry: FontEntry) -> usize {
        self.fonts.push(Some(entry));
        let index = self.fonts.len() - 1;
        self.by_face.insert(face, index);
        self.new_fonts.push(index);
        index
    }

    /// Reads a face's metrics and shaping tables at its variation settings
    /// and gives it the next key.
    fn make_entry(
        &mut self,
        data: Blob<u8>,
        index: u32,
        charmap: Option<CharmapIndex>,
        variations: Vec<(Tag, f32)>,
    ) -> Option<FontEntry> {
        let font = skrifa::FontRef::from_index(data.as_ref(), index).ok()?;
        let location = font.axes().location(variations.iter().copied());
        let metrics = font.metrics(Size::unscaled(), LocationRef::from(&location));
        let shaper = ShaperData::new(&font);
        let instance = ShaperInstance::from_coords(&font, location.coords().iter().copied());
        let key = FontKey(self.next_key);
        self.next_key = self.next_key.saturating_add(1);
        Some(FontEntry {
            key,
            index,
            charmap,
            web: false,
            variations,
            units_per_em: f32::from(metrics.units_per_em.max(1)),
            ascent: metrics.ascent,
            // Fonts store the descent below the baseline as a negative number.
            descent: metrics.descent.abs(),
            line_gap: metrics.leading.max(0.0),
            underline: metrics.underline.map(|u| (u.offset, u.thickness)),
            shaper,
            instance,
            data,
        })
    }
}

/// The first font in a resolved list, and whether a pending web face
/// comes before it.
fn first_slot(slots: &[Slot]) -> Option<(usize, bool)> {
    let mut hidden = false;
    for slot in slots {
        match *slot {
            Slot::Pending => hidden = true,
            Slot::Font(index) => return Some((index, hidden)),
        }
    }
    None
}

/// Variation settings as bits, so they hash.
fn variation_bits(variations: &[(Tag, f32)]) -> Vec<(u32, u32)> {
    variations
        .iter()
        .map(|(tag, value)| (u32::from_be_bytes(tag.to_be_bytes()), value.to_bits()))
        .collect()
}

/// The `wght` setting for a variable web face at `weight`, already clamped
/// to the face's range; nothing for a static face.
fn weight_variation(data: &FontData, weight: u16) -> Vec<(Tag, f32)> {
    let wght = Tag::new(b"wght");
    let variable = skrifa::FontRef::new(data)
        .is_ok_and(|font| font.axes().iter().any(|axis| axis.tag() == wght));
    if variable {
        vec![(wght, f32::from(weight))]
    } else {
        Vec::new()
    }
}

/// The best face of one family for `attributes`, if the system has the
/// family.
fn first_font(
    system: &mut System,
    family: QueryFamily<'_>,
    attributes: Attributes,
) -> Option<QueryFont> {
    let mut query = system.collection.query(&mut system.source_cache);
    query.set_families([family]);
    query.set_attributes(attributes);
    query.first_available_font()
}

/// The generic family a `font-family` keyword names, if it is one.
fn generic(name: &str) -> Option<GenericFamily> {
    Some(match name {
        "serif" => GenericFamily::Serif,
        "sans-serif" => GenericFamily::SansSerif,
        "monospace" => GenericFamily::Monospace,
        "cursive" => GenericFamily::Cursive,
        "fantasy" => GenericFamily::Fantasy,
        "system-ui" => GenericFamily::SystemUi,
        _ => return None,
    })
}

/// The grapheme cluster boundaries of `text` (user perceived characters,
/// UAX #29), as byte offsets from 0 to `text.len()`, both included (spec
/// 0008, AC-13). A caret only ever sits on one of these.
pub fn grapheme_boundaries(text: &str) -> Vec<usize> {
    let mut boundaries: Vec<usize> = icu_segmenter::GraphemeClusterSegmenter::new()
        .segment_str(text)
        .collect();
    // The segmenter yields 0 and the end itself, except for empty text.
    if boundaries.first() != Some(&0) {
        boundaries.insert(0, 0);
    }
    boundaries.dedup();
    boundaries
}

/// Whether `c` belongs with the character before it whatever fonts have
/// it: combining marks, variation selectors and joiners (spec 0004, child
/// 0004, Itemize by font). The common blocks only; full Unicode properties
/// arrive with feature 10.
fn extends_cluster(c: char) -> bool {
    matches!(
        u32::from(c),
        0x0300..=0x036F
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x200C..=0x200D
            | 0x20D0..=0x20FF
            | 0xFE00..=0xFE0F
            | 0xFE20..=0xFE2F
            | 0xE0100..=0xE01EF
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serif() -> FontDescriptor<'static> {
        FontDescriptor {
            families: &[],
            weight: 400,
            slant: Slant::Normal,
        }
    }

    #[test]
    fn shaping_gives_one_advancing_glyph_per_letter_and_keys_the_font_once() {
        let mut fonts = FontContext::new();
        let runs = fonts.shape(
            "Hello",
            &serif(),
            Au::from_px(16.0),
            &WebFontLayer::default(),
        );
        let Some(run) = runs.first() else {
            // A machine with no fonts at all (a bare CI box) has nothing to test.
            return;
        };
        assert_eq!(runs.len(), 1);
        assert_eq!(run.glyphs.len(), 5);
        assert!(run.glyphs.iter().all(|g| g.advance > Au::ZERO));
        assert_eq!(
            run.glyphs.iter().map(|g| g.cluster).collect::<Vec<_>>(),
            [0, 1, 2, 3, 4]
        );
        assert!(run.metrics.ascent > Au::ZERO && run.metrics.descent > Au::ZERO);
        assert!(
            run.metrics.underline_offset < Au::ZERO,
            "underlines sit below"
        );
        assert!(run.metrics.underline_thickness > Au::ZERO);

        let new = fonts.take_new_fonts();
        assert!(!new.is_empty());
        assert!(new.iter().any(|f| f.key == run.font));
        assert!(new.iter().all(|f| !f.data.is_empty()));

        // The same face again: same key, nothing new to send.
        let again = fonts.shape(
            "Hello world",
            &serif(),
            Au::from_px(32.0),
            &WebFontLayer::default(),
        );
        assert_eq!(again[0].font, run.font);
        assert!(fonts.take_new_fonts().is_empty());
        assert!(
            again[0].glyphs[0].advance > run.glyphs[0].advance,
            "advances scale with the size"
        );
    }

    #[test]
    fn families_resolve_in_order_and_unknown_names_fall_through() {
        let mut fonts = FontContext::new();
        let size = Au::from_px(16.0);
        if fonts
            .shape("a", &serif(), size, &WebFontLayer::default())
            .is_empty()
        {
            return;
        }
        let mono = [Atom::from("monospace")];
        let monospace = FontDescriptor {
            families: &mono,
            ..serif()
        };
        let missing = [Atom::from("No Such Font 12345"), Atom::from("monospace")];
        let fallthrough = FontDescriptor {
            families: &missing,
            ..serif()
        };
        let a = fonts.shape("iiii", &monospace, size, &WebFontLayer::default());
        let b = fonts.shape("iiii", &fallthrough, size, &WebFontLayer::default());
        assert_eq!(a[0].font, b[0].font, "a missing family is skipped");
        // A monospace font gives `i` and `m` the same advance.
        let m = fonts.shape("m", &monospace, size, &WebFontLayer::default());
        assert_eq!(a[0].glyphs[0].advance, m[0].glyphs[0].advance);
    }

    #[test]
    fn with_families_finds_named_fonts_and_sends_every_other_family_to_the_fallback() {
        let ahem =
            include_bytes!("../../../tools/conformance/tests/fixtures/fonts/Ahem.ttf").to_vec();
        let cantarell =
            include_bytes!("../../../tools/conformance/fonts/Cantarell-Regular.ttf").to_vec();
        let mut fonts = FontContext::with_families(cantarell, [ahem]);
        let size = Au::from_px(10.0);
        let shape = |fonts: &mut FontContext, names: &[Atom]| {
            let font = FontDescriptor {
                families: names,
                ..serif()
            };
            fonts.shape("x", &font, size, &WebFontLayer::default())[0].clone()
        };
        let ahem = shape(&mut fonts, &[Atom::from("Ahem")]);
        // Every Ahem glyph is a 1em square.
        assert_eq!(ahem.glyphs[0].advance, size);
        let generic = shape(&mut fonts, &[Atom::from("monospace")]);
        let unknown = shape(&mut fonts, &[Atom::from("Arial")]);
        let default = shape(&mut fonts, &[]);
        assert_ne!(generic.font, ahem.font);
        assert_eq!(generic.font, unknown.font);
        assert_eq!(generic.font, default.font);
    }

    #[test]
    fn bold_picks_another_face_or_instance() {
        let mut fonts = FontContext::new();
        let size = Au::from_px(16.0);
        let regular = fonts.shape("a", &serif(), size, &WebFontLayer::default());
        let Some(regular) = regular.first() else {
            return;
        };
        let bold = fonts.shape(
            "a",
            &FontDescriptor {
                weight: 700,
                ..serif()
            },
            size,
            &WebFontLayer::default(),
        );
        assert_ne!(bold[0].font, regular.font);
    }

    #[test]
    fn grapheme_boundaries_keep_emoji_and_marks_whole() {
        // Spec 0008, AC-13
        assert_eq!(grapheme_boundaries(""), [0]);
        assert_eq!(grapheme_boundaries("ab"), [0, 1, 2]);
        // A thumbs up with a skin tone modifier is one grapheme.
        let thumbs = "a\u{1F44D}\u{1F3FD}b";
        assert_eq!(grapheme_boundaries(thumbs), [0, 1, 9, 10]);
        // e and a combining acute accent.
        assert_eq!(grapheme_boundaries("e\u{301}x"), [0, 3, 4]);
    }

    #[test]
    fn clusters_count_from_the_start_of_the_whole_text() {
        let mut fonts = FontContext::new();
        let runs = fonts.shape(
            "ab cd",
            &serif(),
            Au::from_px(16.0),
            &WebFontLayer::default(),
        );
        let Some(run) = runs.first() else {
            return;
        };
        assert_eq!(run.range, 0..5);
        assert_eq!(run.glyphs.last().map(|g| g.cluster), Some(4));
    }

    fn ahem() -> Vec<u8> {
        include_bytes!("../../../tools/conformance/tests/fixtures/fonts/Ahem.ttf").to_vec()
    }

    fn cantarell() -> Vec<u8> {
        include_bytes!("../../../tools/conformance/fonts/Cantarell-Regular.ttf").to_vec()
    }

    fn face(id: u32, weight: (u16, u16), slant: Slant) -> WebFace {
        WebFace {
            id,
            weight,
            slant,
            state: FaceState::Fallback,
        }
    }

    #[test]
    fn face_matching_takes_style_first_then_the_weight_rule() {
        // Spec 0006, Value sourcing, Face choice (CSS Fonts 4).
        let faces = [
            face(1, (300, 300), Slant::Normal),
            face(2, (600, 600), Slant::Normal),
            face(3, (700, 700), Slant::Normal),
            face(4, (400, 400), Slant::Italic),
        ];
        let id = |weight, slant| match_face(&faces, weight, slant).map(|f| f.id);
        assert_eq!(id(700, Slant::Normal), Some(3), "an exact weight");
        assert_eq!(id(700, Slant::Italic), Some(4), "italic before any weight");
        assert_eq!(
            id(400, Slant::Oblique),
            Some(4),
            "oblique falls back to italic"
        );
        // 400 to 500: up to 500 first, then down, then above 500.
        assert_eq!(id(450, Slant::Normal), Some(1));
        // Below 400: lighter first; above 500: heavier first.
        assert_eq!(id(200, Slant::Normal), Some(1));
        assert_eq!(id(350, Slant::Normal), Some(1));
        assert_eq!(id(650, Slant::Normal), Some(3));
        assert_eq!(id(900, Slant::Normal), Some(3));
        let ranged = [
            face(1, (100, 900), Slant::Normal),
            face(2, (100, 900), Slant::Normal),
        ];
        assert_eq!(
            match_face(&ranged, 500, Slant::Normal).map(|f| f.id),
            Some(2),
            "the later face wins a tie"
        );
        assert!(match_face(&[], 400, Slant::Normal).is_none());
    }

    #[test]
    fn only_truetype_and_opentype_files_load() {
        assert!(FontData::parse(ahem()).is_some());
        assert!(
            FontData::parse(b"wOF2\0\0\0\0rest".to_vec()).is_none(),
            "WOFF2"
        );
        assert!(
            FontData::parse(b"wOFF\0\0\0\0rest".to_vec()).is_none(),
            "WOFF"
        );
        assert!(
            FontData::parse(vec![0, 1, 0, 0, 9, 9]).is_none(),
            "the magic alone"
        );
        assert!(FontData::parse(Vec::new()).is_none());
    }

    #[test]
    fn web_faces_shape_their_document_only_and_hide_text_while_pending() {
        // Spec 0006, AC-10, AC-11, Key invariants (font scope).
        let size = Au::from_px(10.0);
        let mut fonts = FontContext::with_fonts([cantarell()]);
        let families = [Atom::from("Icons"), Atom::from("sans-serif")];
        let descriptor = FontDescriptor {
            families: &families,
            ..serif()
        };
        let layer = |generation, state| {
            let mut layer = WebFontLayer::new(generation);
            layer.add(
                "ICONS",
                WebFace {
                    id: 7,
                    weight: (400, 400),
                    slant: Slant::Normal,
                    state,
                },
            );
            layer
        };

        // Pending: laid out in the fallback, hidden, and asked for once.
        let pending = layer(1, FaceState::Pending);
        let runs = fonts.shape("x", &descriptor, size, &pending);
        assert!(runs[0].hidden_font);
        assert_ne!(runs[0].glyphs[0].advance, size, "not Ahem yet");
        let fallback_font = runs[0].font;
        assert!(fonts.metrics(&descriptor, size, &pending).is_some());
        assert_eq!(fonts.take_needed_faces(), [7]);
        assert!(fonts.take_needed_faces().is_empty());

        // Loaded: Ahem's 1em squares, keyed and queued for AddFont.
        let _ = fonts.take_new_fonts();
        let data = FontData::parse(ahem()).unwrap();
        let ready = layer(2, FaceState::Ready(data));
        let runs = fonts.shape("x", &descriptor, size, &ready);
        assert!(!runs[0].hidden_font);
        assert_eq!(runs[0].glyphs[0].advance, size);
        let web_key = runs[0].font;
        assert!(fonts.take_new_fonts().iter().any(|f| f.key == web_key));

        // Another document without the rule: the name means nothing there.
        let runs = fonts.shape("x", &descriptor, size, &WebFontLayer::new(3));
        assert_eq!(runs[0].font, fallback_font);
        assert!(!runs[0].hidden_font);

        // Past its block period: drawn in the fallback.
        let runs = fonts.shape("x", &descriptor, size, &layer(4, FaceState::Fallback));
        assert_eq!(runs[0].font, fallback_font);
        assert!(!runs[0].hidden_font);

        // The document goes: its face's key is handed back once.
        assert_eq!(fonts.forget_web_fonts(), [web_key]);
        assert!(fonts.forget_web_fonts().is_empty());
    }

    #[test]
    fn combining_marks_stay_with_their_base() {
        assert!(extends_cluster('\u{0301}'));
        assert!(extends_cluster('\u{FE0F}'));
        assert!(!extends_cluster('a'));
    }
}
