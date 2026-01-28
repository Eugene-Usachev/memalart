use std::ptr;
use crate::sizes::{large_size_to_id_and_real_size, split_large_chunk_to_common, CommonRawChunk, LargeRawChunk, COMMON_CHUNKS_IN_LARGE_CHUNK, COMMON_CHUNK_SIZE, COMMON_SIZE_MAX, LARGE_CHUNK_SIZE, LARGE_SIZE_MAX};
use std::ptr::NonNull;
use orengine_utils::ArrayBuffer;
use orengine_utils::hints::{cold_path, unlikely, unreachable_hint, unwrap_or_bug_hint};
use crate::{NonLocalPtrManager, ThreadLocalAllocator};
use crate::non_local_ptr_manager::OffsetToSlotInTable;
use crate::AllocatorForSelfAllocations;
use crate::thread_local_allocator::numbers_of_used_objects_with_size::{UsedObjectsFixed, UsedObjectsMutSlice};
use crate::region::{alloc_region, parse_region, RawRegion, MAX_COMMON_CHUNKS_IN_REGION, LARGE_CHUNKS_IN_REGION, alloc_region_without_counting_limit, init_metadata_for_common_chunk, init_metadata_for_large_chunk, chunk_ptr_to_chunk_metadata_ptr};
use crate::sizes::{common_size_to_index, SizeClass};
use crate::test_assert::test_assert;

pub(super) struct UsedCommonChunk {
    pub(super) size: usize,
    pub(super) ptr: NonNull<CommonRawChunk>,
    pub(super) number_of_free_slots: usize,
}

pub(super) struct UsedLargeChunk {
    pub(super) size: usize,
    pub(super) ptr: NonNull<LargeRawChunk>,
    pub(super) number_of_free_slots: usize,
}

/// The key idea:
/// when we need to allocate the memory,
/// we need to take an object from a vector with this size objects,
/// and the `State` is used when the allocator does not contain the vector.
/// Because we need to allocate the vector for the allocator, it is impossible to use the allocator
/// itself as it needs to allocate the vector,
/// and this allocation is a new object to allocate and so on.
///
/// `State` contains the raw region of memory,
/// and it will not use the allocator to allocate all objects.
/// It gets a chunk for the size (for <1024 it takes a common chunk, for <64KB — a large one),
/// and stores it in itself not to call the allocator.
///
/// After it finally allocates all memory (`State` is used in, for example, `alloc_common_slow`
/// that needs more than one allocation), the caller should commit all the `State's` changes.
// TODO pub(crate)
pub struct State {
    /// A pointer to the underlying region of memory.
    /// It is used to parse the region and to store the region
    /// to deallocation at the end of the program.
    region_ptr: NonNull<RawRegion>,
    /// Free common chunks in the region after splitting a large one.
    free_common_chunks: ArrayBuffer<NonNull<CommonRawChunk>, COMMON_CHUNKS_IN_LARGE_CHUNK>,
    /// Free large chunks in the region.
    free_large_chunks: ArrayBuffer<NonNull<LargeRawChunk>, LARGE_CHUNKS_IN_REGION>,
    /// It contains already allocated common chunks, they will be return to the allocator later,
    /// but now it can be used for allocating from the `State`.
    used_common_chunks: ArrayBuffer<UsedCommonChunk, MAX_COMMON_CHUNKS_IN_REGION>,
    /// It contains already allocated large chunks, they will be return to the allocator later,
    /// but now it can be used for allocating from the `State`.
    used_large_chunks: ArrayBuffer<UsedLargeChunk, LARGE_CHUNKS_IN_REGION>,
    /// If the region is over while we need more memory, it will be used to allocate a new region.
    /// It is very unlikely.
    next_state: Option<NonNull<State>>,
    /// The first `State` in the linked list of states.
    /// `None` if the current `State` is the first.
    first_state: Option<NonNull<State>>,
    is_allocating_non_local_ptr_manager_now: bool,
}

impl State {
    fn from_region(
        region: NonNull<RawRegion>,
        first_state: Option<NonNull<State>>
    ) -> Self {
        let parsed_region = parse_region(region);

        let mut free_common_chunks = ArrayBuffer::new();
        unsafe {
            free_common_chunks.refill_with(|arr| {
                ptr::copy_nonoverlapping(
                    parsed_region.common_chunks.as_ptr(),
                    arr.as_mut_ptr().cast(),
                    parsed_region.common_chunks.len()
                );

                parsed_region.common_chunks.len()
            });
        };

        let mut free_large_chunks = ArrayBuffer::new();
        unsafe {
            free_large_chunks.refill_with(|arr| {
                ptr::copy_nonoverlapping(
                    parsed_region.large_chunks.as_ptr(),
                    arr.as_mut_ptr().cast(),
                    parsed_region.large_chunks.len()
                );

                parsed_region.large_chunks.len()
            });
        };

        Self {
            region_ptr: region,
            free_common_chunks,
            free_large_chunks,
            used_common_chunks: ArrayBuffer::new(),
            used_large_chunks: ArrayBuffer::new(),
            next_state: None,
            first_state,
            is_allocating_non_local_ptr_manager_now: false
        }
    }

    pub(super) fn is_processed(&self) -> bool {
        self.next_state.is_none()
            && self.used_large_chunks.is_empty()
            && self.used_common_chunks.is_empty()
            && self.free_common_chunks.is_empty()
            && self.free_large_chunks.is_empty()
    }

    pub(super) fn region_ptr(&self) -> NonNull<RawRegion> {
        self.region_ptr
    }

    pub(super) fn has_next_state(&self) -> bool {
        self.next_state.is_some()
    }

    pub(super) fn free_common_chunks_len(&self) -> usize {
        self.free_common_chunks.len()
    }

    pub(super) fn free_large_chunks_len(&self) -> usize {
        self.free_large_chunks.len()
    }

    pub(super) fn used_common_chunks_len(&self) -> usize {
        self.used_common_chunks.len()
    }

    pub(super) fn used_large_chunks_len(&self) -> usize {
        self.used_large_chunks.len()
    }

    pub(super) fn clear_free_common_chunks(&mut self, mut f: impl FnMut(&[NonNull<CommonRawChunk>])) {
        f(&*self.free_common_chunks);

        self.free_common_chunks.clear();
    }

    pub(super) fn clear_free_large_chunks(&mut self, mut f: impl FnMut(&[NonNull<LargeRawChunk>])) {
        f(&*self.free_large_chunks);

        self.free_large_chunks.clear();
    }

    pub(crate) fn for_each_used_common_size_number_of_free_slots_chunk_with_state(
        &mut self, mut f: impl FnMut(&mut Self, usize, usize)
    ) {
        for i in 0..self.used_common_chunks.len() {
            let used_chunk = unsafe { self.used_common_chunks.get_unchecked(i) };
            let size = used_chunk.size;
            let number_of_free_slots = used_chunk.number_of_free_slots;

            f(self, size, number_of_free_slots);
        }
    }

    pub(crate) fn for_each_used_large_size_number_of_free_slots_chunk_with_state(
        &mut self, mut f: impl FnMut(&mut Self, usize, usize)
    ) {
        for i in 0..self.used_large_chunks.len() {
            let used_chunk = unsafe { self.used_large_chunks.get_unchecked(i) };
            let size = used_chunk.size;
            let number_of_free_slots = used_chunk.number_of_free_slots;

            f(self, size, number_of_free_slots);
        }
    }

