use core::borrow::Borrow;
use core::cmp::Ordering;
use core::error::Error;
use core::fmt::{self, Debug};
use core::hash::{Hash, Hasher};
use core::iter::FusedIterator;
use core::marker::PhantomData;
use core::mem::{self, ManuallyDrop};
use core::ops::{Bound, Index, RangeBounds};
use core::ptr;
use std::cell::{Cell, UnsafeCell};
use orengine_utils::hints::cold_path;
use crate::self_allocated_structs::btree::entry::Entry::{Occupied, Vacant};
use crate::self_allocated_structs::btree::entry::{Entry, OccupiedEntry, OccupiedError, VacantEntry};
use crate::test_assert::test_assert;
use crate::thread_local_allocator::AllocatorForSelfAllocations;
use super::borrow::DormantMutRef;
use super::dedup_sorted_iter::DedupSortedIter;
use super::navigate::{LazyLeafRange, LeafRange};
use super::node::ForceResult::*;
use super::node::{self, NodeHandle, NodeRef, Root, marker};
use super::search::SearchResult::*;

/// Minimum number of elements in a node that is not a root.
/// We might temporarily have fewer elements during methods.
pub(super) const MIN_LEN: usize = node::MIN_LEN_AFTER_SPLIT;

// A tree in a `BTreeMap` is a tree in the `node` module with additional invariants:
// - Keys must appear in ascending order (according to the key's type).
// - Every non-leaf node contains at least 1 element (has at least 2 children).
// - Every non-root node contains at least MIN_LEN elements.
//
// An empty map is represented either by the absence of a root node or by a
// root node that is an empty leaf.

pub(super) struct SelfAllocatedMemory([(usize, u32); 2]);

impl SelfAllocatedMemory {
    const fn new() -> Self {
        SelfAllocatedMemory([(0, 0); 2])
    }

    pub(super) fn inc_for_size(&mut self, size: usize) {
        let idx = if self.0[0].0 == size {
            0
        } else if self.0[1].0 == size {
            1
        } else {
            cold_path();

            if self.0[0].0 == 0 {
                self.0[0].0 = size;

                0
            } else {
                self.0[1].0 = size;

                1
            }
        };

        self.0[idx].1 += 1;
    }

    pub(super) fn dec_for_size(&mut self, size: usize) {
        let idx = if self.0[0].0 == size {
            0
        } else {
            1
        };

        self.0[idx].1 -= 1;
    }
}

