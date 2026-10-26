//! Font bytes the renderer rasterizes glyphs from (spec 0032).
//!
//! The bytes arrive from the browser process, which maps or reads a system
//! font file once per file and receives a web font's shared region once per
//! blob. Keeping them behind one `Arc` means every key, on every page, that
//! names the same file shares the bytes, and a key going away never frees
//! bytes another key still uses (spec 0032, Key invariants).

use std::fmt;
use std::ops::Deref;
use std::sync::{Arc, Weak};

/// Where a font's bytes live, for the memory report (spec 0032, AC-10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontStorage {
    /// A read only system font file, memory mapped; mostly outside the
    /// process footprint.
    Mapped,
    /// Bytes the browser read into its own heap.
    Heap,
    /// A shared memory region from the content process (a web font).
    Shared,
}

/// A font file's bytes, shared by every key that names the file.
#[derive(Clone)]
pub struct FontBytes {
    pub(crate) data: Arc<dyn AsRef<[u8]> + Send + Sync>,
    pub(crate) storage: FontStorage,
    pub(crate) len: usize,
}

impl FontBytes {
    fn new<T>(data: T, storage: FontStorage) -> Self
    where
        T: AsRef<[u8]> + Send + Sync + 'static,
    {
        let len = data.as_ref().len();
        Self {
            data: Arc::new(data),
            storage,
            len,
        }
    }

    /// Bytes of a memory mapped, read only system font file.
    pub fn mapped<T>(data: T) -> Self
    where
        T: AsRef<[u8]> + Send + Sync + 'static,
    {
        Self::new(data, FontStorage::Mapped)
    }

    /// Bytes read from a user writable font file into the heap.
    pub fn heap(data: Vec<u8>) -> Self {
        Self::new(data, FontStorage::Heap)
    }

    /// Bytes of a shared memory region (a web font), kept without a copy.
    pub fn shared<T>(data: T) -> Self
    where
        T: AsRef<[u8]> + Send + Sync + 'static,
    {
        Self::new(data, FontStorage::Shared)
    }

    /// A weak handle into these bytes, so a cache can share them without
    /// keeping them alive (spec 0032, AC-2).
    pub fn handle(&self) -> Weak<dyn AsRef<[u8]> + Send + Sync> {
        Arc::downgrade(&self.data)
    }

    /// The bytes a live handle still holds, or `None` once they are gone.
    pub fn from_handle(
        handle: &Weak<dyn AsRef<[u8]> + Send + Sync>,
        storage: FontStorage,
    ) -> Option<Self> {
        let data = handle.upgrade()?;
        let slot: &dyn AsRef<[u8]> = data.as_ref();
        let len = slot.as_ref().len();
        Some(Self { data, storage, len })
    }

    /// Where the bytes live.
    pub fn storage(&self) -> FontStorage {
        self.storage
    }

    /// The number of bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// An address that is the same for every handle into one allocation, so
    /// the memory report can count a shared file once (spec 0032, AC-2).
    pub(crate) fn identity(&self) -> usize {
        let slot: &dyn AsRef<[u8]> = self.data.as_ref();
        slot.as_ref().as_ptr() as usize
    }

    /// Whether there are no bytes at all.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl AsRef<[u8]> for FontBytes {
    fn as_ref(&self) -> &[u8] {
        let slot: &dyn AsRef<[u8]> = self.data.as_ref();
        slot.as_ref()
    }
}

impl Deref for FontBytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        let slot: &dyn AsRef<[u8]> = self.data.as_ref();
        slot.as_ref()
    }
}

impl fmt::Debug for FontBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FontBytes")
            .field("storage", &self.storage)
            .field("len", &self.len)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_live_until_the_last_owner_goes() {
        // Several keys may name one file; deleting the first must not free
        // bytes another still uses (spec 0032, Key invariants).
        let bytes = FontBytes::heap(vec![1, 2, 3]);
        let handle = bytes.handle();
        let second = bytes.clone();
        assert_eq!(bytes.identity(), second.identity());
        drop(bytes);
        let still_there = FontBytes::from_handle(&handle, FontStorage::Heap);
        assert!(still_there.is_some());
        assert_eq!(still_there.map(|b| b.len()), Some(3));
        drop(second);
        assert!(FontBytes::from_handle(&handle, FontStorage::Heap).is_none());
    }
}