    pub(super) fn set_is_allocating_non_local_ptr_manager_now(&mut self, value: bool) {
        self.is_allocating_non_local_ptr_manager_now = value;
    }

    pub(super) fn take_next_state(&mut self) -> Option<NonNull<State>> {
        self.next_state.take()
    }

    fn alloc_new_state_in<const WITH_COUNTING_LIMIT: bool>(
        mut first_state: NonNull<Self>
    ) -> Option<NonNull<Self>> {
        const SIZE_OF_STATE: usize = size_of::<State>();
        const SIZE_CALL_OF_STATE: SizeClass = SizeClass::define(SIZE_OF_STATE);

        let mut state = Self::from_region(unsafe {
            if WITH_COUNTING_LIMIT {
                NonNull::new(alloc_region())?
            } else {
                NonNull::new(alloc_region_without_counting_limit())?
            }
        }, Some(first_state));

        // Try to allocate space for the new State inside the already existing state chain first
        let ptr_ = Self::try_alloc_size(
            unsafe { first_state.as_mut() },
            SIZE_CALL_OF_STATE, SIZE_OF_STATE
        );

        if let Some(ptr) = ptr_ {
            // Move the freshly created state into the allocated memory and return its pointer
            let state_ptr = ptr.cast::<State>();

            unsafe { state_ptr.as_ptr().write(state); }

            return Some(state_ptr);
        }

        // Otherwise, allocate space for the State within its own region, write it there, and return
        let ptr = Self::alloc_size::<WITH_COUNTING_LIMIT>(&mut state, SIZE_CALL_OF_STATE, SIZE_OF_STATE)?;
        let state_ptr = ptr.cast::<State>();

        unsafe { state_ptr.as_ptr().write(state); }

        Some(state_ptr)
    }

    fn get_free_large_chunk_<const WITH_COUNTING_LIMIT: bool>(
        &mut self,
    ) -> Option<(NonNull<State>, NonNull<LargeRawChunk>)> {
        if let Some(large_chunk) = self.free_large_chunks.pop() {
            return Some((NonNull::from(&mut *self), large_chunk));
        }

        let next_state_ = self.next_state.map(|mut ptr| unsafe { ptr.as_mut() });
        if let Some(next_state) = next_state_ {
            cold_path();

            return next_state.get_free_large_chunk_::<WITH_COUNTING_LIMIT>();
        }

        let first_state = self
            .first_state
            .unwrap_or_else(|| unsafe { NonNull::new_unchecked(self) });

        self.next_state = Some(Self::alloc_new_state_in::<WITH_COUNTING_LIMIT>(first_state)?);

        match self.next_state {
            Some(mut next_state) => {
                // Logically, the maximum recursion depth is 2 here
                unsafe { next_state.as_mut() }.get_free_large_chunk_::<WITH_COUNTING_LIMIT>()
            }
            None => unreachable_hint(),
        }
    }

    pub(crate) fn get_free_large_chunk<const WITH_COUNTING_LIMIT: bool>(
        &mut self,
    ) -> Option<NonNull<LargeRawChunk>> {
         self
             .get_free_large_chunk_::<WITH_COUNTING_LIMIT>()
             .map(|(_, free_large_chunk)| free_large_chunk)
        
        // TODO test
    }

    fn get_free_common_chunk<const WITH_COUNTING_LIMIT: bool>(
        &mut self,
    ) -> Option<(NonNull<State>, NonNull<CommonRawChunk>)> {
        if let Some(common_chunk) = self.free_common_chunks.pop() {
            return Some((NonNull::from(&mut *self), common_chunk));
        }

        if let Some(next_state) = self.next_state.map(|mut ptr| unsafe { ptr.as_mut() }) {
            cold_path();

            return next_state.get_free_common_chunk::<WITH_COUNTING_LIMIT>();
        }

        // Need to get a large chunk from somewhere (maybe next state), and then split it
        let (mut owner_state_ptr, large_chunk) = self.get_free_large_chunk_::<WITH_COUNTING_LIMIT>()?;
        let owner_state: &mut State = unsafe { owner_state_ptr.as_mut() };

        if owner_state.free_common_chunks.is_empty() {
            unsafe {
                owner_state.free_common_chunks.refill_with(|common_chunks| {
                    let new_chunks = split_large_chunk_to_common(large_chunk);

                    ptr::copy_nonoverlapping(
                        new_chunks.as_ptr(),
                        common_chunks.as_mut_ptr().cast(),
                        COMMON_CHUNKS_IN_LARGE_CHUNK
                    );

                    COMMON_CHUNKS_IN_LARGE_CHUNK
                });
            };
        } else {
            // The next state has been created after we check self.next_state.is_some().
            // It should have a common chunk.
            return owner_state.get_free_common_chunk::<WITH_COUNTING_LIMIT>();
        }

        Some((owner_state_ptr, unwrap_or_bug_hint(owner_state.free_common_chunks.pop()) ))
    }

    #[cold]
    pub(crate) fn alloc_size_slow<const WITH_COUNTING_LIMIT: bool>(
        &mut self,
        size_class: SizeClass,
        size: usize
    )-> Option<NonNull<u8>> {
        match size_class {
            SizeClass::Common => {
                let (mut owner_ptr, free_common_chunk) = self.get_free_common_chunk::<WITH_COUNTING_LIMIT>()?;
                let number_of_free_slots = (COMMON_CHUNK_SIZE / size) - 1; // because we take it now
                let used_common_chunk = UsedCommonChunk {
                    size,
                    ptr: free_common_chunk,
                    number_of_free_slots
                };

                unsafe {
                    owner_ptr
                        .as_mut()
                        .used_common_chunks.push_unchecked(used_common_chunk)
                };

                Some(unsafe { free_common_chunk.cast().byte_add(size * number_of_free_slots) })
            }
            SizeClass::Large => {
                let (mut owner_ptr, free_large_chunk) = self.get_free_large_chunk_::<WITH_COUNTING_LIMIT>()?;
                let number_of_free_slots = (LARGE_CHUNK_SIZE / size) - 1; // because we take it now
                let used_large_chunk = UsedLargeChunk {
                    size,
                    ptr: free_large_chunk,
                    number_of_free_slots
                };

                unsafe { owner_ptr.as_mut().used_large_chunks.push_unchecked(used_large_chunk) }

                Some(unsafe { free_large_chunk.cast().byte_add(size * number_of_free_slots) })
            }
            SizeClass::ExtraLarge => {
                unreachable_hint()
            }
        }
    }