/// An ordered map based on a [B-Tree].
///
/// B-Trees represent a fundamental compromise between cache-efficiency and actually minimizing
/// the amount of work performed in a search. In theory, a binary search tree (BST) is the optimal
/// choice for a sorted map, as a perfectly balanced BST performs the theoretical minimum amount of
/// comparisons necessary to find an element (log<sub>2</sub>n). However, in practice the way this
/// is done is *very* inefficient for modern computer architectures. In particular, every element
/// is stored in its own individually heap-allocated node. This means that every single insertion
/// triggers a heap-allocation, and every single comparison should be a cache-miss. Since these
/// are both notably expensive things to do in practice, we are forced to, at the very least,
/// reconsider the BST strategy.
///
/// A B-Tree instead makes each node contain B-1 to 2B-1 elements in a contiguous array. By doing
/// this, we reduce the number of allocations by a factor of B, and improve cache efficiency in
/// searches. However, this does mean that searches will have to do *more* comparisons on average.
/// The precise number of comparisons depends on the node search strategy used. For optimal cache
/// efficiency, one could search the nodes linearly. For optimal comparisons, one could search
/// the node using binary search. As a compromise, one could also perform a linear search
/// that initially only checks every i<sup>th</sup> element for some choice of i.
///
/// Currently, our implementation simply performs naive linear search. This provides excellent
/// performance on *small* nodes of elements which are cheap to compare. However in the future we
/// would like to further explore choosing the optimal search strategy based on the choice of B,
/// and possibly other factors. Using linear search, searching for a random element is expected
/// to take B * log(n) comparisons, which is generally worse than a BST. In practice,
/// however, performance is excellent.
///
/// It is a logic error for a key to be modified in such a way that the key's ordering relative to
/// any other key, as determined by the [`Ord`] trait, changes while it is in the map. This is
/// normally only possible through [`Cell`], [`RefCell`], global state, I/O, or unsafe code.
/// The behavior resulting from such a logic error is not specified, but will be encapsulated to the
/// `BTreeMap` that observed the logic error and not result in undefined behavior. This could
/// include panics, incorrect results, aborts, memory leaks, and non-termination.
///
/// Iterators obtained from functions such as [`SelfAllocatedBTreeMap::iter`], [`SelfAllocatedBTreeMap::into_iter`], [`SelfAllocatedBTreeMap::values`], or
/// [`SelfAllocatedBTreeMap::keys`] produce their items in order by key, and take worst-case logarithmic and
/// amortized constant time per item returned.
///
/// [B-Tree]: https://en.wikipedia.org/wiki/B-tree
/// [`Cell`]: core::cell::Cell
/// [`RefCell`]: core::cell::RefCell
///
/// # Examples
///
/// ```
/// use std::collections::BTreeMap;
///
/// // type inference lets us omit an explicit type signature (which
/// // would be `BTreeMap<&str, &str>` in this example).
/// let mut movie_reviews = BTreeMap::new();
///
/// // review some movies.
/// movie_reviews.insert("Office Space",       "Deals with real issues in the workplace.");
/// movie_reviews.insert("Pulp Fiction",       "Masterpiece.");
/// movie_reviews.insert("The Godfather",      "Very enjoyable.");
/// movie_reviews.insert("The Blues Brothers", "Eye lyked it a lot.");
///
/// // check for a specific one.
/// if !movie_reviews.contains_key("Les Misérables") {
///     println!("We've got {} reviews, but Les Misérables ain't one.",
///              movie_reviews.len());
/// }
///
/// // oops, this review has a lot of spelling mistakes, let's delete it.
/// movie_reviews.remove("The Blues Brothers");
///
/// // look up the values associated with some keys.
/// let to_find = ["Up!", "Office Space"];
/// for movie in &to_find {
///     match movie_reviews.get(movie) {
///        Some(review) => println!("{movie}: {review}"),
///        None => println!("{movie} is unreviewed.")
///     }
/// }
///
/// // Look up the value for a key (will panic if the key is not found).
/// println!("Movie review: {}", movie_reviews["Office Space"]);
///
/// // iterate over everything.
/// for (movie, review) in &movie_reviews {
///     println!("{movie}: \"{review}\"");
/// }
/// ```
///
/// A `BTreeMap` with a known list of items can be initialized from an array:
///
/// ```
/// use std::collections::BTreeMap;
///
/// let solar_distance = BTreeMap::from([
///     ("Mercury", 0.4),
///     ("Venus", 0.7),
///     ("Earth", 1.0),
///     ("Mars", 1.5),
/// ]);
/// ```
///
/// `BTreeMap` implements an [`Entry API`], which allows for complex
/// methods of getting, setting, updating and removing keys and their values:
///
/// [`Entry API`]: SelfAllocatedBTreeMap::entry
///
/// ```
/// use std::collections::BTreeMap;
///
/// // type inference lets us omit an explicit type signature (which
/// // would be `BTreeMap<&str, u8>` in this example).
/// let mut player_stats = BTreeMap::new();
///
/// fn random_stat_buff() -> u8 {
///     // could actually return some random value here - let's just return
///     // some fixed value for now
///     42
/// }
///
/// // insert a key only if it doesn't already exist
/// player_stats.entry("health").or_insert(100);
///
/// // insert a key using a function that provides a new value only if it
/// // doesn't already exist
/// player_stats.entry("defence").or_insert_with(random_stat_buff);
///
/// // update a key, guarding against the key possibly not being set
/// let stat = player_stats.entry("attack").or_insert(100);
/// *stat += random_stat_buff();
///
/// // modify an entry before an insert with in-place mutation
/// player_stats.entry("mana").and_modify(|mana| *mana += 200).or_insert(100);
/// ```
pub(crate) struct SelfAllocatedBTreeMap<
    K,
    V,
> {
    pub(crate) root: Option<Root<K, V>>,
    pub(crate) length: usize,
    pub(super) allocated_memory: SelfAllocatedMemory,
}

impl<K, V> Drop for SelfAllocatedBTreeMap<K, V> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            return;
        }

        test_assert!(
            self.length == 0 && self.root.is_none(),
            "`SelfAllocatedBTreeMap` should be empty on drop"
        );
    }
}

/// An iterator over the entries of a `BTreeMap`.
///
/// This `struct` is created by the [`iter`] method on [`SelfAllocatedBTreeMap`]. See its
/// documentation for more.
///
/// [`iter`]: SelfAllocatedBTreeMap::iter
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub(crate) struct Iter<'a, K: 'a, V: 'a> {
    range: LazyLeafRange<marker::Immut<'a>, K, V>,
    length: usize,
}

impl<K: Debug, V: Debug> Debug for Iter<'_, K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.clone()).finish()
    }
}

