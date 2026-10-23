//! The generational arena that holds DOM nodes and layout boxes (spec 0002, Arena).

use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::num::NonZeroU32;

/// A handle into an [`Arena`]: a slot index plus the generation the slot had
/// when the value was stored.
///
/// `M` only tells ids of different arenas apart, so a node id cannot be
/// passed where a box id is expected. `Id` is 8 bytes, and so is
/// `Option<Id>` thanks to the non zero generation (AC-18).
pub struct Id<M> {
    index: u32,
    generation: NonZeroU32,
    _marker: PhantomData<fn() -> M>,
}

impl<M> Id<M> {
    /// The slot index. Stable for as long as the value lives.
    pub fn index(self) -> u32 {
        self.index
    }

    /// The slot's generation when the value was stored.
    pub fn generation(self) -> NonZeroU32 {
        self.generation
    }

    /// The id with these parts, as a script wrapper's key stores them
    /// (spec 0004, child 0006). An id for a slot that was freed or reused
    /// finds nothing in the arena, so a made up one is harmless.
    pub fn from_parts(index: u32, generation: NonZeroU32) -> Self {
        Self {
            index,
            generation,
            _marker: PhantomData,
        }
    }
}

// Manual impls: deriving would wrongly require `M` itself to be `Copy`, `Eq`, and so on.
impl<M> Clone for Id<M> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M> Copy for Id<M> {}

impl<M> PartialEq for Id<M> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && self.generation == other.generation
    }
}

impl<M> Eq for Id<M> {}

impl<M> Hash for Id<M> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.index.hash(state);
        self.generation.hash(state);
    }
}

impl<M> fmt::Debug for Id<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Id({}v{})", self.index, self.generation)
    }
}

struct Slot<T> {
    generation: NonZeroU32,
    value: Option<T>,
}

/// A `Vec` of slots plus a free list. Freeing a slot clears it and bumps its
/// generation, so an old [`Id`] for that slot reads `None` from then on (AC-3).
///
/// `T` is the stored value and `M` the id marker; they differ only when the
/// id type must be named by a crate that cannot name `T` (see `BoxId`).
pub struct Arena<T, M = T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
    len: usize,
    _marker: PhantomData<fn() -> M>,
}

impl<T: fmt::Debug, M> fmt::Debug for Arena<T, M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<T, M> Default for Arena<T, M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, M> Arena<T, M> {
    /// An empty arena.
    pub const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            len: 0,
            _marker: PhantomData,
        }
    }

    /// How many values are stored.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether no values are stored.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Stores `value` and returns its id. Reuses a freed slot when there is one.
    pub fn insert(&mut self, value: T) -> Id<M> {
        self.len += 1;
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.value = Some(value);
            return Id {
                index,
                generation: slot.generation,
                _marker: PhantomData,
            };
        }
        #[expect(
            clippy::expect_used,
            reason = "4 billion slots would need hundreds of GB; the process runs out of memory long before"
        )]
        let index = u32::try_from(self.slots.len()).expect("arena exceeded u32::MAX slots");
        self.slots.push(Slot {
            generation: NonZeroU32::MIN,
            value: Some(value),
        });
        Id {
            index,
            generation: NonZeroU32::MIN,
            _marker: PhantomData,
        }
    }

    /// The value for `id`, or `None` if it was freed.
    pub fn get(&self, id: Id<M>) -> Option<&T> {
        self.slots
            .get(id.index as usize)
            .filter(|slot| slot.generation == id.generation)
            .and_then(|slot| slot.value.as_ref())
    }

    /// The value for `id` to change, or `None` if it was freed.
    pub fn get_mut(&mut self, id: Id<M>) -> Option<&mut T> {
        self.slots
            .get_mut(id.index as usize)
            .filter(|slot| slot.generation == id.generation)
            .and_then(|slot| slot.value.as_mut())
    }

    /// Whether `id` still names a live value.
    pub fn contains(&self, id: Id<M>) -> bool {
        self.get(id).is_some()
    }

    /// The generation the slot at `index` has now, if the slot exists.
    /// Used to report a stale id clearly.
    pub fn current_generation(&self, index: u32) -> Option<NonZeroU32> {
        self.slots.get(index as usize).map(|slot| slot.generation)
    }

    /// Frees `id` and returns its value, or `None` if it was already freed.
    pub fn remove(&mut self, id: Id<M>) -> Option<T> {
        let slot = self.slots.get_mut(id.index as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        let value = slot.value.take()?;
        self.len -= 1;
        // A slot whose generation would overflow is retired instead of reused,
        // so no two values ever share an id.
        if let Some(next) = slot.generation.checked_add(1) {
            slot.generation = next;
            self.free.push(id.index);
        }
        Some(value)
    }

    /// Every live value with its id, in slot order.
    pub fn iter(&self) -> impl Iterator<Item = (Id<M>, &T)> {
        self.slots.iter().zip(0u32..).filter_map(|(slot, index)| {
            slot.value.as_ref().map(|value| {
                let id = Id {
                    index,
                    generation: slot.generation,
                    _marker: PhantomData,
                };
                (id, value)
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::mem::size_of;

    use super::*;

    #[test]
    fn ids_are_eight_bytes_even_as_options() {
        assert_eq!(size_of::<Id<()>>(), 8);
        assert_eq!(size_of::<Option<Id<()>>>(), 8);
    }

    #[test]
    fn freed_ids_read_none_and_reused_slots_get_a_higher_generation() {
        let mut arena: Arena<&str> = Arena::new();
        let a = arena.insert("a");
        assert_eq!(arena.get(a), Some(&"a"));

        assert_eq!(arena.remove(a), Some("a"));
        assert_eq!(arena.get(a), None);
        assert_eq!(arena.remove(a), None);

        let b = arena.insert("b");
        assert_eq!(b.index(), a.index(), "the freed slot is reused");
        assert!(b.generation() > a.generation());
        assert_eq!(arena.get(a), None, "the old id must not read the new value");
        assert_eq!(arena.get(b), Some(&"b"));
        assert_eq!(arena.len(), 1);
    }

    #[test]
    fn iter_skips_freed_slots() {
        let mut arena: Arena<u32> = Arena::new();
        let ids: Vec<_> = (0..4).map(|n| arena.insert(n)).collect();
        arena.remove(ids[1]);
        let values: Vec<u32> = arena.iter().map(|(_, v)| *v).collect();
        assert_eq!(values, [0, 2, 3]);
    }
}