    fn try_alloc_size(state: &mut Self, size_class: SizeClass, size: usize) -> Option<NonNull<u8>> {
        // Traverse the current state and its linked next states to find a used chunk
        // with the requested object size and at least one free slot.
        let mut curr: Option<&mut State> = Some(state);

        while let Some(s) = curr {
            match size_class {
                SizeClass::Common => {
                    #[cfg(test)]
                    {
                        assert_eq!(size % 8, 0);
                    }

                    for used_chunk in s.used_common_chunks.iter_mut() {
                        if used_chunk.size != size || used_chunk.number_of_free_slots == 0 {
                            continue;
                        }

                        used_chunk.number_of_free_slots -= 1;

                        let offset = size * used_chunk.number_of_free_slots;

                        return Some(unsafe { used_chunk.ptr.cast::<u8>().byte_add(offset) });
                    }
                }
                SizeClass::Large => {
                    #[cfg(test)]
                    {
                        assert_eq!(
                            large_size_to_id_and_real_size(size).1,
                            size
                        );
                    }

                    for used_chunk in s.used_large_chunks.iter_mut() {
                        if used_chunk.size != size || used_chunk.number_of_free_slots == 0 {
                            continue;
                        }

                        used_chunk.number_of_free_slots -= 1;

                        let offset = size * used_chunk.number_of_free_slots;

                        return Some(unsafe { used_chunk.ptr.cast::<u8>().byte_add(offset) });
                    }
                }
                SizeClass::ExtraLarge => unreachable_hint(),
            }

            curr = s
                .next_state
                .map(|mut p| unsafe { p.as_mut() });
        }

        None
    }

    pub(crate) fn alloc_size<const WITH_COUNTING_LIMIT: bool>(
        state: &mut Self,
        size_class: SizeClass,
        size: usize
    ) -> Option<NonNull<u8>> {
        if let Some(ptr) = Self::try_alloc_size(state, size_class, size) {
            return Some(ptr);
        }

        state.alloc_size_slow::<WITH_COUNTING_LIMIT>(size_class, size)
    }
}

#[cfg(test)]
impl Drop for State {
    fn drop(&mut self) {
        if cfg!(test) {
            if std::thread::panicking() {
                return;
            }

            assert!(self.is_processed(), "The state was not processed but was dropped");
        }
    }
}

impl<NLPM: NonLocalPtrManager> ThreadLocalAllocator<NLPM> {
    // region try_alloc_from_cache

    fn try_alloc_common_from_cache(
        &self,
        size: usize,
        maybe_state: Option<&mut State>
    ) -> Option<NonNull<u8>> {
        test_assert!(size <= COMMON_SIZE_MAX as usize);

        let vec = self.common_chunk_object_vector_for_size(size);

        let res = vec.pop();
        if let Some(ptr) = res {
            return Some(ptr);
        }
        
        if let Some(state) = maybe_state {
            if !state.is_allocating_non_local_ptr_manager_now {
                unsafe {
                    self.alloc_common_slow::<false>(
                        size,
                        |inner| {
                            inner.chunk_manager.try_get_common_chunk()
                        },
                        Some(state)
                    )
                }
            } else {
                unsafe {
                    state.is_allocating_non_local_ptr_manager_now = false;

                    self.alloc_common_slow::<true>(
                        size,
                        |inner| {
                            inner.chunk_manager.try_get_common_chunk()
                        },
                        Some(state)
                    )
                }
            }
        } else {
            unsafe {
                self.alloc_common_slow::<false>(
                    size,
                    |inner| {
                        inner.chunk_manager.try_get_common_chunk()
                    },
                    maybe_state
                )
            }
        }
    }

    fn try_alloc_large_from_cache(
        &self,
        size: usize,
        maybe_state: Option<&mut State>
    ) -> Option<NonNull<u8>> {
        test_assert!(size > COMMON_SIZE_MAX as usize);
        test_assert!(size <= LARGE_SIZE_MAX);

        let (id, _real_size) = large_size_to_id_and_real_size(size);
        let inner = self.get_inner();
        
        let vec = inner.large_object_table.get_vec_mut_by_id(id);
        if let Some(ptr) = vec.pop() {
            return Some(ptr);
        }

        if let Some(state) = maybe_state {
            if !state.is_allocating_non_local_ptr_manager_now {
                unsafe {
                    self.alloc_large_slow::<false>(
                        id,
                        size,
                        |inner| {
                            inner.chunk_manager.try_get_large_chunk()
                        },
                        Some(state)
                    )
                }
            } else {
                state.is_allocating_non_local_ptr_manager_now = false;

                unsafe {
                    self.alloc_large_slow::<true>(
                        id,
                        size,
                        |inner| {
                            inner.chunk_manager.try_get_large_chunk()
                        },
                        Some(state)
                    )
                }
            }
        } else {
            unsafe {
                self.alloc_large_slow::<false>(
                    id,
                    size,
                    |inner| {
                        inner.chunk_manager.try_get_large_chunk()
                    },
                    maybe_state
                )
            }
        }
    }

    // endregion

    #[cold]
    #[inline(never)]
    fn slow_self_alloc_<const WITH_COUNTING_LIMIT: bool>(
        &self,
        size: usize,
        size_class: SizeClass,
        maybe_state: Option<&mut State>
    )-> Option<NonNull<u8>> {
        match maybe_state {
            Some(state) => {
                State::alloc_size::<WITH_COUNTING_LIMIT>(state, size_class, size)
            }
            None => {
                let region = NonNull::new(if WITH_COUNTING_LIMIT {
                    unsafe { alloc_region() }
                } else {
                    unsafe { alloc_region_without_counting_limit() }
                })?;

                let mut state = Self::state_from_region(region);
                let res = State::alloc_size::<WITH_COUNTING_LIMIT>(&mut state, size_class, size);

                self.handle_state(state);

                res
            }
        }
    }

    fn self_alloc_<const WITH_COUNTING_LIMIT: bool>(
        &self,
        size: usize,
        mut maybe_state: Option<&mut State>
    ) -> Option<NonNull<u8>> {
        let size_class = SizeClass::define(size);
        let ptr = match size_class {
            SizeClass::Common => self.try_alloc_common_from_cache(
                size,
                maybe_state.as_deref_mut()
            ),
            SizeClass::Large => self.try_alloc_large_from_cache(
                size,
                maybe_state.as_deref_mut()
            ),
            SizeClass::ExtraLarge => return unsafe { self.alloc_extra_large(size) },
        };

        if let Some(ptr) = ptr {
            return Some(ptr);
        }

        self.slow_self_alloc_::<WITH_COUNTING_LIMIT>(
            size,
            size_class,
            maybe_state.as_deref_mut()
        )
    }

    fn self_dealloc_common_with_state(
        &self,
        ptr: NonNull<u8>,
        size: usize,
        state: &mut State
    ) {
        test_assert!(size <= COMMON_SIZE_MAX as usize, "size: {size}");

        let idx = common_size_to_index(size as u16);
        let object_vec = unwrap_or_bug_hint(self.get_inner().common_object_table.get_mut(idx));

        if unlikely(object_vec.push_without_counting_limit(ptr, self, Some(state)).is_err()) {
            panic!("Failed to return the pointer back.");
        }

        return;
    }

    fn self_dealloc_large_with_state(
        &self,
        ptr: NonNull<u8>,
        size: usize,
        state: &mut State
    ) {
        test_assert!(size > COMMON_SIZE_MAX as usize, "size: {size}");
        test_assert!(size <= LARGE_SIZE_MAX, "size: {size}");

        let (id, _real_size) = large_size_to_id_and_real_size(size);
        let object_vec = self
            .get_inner()
            .large_object_table
            .get_vec_mut_by_id(id);

        if unlikely(object_vec.push_without_counting_limit(ptr, self, Some(state)).is_err()) {
            panic!("Failed to return the pointer back.");
        }

        return;
    }