impl<'a, K: 'a, V: 'a> Default for Iter<'a, K, V> {
    /// Creates an empty `btree_map::Iter`.
    ///
    /// ```
    /// # use std::collections::btree_map;
    /// let iter: btree_map::Iter<'_, u8, u8> = Default::default();
    /// assert_eq!(iter.len(), 0);
    /// ```
    fn default() -> Self {
        Iter { range: Default::default(), length: 0 }
    }
}

impl<K, V> SelfAllocatedBTreeMap<K, V> {
    /// Makes a new, empty `BTreeMap`.
    ///
    /// Does not allocate anything on its own.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::BTreeMap;
    ///
    /// let mut map = BTreeMap::new();
    ///
    /// // entries can now be inserted into the empty map
    /// map.insert(1, "a");
    /// ```
    #[inline]
    #[must_use]
    pub(crate) const fn new() -> SelfAllocatedBTreeMap<K, V> {
        SelfAllocatedBTreeMap {
            root: None,
            length: 0,
            allocated_memory: SelfAllocatedMemory::new()
        }
    }

    fn into_iter<'state, A: AllocatorForSelfAllocations>(
        self,
        allocator: &'state A,
        maybe_state: Option<&'state mut A::State>
    ) -> IntoIter<'state, K, V, A> {
        let mut me = ManuallyDrop::new(self);
        if let Some(root) = me.root.take() {
            let full_range = root.into_dying().full_range();

            IntoIter {
                range: full_range,
                length: me.length,
                allocator,
                maybe_state,
            }
        } else {
            IntoIter {
                range: LazyLeafRange::none(),
                length: 0,
                allocator,
                maybe_state,
            }
        }
    }

    /// Clears the map, removing all elements.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::BTreeMap;
    ///
    /// let mut a = BTreeMap::new();
    /// a.insert(1, "a");
    /// a.clear();
    /// assert!(a.is_empty());
    /// ```
    pub(crate) fn clear<A: AllocatorForSelfAllocations>(
        &mut self,
        allocator: &A,
        maybe_state: Option<&mut A::State>
    ) {
        // avoid moving the allocator
        let prev = SelfAllocatedBTreeMap {
            root: mem::replace(&mut self.root, None),
            length: mem::replace(&mut self.length, 0),
            allocated_memory: mem::replace(&mut self.allocated_memory, SelfAllocatedMemory::new()),
        };

        drop(prev.into_iter(allocator, maybe_state))
    }

    /// Removes a key from the map, returning the stored key and value if the key
    /// was previously in the map.
    ///
    /// The key may be any borrowed form of the map's key type, but the ordering
    /// on the borrowed form *must* match the ordering on the key type.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::BTreeMap;
    ///
    /// let mut map = BTreeMap::new();
    /// map.insert(1, "a");
    /// assert_eq!(map.remove_entry(&1), Some((1, "a")));
    /// assert_eq!(map.remove_entry(&1), None);
    /// ```
    pub(crate) fn remove_entry<A: AllocatorForSelfAllocations, Q: ?Sized>(
        &mut self,
        key: &Q,
        allocator: &A,
        maybe_state: Option<&mut A::State>
    ) -> Option<(K, V)>
    where
        K: Borrow<Q> + Ord,
        Q: Ord,
    {
        let (map, dormant_map) = DormantMutRef::new(self);
        let root_node = map.root.as_mut()?.borrow_mut();
        match root_node.search_tree(key) {
            Found(handle) => Some(
                OccupiedEntry {
                    handle,
                    dormant_map,
                    _marker: PhantomData,
                }
                    .remove_entry(&mut map.allocated_memory, allocator, maybe_state),
            ),
            GoDown(_) => None,
        }
    }

    /// Removes a key from the map, returning the value at the key if the key
    /// was previously in the map.
    ///
    /// The key may be any borrowed form of the map's key type, but the ordering
    /// on the borrowed form *must* match the ordering on the key type.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::BTreeMap;
    ///
    /// let mut map = BTreeMap::new();
    /// map.insert(1, "a");
    /// assert_eq!(map.remove(&1), Some("a"));
    /// assert_eq!(map.remove(&1), None);
    /// ```
    pub(crate) fn remove<A: AllocatorForSelfAllocations, Q: ?Sized>(
        &mut self,
        key: &Q,
        allocator: &A,
        maybe_state: Option<&mut A::State>
    ) -> Option<V>
    where
        K: Borrow<Q> + Ord,
        Q: Ord,
    {
        self.remove_entry(key, allocator, maybe_state).map(|(_, v)| v)
    }

    /// Gets the given key's corresponding entry in the map for in-place manipulation.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::BTreeMap;
    ///
    /// let mut count: BTreeMap<&str, usize> = BTreeMap::new();
    ///
    /// // count the number of occurrences of letters in the vec
    /// for x in ["a", "b", "a", "c", "a", "b"] {
    ///     count.entry(x).and_modify(|curr| *curr += 1).or_insert(1);
    /// }
    ///
    /// assert_eq!(count["a"], 3);
    /// assert_eq!(count["b"], 2);
    /// assert_eq!(count["c"], 1);
    /// ```
    pub(crate) fn entry(&mut self, key: K) -> Entry<'_, K, V>
    where
        K: Ord,
    {
        let (map, dormant_map) = DormantMutRef::new(self);
        match map.root {
            None => Vacant(VacantEntry {
                key,
                handle: None,
                dormant_map,
                _marker: PhantomData,
            }),
            Some(ref mut root) => match root.borrow_mut().search_tree(&key) {
                Found(handle) => Occupied(OccupiedEntry {
                    handle,
                    dormant_map,
                    _marker: PhantomData,
                }),
                GoDown(handle) => Vacant(VacantEntry {
                    key,
                    handle: Some(handle),
                    dormant_map,
                    _marker: PhantomData,
                }),
            },
        }
    }

    pub(crate) fn self_allocated_memory(&self) -> impl Iterator<Item = (usize, u32)> {
        self.allocated_memory.0.into_iter()
    }

    pub(crate) unsafe fn force_become_null(&mut self) {
        self.length = 0;
        self.root = None;
        self.allocated_memory = SelfAllocatedMemory::new();
    }
}