    #[inline(never)]
    pub(super) fn handle_state(&self, mut state: <Self as AllocatorForSelfAllocations>::State) {
        // We don't want to allocate anything on heap in this function.
        // Here we can use stack instead of using HashMaps on heap.
        fn count_all_large_used_chunks(
            chunks: &ArrayBuffer<UsedLargeChunk, LARGE_CHUNKS_IN_REGION>
        ) -> UsedObjectsFixed<LARGE_CHUNKS_IN_REGION> {
            let mut arr = UsedObjectsFixed::new();
            for chunk in chunks.iter() {
                arr.write_used_number_and_size(chunk.size, 1);
            }

            arr
        }

        // We don't want to allocate anything on heap in this function.
        // Here we can use stack instead of using HashMaps on heap.
        fn count_all_common_used_chunks(
            chunks: &ArrayBuffer<UsedCommonChunk, MAX_COMMON_CHUNKS_IN_REGION>
        ) -> UsedObjectsFixed<MAX_COMMON_CHUNKS_IN_REGION> {
            let mut arr = UsedObjectsFixed::new();
            for chunk in chunks.iter() {
                arr.write_used_number_and_size(chunk.size, 1);
            }

            arr
        }

        let inner = self.get_inner();
        let mut to_dealloc = ArrayBuffer::<_, 8>::new();
        if unlikely(!inner.is_destructor_registered) {
            self.get_or_init_and_get_non_local_ptr_manager_ptr(Some(&mut state));
        }

        loop {
            loop {
                let mut did_allocate_more = false;

                state.for_each_used_common_size_number_of_free_slots_chunk_with_state(
                    |state, size, number_of_free_slots| {
                        test_assert!(size <= COMMON_SIZE_MAX as usize);

                        let idx = common_size_to_index(size as u16);
                        let object_vec = unsafe {
                            inner.common_object_table.get_unchecked_mut(idx)
                        };

                        let res = object_vec
                            .reserve_without_counting_limit(number_of_free_slots as u32, self, Some(state));

                        match res {
                            Ok(did_allocate_more_) => {
                                did_allocate_more |= did_allocate_more_;
                            }
                            Err(_) => {
                                panic!("Failed to allocate an object table, a memory leak occurred");
                            }
                        }
                    }
                );

                state.for_each_used_large_size_number_of_free_slots_chunk_with_state(
                    |state, size, number_of_free_slots| {
                        test_assert!(size > COMMON_SIZE_MAX as usize);
                        test_assert!(size <= LARGE_SIZE_MAX);

                        let (id, _) = large_size_to_id_and_real_size(size);
                        let object_vec = inner.large_object_table.get_vec_mut_by_id(id);
                        let res = object_vec
                            .reserve_without_counting_limit(number_of_free_slots as u32, self, Some(state));

                        match res {
                            Ok(did_allocate_more_) => {
                                did_allocate_more |= did_allocate_more_;
                            }
                            Err(_) => {
                                panic!("Failed to allocate an object table, a memory leak occurred");
                            }
                        }
                    }
                );

                for (number_, size) in count_all_large_used_chunks(&mut state.used_large_chunks).as_slice().as_slice().iter().copied() {
                    let Some(number) = number_ else { break };
                    let (id, _) = large_size_to_id_and_real_size(size);
                    let idx = inner.large_object_table.get_idx_by_id(id);

                    match self.large_chunk_table(Some(&mut state))[idx].reserve(number.get(), self, Some(&mut state)) {
                        Ok(did_allocate) => did_allocate_more |= did_allocate,
                        Err(_) => {
                            panic!("Failed to allocate a large chunk table, a memory leak occurred");
                        }
                    }
                }

                for (number_, size) in count_all_common_used_chunks(&mut state.used_common_chunks).as_slice().as_slice().iter().copied() {
                    let Some(number) = number_ else { break };
                    let idx = common_size_to_index(size as u16);

                    match self.common_chunk_table(Some(&mut state))[idx].reserve(number.into(), self, Some(&mut state)) {
                        Ok(did_allocate) => did_allocate_more |= did_allocate,
                        Err(_) => {
                            panic!("Failed to allocate a common chunk table, a memory leak occurred");
                        }
                    }
                }

                did_allocate_more |= inner.chunk_manager.prepare_to_handle_state_and_return_if_allocated_more(self, &mut state);

                if !did_allocate_more {
                    break;
                }
            }

            let non_local_ptr_manager = self.get_or_init_and_get_non_local_ptr_manager_ptr(Some(&mut state));
            let common_chunk_table = self.common_chunk_table(Some(&mut state));

            state.used_common_chunks.clear_with(|used_common_chunk| {
                let idx = common_size_to_index(used_common_chunk.size as u16);
                let object_table = unsafe {
                    inner.common_object_table.get_unchecked_mut(idx)
                };
                
                let start: NonNull<u8> = used_common_chunk.ptr.cast();
                
                for i in 0..used_common_chunk.number_of_free_slots {
                    unwrap_or_bug_hint(object_table.try_push(
                        unsafe { start.byte_add(i * used_common_chunk.size) }
                    ));
                }

                init_metadata_for_common_chunk(
                    non_local_ptr_manager,
                    OffsetToSlotInTable::new_for_common_chunk(idx),
                    used_common_chunk.ptr.cast(),
                );

                let common_chunk_table = unwrap_or_bug_hint(common_chunk_table.get_mut(idx));

                if cfg!(test) {
                    let already_contain = common_chunk_table
                        .iter()
                        .find(|chunk| used_common_chunk.ptr == **chunk);

                    assert!(already_contain.is_none(), "The chunk was already in the table");
                }

                unwrap_or_bug_hint(common_chunk_table.try_push(used_common_chunk.ptr));
            });

            let large_chunk_table = self.large_chunk_table(Some(&mut state));

            state.used_large_chunks.clear_with(|used_large_chunk| {
                let (id, _) = large_size_to_id_and_real_size(used_large_chunk.size);
                let idx = inner.large_object_table.get_idx_by_id(id);
                let object_table = inner
                    .large_object_table
                    .get_vec_mut_by_id(id);

                let start: NonNull<u8> = used_large_chunk.ptr.cast();

                init_metadata_for_large_chunk(
                    non_local_ptr_manager,
                    OffsetToSlotInTable::new_for_large_chunk(idx),
                    used_large_chunk.ptr.cast(),
                );

                for i in 0..used_large_chunk.number_of_free_slots {
                    unwrap_or_bug_hint(object_table.try_push(
                        unsafe { start.byte_add(i * used_large_chunk.size) }
                    ));
                }

                let large_chunk_table = unwrap_or_bug_hint(large_chunk_table.get_mut(idx));

                if cfg!(test) {
                    let already_contain = large_chunk_table
                        .iter()
                        .find(|chunk| used_large_chunk.ptr == **chunk);

                    assert!(already_contain.is_none(), "The chunk was already in the table");
                }

                unwrap_or_bug_hint(large_chunk_table.try_push(used_large_chunk.ptr));
            });

            let next_ = state.take_next_state();

            inner.chunk_manager.handle_state::<NLPM>(state);

            if let Some(next) = next_ {
                state = unsafe { next.read() };

                to_dealloc.push(next).expect("Too many states were allocated.");
            } else {
                break;
            }
        }

        for ptr in to_dealloc.iter().copied() {
            unsafe { self.dealloc(ptr.cast(), size_of::<State>()) };
        }
    }
}

impl<NLPM: NonLocalPtrManager> AllocatorForSelfAllocations for ThreadLocalAllocator<NLPM> {
    type State = State;

    fn state_from_region(region: NonNull<RawRegion>) -> Self::State {
        State::from_region(region, None)
    }

    fn self_alloc(
        &self,
        size: usize,
        maybe_state: Option<&mut Self::State>
    ) -> Option<NonNull<u8>>{
        self.self_alloc_::<true>(size, maybe_state)
    }

    fn self_alloc_without_counting_limit(
        &self,
        size: usize,
        maybe_state: Option<&mut Self::State>
    ) -> Option<NonNull<u8>> {
        self.self_alloc_::<false>(size, maybe_state)
    }

    unsafe fn self_dealloc(
        &self,
        ptr: NonNull<u8>,
        size: usize,
        maybe_state: Option<&mut Self::State>
    ) {
        match SizeClass::define(size) {
            SizeClass::Common => unsafe {
                match maybe_state {
                    Some(state) => {
                        self.self_dealloc_common_with_state(ptr, size, state);
                    }
                    None => {
                        self.dealloc_common(ptr, size)
                    }
                }
            },
            SizeClass::Large => unsafe {
                match maybe_state {
                    Some(state) => {
                        self.self_dealloc_large_with_state(ptr, size, state);
                    }
                    None => {
                        self.dealloc_large(ptr, size)
                    }
                }
            },
            SizeClass::ExtraLarge => unsafe { self.dealloc_extra_large(ptr, size) },
        }
    }
}

#[cfg(test)]
mod state_tests {
    use std::collections::BTreeSet;
    use std::mem;
    use super::*;
    use crate::region::{alloc_region, MAX_COMMON_CHUNKS_IN_REGION, LARGE_CHUNKS_IN_REGION, REGION_SIZE, REST_COMMON_CHUNKS_IN_REGION, dealloc_region};
    use crate::sizes::{COMMON_CHUNK_SIZE, COMMON_CHUNKS_IN_LARGE_CHUNK, LARGE_CHUNK_SIZE};
    use std::ptr::NonNull;
    use crate::test_utilities::acquire_test_lock;

    fn drop_test_state(mut state: State) {
        loop {
            let next = state.next_state.take();
            let region = mem::ManuallyDrop::new(state).region_ptr;

            match next {
                Some(next) => {
                    state = unsafe {
                        let next = next.read();

                        dealloc_region(region);

                        next
                    }
                }
                None => {
                    unsafe { dealloc_region(region) };

                    break
                }
            }
        }
    }

    #[test]
    fn test_from_region_new_state() {
        let test_lock = acquire_test_lock();

        let region = unsafe { NonNull::new(alloc_region()).expect("Failed to allocate region") };
        let state = State::from_region(region, None);

        assert_eq!(state.region_ptr, region);
        assert_eq!(state.free_large_chunks.len(), LARGE_CHUNKS_IN_REGION);
        assert_eq!(state.free_common_chunks.len(), REST_COMMON_CHUNKS_IN_REGION);
        assert!(state.used_common_chunks.is_empty());
        assert!(state.used_large_chunks.is_empty());
        assert!(state.next_state.is_none());
        assert!(state.first_state.is_none());

        drop_test_state(state);

        drop(test_lock);
    }

    #[test]
    fn test_from_region_with_first_state() {
        let test_lock = acquire_test_lock();

        let region1 = unsafe { NonNull::new(alloc_region()).expect("Failed to allocate region") };
        let mut first_state = State::from_region(region1, None);
        let first_state_ptr = NonNull::from(&mut first_state);

        assert!(first_state.next_state.is_none());

        let region2 = unsafe { NonNull::new(alloc_region()).expect("Failed to allocate region") };
        let state = State::from_region(region2, Some(first_state_ptr));

        assert_eq!(state.first_state, Some(first_state_ptr));
        assert!(state.next_state.is_none());

        drop_test_state(first_state);
        drop_test_state(state);

        drop(test_lock);
    }

    #[test]
    fn test_get_free_common_chunk_from_initial_pool() {
        let test_lock = acquire_test_lock();

        let region = unsafe { NonNull::new(alloc_region()).expect("Failed to allocate region") };
        let mut state = State::from_region(region, None);
        let initial_count = state.free_common_chunks.len();

        let result = state.get_free_common_chunk::<false>();

        assert!(result.is_some());

        let (owner_ptr, chunk_ptr) = result.unwrap();

        assert_eq!(owner_ptr.as_ptr(), &mut state as *mut _);
        assert_eq!(
            unsafe { chunk_ptr.as_ptr().byte_offset_from(state.region_ptr.as_ptr()) },
            (COMMON_CHUNK_SIZE * initial_count) as isize
        );
        assert_eq!(state.free_common_chunks.len(), initial_count - 1);

        drop_test_state(state);

        drop(test_lock);
    }

    #[test]
    fn test_get_free_common_chunk_requires_large_split() {
        let test_lock = acquire_test_lock();

        let region = unsafe { NonNull::new(alloc_region()).expect("Failed to allocate region") };
        let mut state = State::from_region(region, None);

        for _ in 0..REST_COMMON_CHUNKS_IN_REGION {
            let chunk = state.get_free_common_chunk::<false>();

            assert!(chunk.is_some());
        }

        let initial_large_count = state.free_large_chunks.len();
        let result = state.get_free_common_chunk::<false>();

        assert!(result.is_some());
        assert_eq!(state.free_large_chunks.len(), initial_large_count - 1);
        assert_eq!(state.free_common_chunks.len(), COMMON_CHUNKS_IN_LARGE_CHUNK - 1);
        assert_eq!(
            unsafe { result.unwrap().1.byte_offset_from(state.region_ptr) },
            (REGION_SIZE - COMMON_CHUNK_SIZE) as isize
        );

        drop_test_state(state);

        drop(test_lock);
    }

    #[test]
    fn test_get_free_large_chunk_simple() {
        let test_lock = acquire_test_lock();

        let region = unsafe { NonNull::new(alloc_region()).expect("Failed to allocate region") };
        let mut state = State::from_region(region, None);
        let initial_count = state.free_large_chunks.len();

        let result = state.get_free_large_chunk_::<false>();

        assert!(result.is_some());

        let (owner_ptr, chunk_ptr) = result.unwrap();

        assert_eq!(owner_ptr.as_ptr(), &mut state as *mut _);
        assert_eq!(
            unsafe { chunk_ptr.byte_offset_from(state.region_ptr) },
            (REGION_SIZE - LARGE_CHUNK_SIZE) as isize
        );
        assert_eq!(state.free_large_chunks.len(), initial_count - 1);

        drop_test_state(state);

        drop(test_lock);
    }