impl<'a, K: 'a, V: 'a> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<(&'a K, &'a V)> {
        if self.length == 0 {
            None
        } else {
            self.length -= 1;
            Some(unsafe { self.range.next_unchecked() })
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.length, Some(self.length))
    }

    fn last(mut self) -> Option<(&'a K, &'a V)> {
        self.next_back()
    }

    fn min(mut self) -> Option<(&'a K, &'a V)>
    where
        (&'a K, &'a V): Ord,
    {
        self.next()
    }

    fn max(mut self) -> Option<(&'a K, &'a V)>
    where
        (&'a K, &'a V): Ord,
    {
        self.next_back()
    }
}

impl<K, V> FusedIterator for Iter<'_, K, V> {}

impl<'a, K: 'a, V: 'a> DoubleEndedIterator for Iter<'a, K, V> {
    fn next_back(&mut self) -> Option<(&'a K, &'a V)> {
        if self.length == 0 {
            None
        } else {
            self.length -= 1;
            Some(unsafe { self.range.next_back_unchecked() })
        }
    }
}

impl<K, V> ExactSizeIterator for Iter<'_, K, V> {
    fn len(&self) -> usize {
        self.length
    }
}

impl<K, V> Clone for Iter<'_, K, V> {
    fn clone(&self) -> Self {
        Iter { range: self.range.clone(), length: self.length }
    }
}

/// An owning iterator over the entries of a `BTreeMap`, sorted by key.
///
/// This `struct` is created by the [`into_iter`] method on [`BTreeMap`]
/// (provided by the [`IntoIterator`] trait). See its documentation for more.
///
/// [`into_iter`]: IntoIterator::into_iter
pub struct IntoIter<
    'state,
    K,
    V,
    A: AllocatorForSelfAllocations
> {
    range: LazyLeafRange<marker::Dying, K, V>,
    length: usize,
    allocator: &'state A,
    maybe_state: Option<&'state mut A::State>,
}

impl<'state, K, V, A: AllocatorForSelfAllocations> IntoIter<'state, K, V, A> {
    /// Returns an iterator of references over the remaining items.
    #[inline]
    pub(super) fn iter(&self) -> Iter<'_, K, V> {
        Iter { range: self.range.reborrow(), length: self.length }
    }
}

impl<'state, K, V, A: AllocatorForSelfAllocations> Drop for IntoIter<'state, K, V, A> {
    fn drop(&mut self) {
        while let Some(kv) = self.dying_next() {
            // SAFETY: we don't touch the tree before consuming the dying handle.
            unsafe { kv.drop_key_val() };
        }
    }
}