    #[test]
    fn test_get_free_common_chunk_multiple() {
        let test_lock = acquire_test_lock();

        let region = unsafe { NonNull::new(alloc_region()).expect("Failed to allocate region") };
        let mut state = State::from_region(region, None);

        for i in 0..REST_COMMON_CHUNKS_IN_REGION {
            let result = state.get_free_common_chunk::<false>();
            assert!(result.is_some());
            assert_eq!(state.free_common_chunks.len(), REST_COMMON_CHUNKS_IN_REGION - i - 1);
            assert_eq!(
                unsafe { result.unwrap().1.byte_offset_from(state.region_ptr) },
                (LARGE_CHUNK_SIZE - (i + 1) * COMMON_CHUNK_SIZE) as isize
            );
        }

        for i in 0..MAX_COMMON_CHUNKS_IN_REGION - REST_COMMON_CHUNKS_IN_REGION {
            let result = state.get_free_common_chunk::<false>();
            assert!(result.is_some());
            assert_eq!(
                state.free_common_chunks.len(),
                (MAX_COMMON_CHUNKS_IN_REGION - i) % COMMON_CHUNKS_IN_LARGE_CHUNK
            );
            assert_eq!(
                unsafe { result.unwrap().1.byte_offset_from(state.region_ptr) },
                (REGION_SIZE - (i + 1) * COMMON_CHUNK_SIZE) as isize
            );
        }

        drop_test_state(state);

        drop(test_lock);
    }

    #[test]
    fn test_get_free_large_chunk_multiple() {
        let test_lock = acquire_test_lock();

        let region = unsafe { NonNull::new(alloc_region()).expect("Failed to allocate region") };
        let mut state = State::from_region(region, None);

        for i in 0..LARGE_CHUNKS_IN_REGION {
            let result = state.get_free_large_chunk_::<false>();
            assert!(result.is_some());
            assert_eq!(state.free_large_chunks.len(), LARGE_CHUNKS_IN_REGION - i - 1);
            assert_eq!(
                unsafe { result.unwrap().1.byte_offset_from(state.region_ptr) },
                (REGION_SIZE - (i + 1) * LARGE_CHUNK_SIZE) as isize
            );
        }

        drop_test_state(state);

        drop(test_lock);
    }

    #[test]
    fn test_alloc_size_slow_common_new_chunk() {
        let test_lock = acquire_test_lock();

        let region = unsafe { NonNull::new(alloc_region()).expect("Failed to allocate region") };
        let mut state = State::from_region(region, None);
        let size = 32;
        let initial_common_free = state.free_common_chunks.len();

        let ptr = state.alloc_size_slow::<false>(SizeClass::Common, size);

        assert!(ptr.is_some());
        assert_eq!(state.used_common_chunks.len(), 1);
        assert_eq!(initial_common_free - 1, state.free_common_chunks.len());

        let used_chunk = state.used_common_chunks.get_mut(0).unwrap();
        assert_eq!(used_chunk.size, size);
        assert_eq!(used_chunk.number_of_free_slots, (COMMON_CHUNK_SIZE / size) - 1);

        drop_test_state(state);

        drop(test_lock);
    }

    #[test]
    fn test_alloc_size_slow_large_new_chunk() {
        let test_lock = acquire_test_lock();

        let region = unsafe { NonNull::new(alloc_region()).expect("Failed to allocate region") };
        let mut state = State::from_region(region, None);
        let size = 2048;
        let initial_large_free = state.free_large_chunks.len();

        let ptr = state.alloc_size_slow::<false>(SizeClass::Large, size);

        assert!(ptr.is_some());
        assert_eq!(state.used_large_chunks.len(), 1);
        assert_eq!(initial_large_free - 1, state.free_large_chunks.len());

        let used_chunk = &state.used_large_chunks[0];
        assert_eq!(used_chunk.size, size);
        assert_eq!(used_chunk.number_of_free_slots, (LARGE_CHUNK_SIZE / size) - 1);

        drop_test_state(state);

        drop(test_lock);
    }

    #[test]
    fn test_try_alloc_size_common_from_used_chunk() {
        let test_lock = acquire_test_lock();

        let region = unsafe { NonNull::new(alloc_region()).expect("Failed to allocate region") };
        let mut state = State::from_region(region, None);
        let size = 64;
        let mut ptrs = BTreeSet::new();

        // First allocation creates a used chunk
        let ptr1 = State::alloc_size::<false>(
            &mut state,
            SizeClass::Common,
            size
        );
        assert!(ptr1.is_some());

        ptrs.insert(ptr1.unwrap());

        let initial_used_chunks = state.used_common_chunks.len();

        for _ in 1..COMMON_CHUNK_SIZE / size {
            let ptr = State::try_alloc_size(&mut state, SizeClass::Common, size).expect("null was returned");

            assert!(ptrs.insert(ptr));
        }

        assert_eq!(state.used_common_chunks.len(), initial_used_chunks);

        let ptr = State::alloc_size::<false>(
            &mut state,
            SizeClass::Common,
            size
        ).expect("null was returned");

        assert!(ptrs.insert(ptr));

        assert_eq!(state.used_common_chunks.len(), initial_used_chunks + 1);

        drop_test_state(state);

        drop(test_lock);
    }

    #[test]
    fn test_try_alloc_size_large_from_used_chunk() {
        let test_lock = acquire_test_lock();

        let region = unsafe { NonNull::new(alloc_region()).expect("Failed to allocate region") };
        let mut state = State::from_region(region, None);
        let size = 2048;
        let mut ptrs = BTreeSet::new();

        // First allocation creates a used chunk
        let ptr1 = State::alloc_size::<false>(&mut state, SizeClass::Large, size);
        assert!(ptr1.is_some());

        ptrs.insert(ptr1.unwrap());

        let initial_used_chunks = state.used_large_chunks.len();

        for _ in 1..LARGE_CHUNK_SIZE / size {
            let ptr = State::try_alloc_size(&mut state, SizeClass::Large, size).expect("null was returned");

            unsafe { ptr.write_bytes(0, 1) };

            assert!(ptrs.insert(ptr));
        }

        assert_eq!(state.used_large_chunks.len(), initial_used_chunks);

        let ptr = State::alloc_size::<false>(&mut state, SizeClass::Large, size).expect("null was returned");

        unsafe { ptr.write_bytes(0, 1) };

        assert!(ptrs.insert(ptr));

        assert_eq!(state.used_large_chunks.len(), initial_used_chunks + 1);

        drop_test_state(state);

        drop(test_lock);
    }

    #[test]
    fn test_try_alloc_size_no_matching_size() {
        let test_lock = acquire_test_lock();

        let region = unsafe { NonNull::new(alloc_region()).expect("Failed to allocate region") };
        let mut state = State::from_region(region, None);
        let initial_chunks_len = state.free_common_chunks.len();

        // Allocate with size 32
        let _ = State::alloc_size::<false>(&mut state, SizeClass::Common, 32);

        // Try to allocate with different size 64 - should return None
        let ptr = State::try_alloc_size(&mut state, SizeClass::Common, 64);
        assert!(ptr.is_none());

        let ptr = State::alloc_size::<false>(&mut state, SizeClass::Common, 64);
        assert!(ptr.is_some());

        assert_eq!(state.used_common_chunks.len(), 2);
        assert_eq!(state.free_common_chunks.len(), initial_chunks_len - 2);

        drop_test_state(state);

        drop(test_lock);
    }

    #[test]
    fn test_alloc_size_large_mixed_sizes() {
        let test_lock = acquire_test_lock();

        let region = unsafe { NonNull::new(alloc_region()).expect("Failed to allocate region") };
        let mut state = State::from_region(region, None);
        let initial_chunks_len = state.free_large_chunks.len();

        // Allocate different large sizes
        let ptr1 = State::alloc_size::<false>(&mut state, SizeClass::Large, 2048);
        let ptr2 = State::alloc_size::<false>(&mut state, SizeClass::Large, 4096);
        let ptr3 = State::alloc_size::<false>(&mut state, SizeClass::Large, 2048);

        assert!(ptr1.is_some());
        assert!(ptr2.is_some());
        assert!(ptr3.is_some());

        unsafe {
            ptr1.unwrap().write_bytes(0, 1);
            ptr2.unwrap().write_bytes(0, 1);
            ptr3.unwrap().write_bytes(0, 1);
        };

        // Should have 2 used chunks (one for size 2048, one for size 4096)
        assert_eq!(state.used_large_chunks.len(), 2);
        assert_eq!(state.free_large_chunks.len(), initial_chunks_len - 2);

        drop_test_state(state);

        drop(test_lock);
    }

    #[test]
    fn test_alloc_size_mix_common_and_large() {
        let test_lock = acquire_test_lock();

        let region = unsafe { NonNull::new(alloc_region()).expect("Failed to allocate region") };
        let mut state = State::from_region(region, None);

        let common_ptr = State::alloc_size::<false>(&mut state, SizeClass::Common, 128);
        let large_ptr = State::alloc_size::<false>(&mut state, SizeClass::Large, 8192);

        assert!(common_ptr.is_some());
        assert!(large_ptr.is_some());
        assert_eq!(state.used_common_chunks.len(), 1);
        assert_eq!(state.used_large_chunks.len(), 1);

        unsafe {
            common_ptr.unwrap().write_bytes(0, 1);
            large_ptr.unwrap().write_bytes(0, 1);
        }

        drop_test_state(state);

        drop(test_lock);
    }

    #[test]
    fn test_state_pointer_arithmetic_common() {
        let test_lock = acquire_test_lock();

        let region = unsafe { NonNull::new(alloc_region()).expect("Failed to allocate region") };
        let mut state = State::from_region(region, None);
        let size = 128;
        let slots = COMMON_CHUNK_SIZE / size;

        // Allocate multiple objects from the same chunk
        let mut ptrs = Vec::new();
        for _ in 0..slots {
            if let Some(ptr) = State::alloc_size::<false>(&mut state, SizeClass::Common, size) {
                ptrs.push(ptr);

                unsafe {
                    ptr.write_bytes(0, 1)
                };
            }
        }

        // Verify pointers are correctly spaced
        if ptrs.len() >= 2 {
            let addr1 = ptrs[0].as_ptr() as usize;
            let addr2 = ptrs[1].as_ptr() as usize;
            assert!(addr1.abs_diff(addr2) >= size);
        }

        drop_test_state(state);

        drop(test_lock);
    }

    #[test]
    fn test_state_pointer_arithmetic_large() {
        let test_lock = acquire_test_lock();

        let region = unsafe { NonNull::new(alloc_region()).expect("Failed to allocate region") };
        let mut state = State::from_region(region, None);
        let size = 8192;
        let slots = LARGE_CHUNK_SIZE / size;

        // Allocate multiple objects from the same chunk
        let mut ptrs = Vec::new();
        for _ in 0..slots {
            if let Some(ptr) = State::alloc_size::<false>(&mut state, SizeClass::Large, size) {
                ptrs.push(ptr);

                unsafe { ptr.write_bytes(0, 1) };
            }
        }

        // Verify pointers are correctly spaced
        if ptrs.len() >= 2 {
            let addr1 = ptrs[0].as_ptr() as usize;
            let addr2 = ptrs[1].as_ptr() as usize;
            assert!(addr1.abs_diff(addr2) >= size);
        }

        drop_test_state(state);

        drop(test_lock);
    }

    #[test]
    fn test_exhaust_common_chunks_and_verify_next_state() {
        let test_lock = acquire_test_lock();

        let region = unsafe { NonNull::new(alloc_region()).expect("Failed to allocate region") };
        let mut state = State::from_region(region, None);
        let size = 64;

        // Allocate all common chunks from the initial region
        for _ in 0..MAX_COMMON_CHUNKS_IN_REGION * (COMMON_CHUNK_SIZE / size) {
            let ptr = State::alloc_size::<false>(&mut state, SizeClass::Common, size);
            assert!(ptr.is_some());

            unsafe { ptr.unwrap().write_bytes(0, 1) };
        }

        assert_eq!(state.free_common_chunks.len(), 0);
        assert_eq!(state.free_large_chunks.len(), 0);
        assert!(state.next_state.is_none());

        // Allocate one more - should trigger the next state creation
        let ptr = State::alloc_size::<false>(&mut state, SizeClass::Common, size);
        assert!(ptr.is_some());

        unsafe { ptr.unwrap().write_bytes(0, 1) };

        // Verify the next state was created and linked properly
        assert!(state.next_state.is_some());

        let next_state = unsafe { state.next_state.unwrap().as_ref() };

        // Verify the parent-child relationship
        assert_eq!(next_state.first_state.unwrap().as_ptr().cast_const(), &state as *const _);

        for _ in 1..MAX_COMMON_CHUNKS_IN_REGION * (COMMON_CHUNK_SIZE / size) {
            let ptr = State::alloc_size::<false>(&mut state, SizeClass::Common, size);
            assert!(ptr.is_some());

            unsafe { ptr.unwrap().write_bytes(0, 1) };
        }

        assert_eq!(next_state.free_common_chunks.len(), 0);
        assert_eq!(next_state.free_large_chunks.len(), 0);

        assert!(matches!(SizeClass::define(size_of::<State>()), SizeClass::Common));
        assert!(next_state.next_state.is_some()); // It should be allocated in itself, and in tests State has a common size, so it should allocate one more state in this test.

        // Wait for the third allocation
        let ptr3 = State::alloc_size::<false>(&mut state, SizeClass::Common, size);
        assert!(ptr3.is_some());

        unsafe { ptr3.unwrap().write_bytes(0, 1) };

        assert_eq!(next_state.free_common_chunks.len(), 0);
        assert_eq!(next_state.free_large_chunks.len(), 0);
        assert!(next_state.next_state.is_some());

        unsafe {
            state.used_common_chunks.get_mut(17).unwrap().size = 1;
            state.next_state.unwrap().as_mut().used_common_chunks.get_mut(17).unwrap().size = 1;
            state.next_state.unwrap().as_mut().next_state.unwrap().as_mut().used_common_chunks.get_mut(0).unwrap().size = 1;
        }

        // Verify we can get from all chunks now
        for _ in 0..3 {
            let ptr = State::try_alloc_size(&mut state, SizeClass::Common, size);
            assert!(ptr.is_some(), "Should be able to allocate from available chunks");
        }

        drop_test_state(state);

        drop(test_lock);
    }