impl<'state, K, V, A: AllocatorForSelfAllocations> IntoIter<'state, K, V, A> {
    /// Core of a `next` method returning a dying KV handle,
    /// invalidated by further calls to this function and some others.
    fn dying_next(
        &mut self,
    ) -> Option<NodeHandle<NodeRef<marker::Dying, K, V, marker::LeafOrInternal>, marker::KV>> {
        let mut plug = usize::MAX; // The real allocated memory number is zero before
        // the first call of this method
        if self.length == 0 {
            self.range.deallocating_end(&mut plug, self.allocator, self.maybe_state.as_deref_mut());
            None
        } else {
            self.length -= 1;
            Some(unsafe { self.range.deallocating_next_unchecked(&mut plug, self.allocator, self.maybe_state.as_deref_mut()) })
        }
    }

    /// Core of a `next_back` method returning a dying KV handle,
    /// invalidated by further calls to this function and some others.
    fn dying_next_back(
        &mut self,
    ) -> Option<NodeHandle<NodeRef<marker::Dying, K, V, marker::LeafOrInternal>, marker::KV>> {
        let mut plug = usize::MAX; // The real allocated memory number is zero before
        // the first call of this method
        if self.length == 0 {
            self.range.deallocating_end(&mut plug, self.allocator, self.maybe_state.as_deref_mut());
            None
        } else {
            self.length -= 1;
            Some(unsafe { self.range.deallocating_next_back_unchecked(&mut plug, self.allocator, self.maybe_state.as_deref_mut()) })
        }
    }
}

impl<'state, K, V, A: AllocatorForSelfAllocations> Iterator for IntoIter<'state, K, V, A> {
    type Item = (K, V);

    fn next(&mut self) -> Option<(K, V)> {
        // SAFETY: we consume the dying handle immediately.
        self.dying_next().map(unsafe { |kv| kv.into_key_val() })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.length, Some(self.length))
    }
}

impl<'state, K, V, A: AllocatorForSelfAllocations> DoubleEndedIterator for IntoIter<'state, K, V, A> {
    fn next_back(&mut self) -> Option<(K, V)> {
        // SAFETY: we consume the dying handle immediately.
        self.dying_next_back().map(unsafe { |kv| kv.into_key_val() })
    }
}

impl<'state, K, V, A: AllocatorForSelfAllocations> ExactSizeIterator for IntoIter<'state, K, V, A> {
    fn len(&self) -> usize {
        self.length
    }
}

impl<'state, K, V, A: AllocatorForSelfAllocations> FusedIterator for IntoIter<'state, K, V, A> {}

impl<K, V> SelfAllocatedBTreeMap<K, V> {
    /// Gets an iterator over the entries of the map, sorted by key.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::BTreeMap;
    ///
    /// let mut map = BTreeMap::new();
    /// map.insert(3, "c");
    /// map.insert(2, "b");
    /// map.insert(1, "a");
    ///
    /// for (key, value) in map.iter() {
    ///     println!("{key}: {value}");
    /// }
    ///
    /// let (first_key, first_value) = map.iter().next().unwrap();
    /// assert_eq!((*first_key, *first_value), (1, "a"));
    /// ```
    pub(crate) fn iter(&self) -> Iter<'_, K, V> {
        if let Some(root) = &self.root {
            let full_range = root.reborrow().full_range();

            Iter { range: full_range, length: self.length }
        } else {
            Iter { range: LazyLeafRange::none(), length: 0 }
        }
    }

    /// Returns the number of elements in the map.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::BTreeMap;
    ///
    /// let mut a = BTreeMap::new();
    /// assert_eq!(a.len(), 0);
    /// a.insert(1, "a");
    /// assert_eq!(a.len(), 1);
    /// ```
    #[must_use]
    pub(crate) const fn len(&self) -> usize {
        self.length
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) const fn is_empty(&self) -> bool {
        self.length == 0
    }
}

#[cfg(test)]
mod tests {
    use crate::thread_local_allocator::ThreadLocalAllocatorFake;
    use super::*;

    #[test]
    fn basics() {
        let allocator = ThreadLocalAllocatorFake::new();
        let mut map = SelfAllocatedBTreeMap::new();

        assert!(map.is_empty());
        assert_eq!(map.len(), 0);

        for i in 0..10_000 {
            *map.entry(i).or_insert(i, &allocator, None).unwrap() *= 2;
            *map.entry(i).or_insert(i, &allocator, None).unwrap() *= 2;

            assert_eq!(*map.entry(i).or_insert(1, &allocator, None).unwrap(), i * 4);
        }

        assert!(!map.is_empty());
        assert_eq!(map.len(), 10_000);

        for (k, v) in map.iter() {
            assert_eq!(*v, k * 4);
        }

        map.clear(&allocator, None);

        assert!(map.is_empty());
        assert_eq!(map.len(), 0);

        assert_eq!(allocator.allocated_memory(), 0);
    }
}