    #[test]
    fn test_exhaust_large_chunks_and_verify_next_state() {
        let test_lock = acquire_test_lock();

        let region = unsafe { NonNull::new(alloc_region()).expect("Failed to allocate region") };
        let mut state = State::from_region(region, None);
        let size = 4096;

        test_assert!(size_of::<State>() < COMMON_SIZE_MAX as usize);

        // Allocate all common chunks from the initial region
        for _ in 0..LARGE_CHUNKS_IN_REGION * (LARGE_CHUNK_SIZE / size) {
            let ptr = State::alloc_size::<false>(&mut state, SizeClass::Large, size);
            assert!(ptr.is_some());

            unsafe { ptr.unwrap().write_bytes(0, 1) };
        }

        assert_eq!(state.free_common_chunks.len(), REST_COMMON_CHUNKS_IN_REGION);
        assert_eq!(state.free_large_chunks.len(), 0);
        assert!(state.next_state.is_none());

        // Allocate one more - should trigger the next state creation
        let ptr = State::alloc_size::<false>(&mut state, SizeClass::Large, size);
        assert!(ptr.is_some());

        unsafe { ptr.unwrap().write_bytes(0, 1) };

        // Verify the next state was created and linked properly
        assert!(state.next_state.is_some());

        let next_state = unsafe { state.next_state.unwrap().as_ref() };

        // Verify the parent-child relationship
        assert_eq!(next_state.first_state.unwrap().as_ptr().cast_const(), &state as *const _);

        for _ in 0..LARGE_CHUNKS_IN_REGION * (LARGE_CHUNK_SIZE / size) - 1 {
            let ptr = State::alloc_size::<false>(&mut state, SizeClass::Large, size);
            assert!(ptr.is_some());

            unsafe { ptr.unwrap().write_bytes(0, 1) };
        }

        assert_eq!(next_state.free_common_chunks.len(), REST_COMMON_CHUNKS_IN_REGION - 1); // one for itself
        assert_eq!(next_state.free_large_chunks.len(), 0);
        assert!(next_state.next_state.is_none());

        // Wait for the third allocation
        let ptr3 = State::alloc_size::<false>(&mut state, SizeClass::Large, size);
        assert!(ptr3.is_some());

        unsafe { ptr3.unwrap().write_bytes(0, 1) };

        let third_state = unsafe { next_state.next_state.as_ref().unwrap().as_ref() };

        assert_eq!(third_state.free_common_chunks.len(), REST_COMMON_CHUNKS_IN_REGION);
        assert_eq!(third_state.free_large_chunks.len(), 2);
        assert!(next_state.next_state.is_some());
        assert!(third_state.next_state.is_none());

        unsafe {
            state.used_large_chunks.get_mut(0).unwrap().number_of_free_slots = 1;
            state.next_state.unwrap().as_mut().used_large_chunks.get_mut(0).unwrap().number_of_free_slots = 1;
            state.next_state.unwrap().as_mut().next_state.unwrap().as_mut().used_large_chunks.get_mut(0).unwrap().number_of_free_slots = 1;
        }

        // Verify we can get from all chunks now
        for _ in 0..3 {
            let ptr = State::try_alloc_size(&mut state, SizeClass::Large, size);
            assert!(ptr.is_some(), "Should be able to allocate from available chunks");
        }

        drop_test_state(state);

        drop(test_lock);
    }

    #[test]
    fn test_allocate_16_byte_then_other_sizes_verify_retrieval() {
        let test_lock = acquire_test_lock();

        let region = unsafe { NonNull::new(alloc_region()).expect("Failed to allocate region") };
        let mut state = State::from_region(region, None);
        let small_size = 16;
        let other_size = 128;

        // Allocate one 16-byte value
        let ptr_16 = State::alloc_size::<false>(&mut state, SizeClass::Common, small_size);

        assert!(ptr_16.is_some());
        assert_eq!(state.used_common_chunks.len(), 1);
        assert_eq!(state.used_common_chunks.get_mut(0).unwrap().size, small_size);

        for _ in 0..MAX_COMMON_CHUNKS_IN_REGION * (COMMON_CHUNK_SIZE / other_size) {
            let ptr = State::alloc_size::<false>(&mut state, SizeClass::Common, other_size);
            assert!(ptr.is_some());

            unsafe { ptr.unwrap().write_bytes(0, 1) };
        }

        assert!(state.next_state.is_some());

        assert!(State::alloc_size::<false>(&mut state, SizeClass::Common, small_size).is_some());
        assert_eq!(unsafe { state.next_state.unwrap().as_ref() }.used_common_chunks.len(), 2); // one for other_size and one for itself

        drop_test_state(state);

        drop(test_lock);
    }
}

#[cfg(test)]
mod allocator_for_self_allocation_tests {
    use crate::DefaultThreadLocalAllocator;
    use crate::test_utilities::do_test_default_thread_local_allocator;
    use super::*;

    fn first_self_alloc_(allocator: &DefaultThreadLocalAllocator, with_state: bool) {
        let mut maybe_state = None;
        let (ptr1, ptr2) = if with_state {
            maybe_state = Some(State::from_region(
                unsafe { NonNull::new(alloc_region()).unwrap() },
                None
            ));
            (
                allocator.self_alloc_::<true>(56, Some(maybe_state.as_mut().unwrap())).unwrap(),
                allocator.self_alloc_::<true>(1152, Some(maybe_state.as_mut().unwrap())).unwrap()
            )
        } else {
            (
                allocator.self_alloc_::<true>(56, None).unwrap(),
                allocator.self_alloc_::<true>(1152, None).unwrap(),
            )
        };

        unsafe {
            ptr1.write_bytes(0, 1);
            ptr2.write_bytes(0, 1);
        };

        if with_state {
            assert!(!maybe_state.as_ref().unwrap().used_common_chunks.is_empty());
            assert!(
                maybe_state
                    .as_ref()
                    .unwrap()
                    .used_large_chunks
                    .iter()
                    .find(|used_chunk| used_chunk.size == 1152)
                    .is_some()
            );
            assert!(
                maybe_state
                    .as_ref()
                    .unwrap()
                    .used_common_chunks
                    .iter()
                    .find(|used_chunk| used_chunk.size == 56)
                    .is_some()
            );

            allocator.handle_state(maybe_state.take().unwrap());
        }

        let ptr3 = allocator.try_alloc_common_from_cache(1024, None);

        assert!(
            ptr3.is_some(),
            "`used_common_chunks` should be moved to the allocator after `handle_state`."
        );

        unsafe { allocator.self_dealloc(ptr1, 56, None) };
        unsafe { allocator.self_dealloc(ptr2, 1152, None) };
        unsafe { allocator.self_dealloc(ptr3.unwrap(), 1024, None) };
    }

    #[test]
    fn first_self_alloc_without_state() {
        do_test_default_thread_local_allocator(|allocator| first_self_alloc_(allocator, false));
    }

    #[test]
    fn first_self_alloc_with_state() {
        do_test_default_thread_local_allocator(|allocator| first_self_alloc_(allocator, true));
    }

    fn first_alloc_(allocator: &mut DefaultThreadLocalAllocator) {
        let ptr = allocator.alloc_or_panic(1024);

        unsafe { ptr.write_bytes(0, 1) };

        unsafe { allocator.dealloc(ptr, 1024) };
    }

    #[test]
    fn first_alloc() {
        do_test_default_thread_local_allocator(first_alloc_);
    }
}