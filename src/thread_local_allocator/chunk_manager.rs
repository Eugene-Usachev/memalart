use crate::region::{alloc_region, dealloc_region, region_ptr_from_chunk_ptr, RawRegion, MAX_COMMON_CHUNKS_IN_REGION};
use crate::sizes::{split_large_chunk_to_common, CommonRawChunk, LargeRawChunk, COMMON_CHUNKS_IN_LARGE_CHUNK};
use orengine_utils::hints::{assert_hint, cold_path, unlikely, unreachable_hint, unwrap_or_bug_hint};
use std::ptr::NonNull;
use crate::AllocatorForSelfAllocations;
use crate::{NonLocalPtrManager, ThreadLocalAllocator};
use crate::os::VecInOs;
use crate::self_allocated_structs::SelfAllocatedVec;
use crate::test_assert::test_assert;
use crate::thread_local_allocator::numbers_of_used_objects_with_size::UsedObjectsMutSlice;

struct RegionView {
    ptr: NonNull<RawRegion>,
    number_of_common_chunks_occupied: usize,
}

pub(crate) struct ChunkManager {
    /// In this allocator we never put back freed chunks, only new chunks are here.
    free_large_chunks: SelfAllocatedVec<NonNull<LargeRawChunk>>,
    /// In this allocator we never put back freed chunks, only new chunks are here.
    free_common_chunks: SelfAllocatedVec<NonNull<CommonRawChunk>>,
    regions: SelfAllocatedVec<RegionView>,
}

pub(super) const MAX_NUMBER_OF_CHUNK_MANAGER_SELF_ALLOCATIONS: usize = 3;

impl ChunkManager {
    pub(crate) const fn new() -> Self {
        Self {
            free_large_chunks: SelfAllocatedVec::new(),
            free_common_chunks: SelfAllocatedVec::new(),
            regions: SelfAllocatedVec::new(),
        }
    }

    /// Returns an array of the size and number of values with this size.
    pub(super) fn write_allocated_memory(&self, mut arr: UsedObjectsMutSlice) {
        arr.write_used_number_and_size(self.free_large_chunks.bytes_allocated(), 1);
        arr.write_used_number_and_size(self.free_common_chunks.bytes_allocated(), 1);
        arr.write_used_number_and_size(self.regions.bytes_allocated(), 1);
    }
    
    #[cold]
    #[inline(never)]
    fn alloc_large_chunk<NLPM: NonLocalPtrManager>(
        &mut self,
        allocator: &ThreadLocalAllocator<NLPM>,
    ) -> Option<NonNull<LargeRawChunk>> {
        let mut state = ThreadLocalAllocator::<NLPM>::state_from_region(
            NonNull::new(unsafe { alloc_region() })?
        );

        let large_chunk = state.get_free_large_chunk::<false>()
            .expect("Can't allocate memory for internal usage, unrecoverable memory leak");

        allocator.handle_state(state);

        Some(large_chunk)
    }

    pub(super) fn try_get_large_chunk(&mut self) -> Option<NonNull<LargeRawChunk>> {
        self.free_large_chunks.pop()
    }

    pub(crate) fn get_large_chunk<NLPM: NonLocalPtrManager>(
        &mut self,
        allocator: &ThreadLocalAllocator<NLPM>
    ) -> Option<NonNull<LargeRawChunk>> {
        if let Some(chunk) = self.try_get_large_chunk() {
            return Some(chunk);
        }

        self.alloc_large_chunk(allocator)
    }

    #[cold]
    #[inline(never)]
    fn get_common_chunk_slow<NLPM: NonLocalPtrManager>(
        &mut self,
        allocator: &ThreadLocalAllocator<NLPM>
    ) -> Option<NonNull<CommonRawChunk>> {
        let large_chunk = self.get_large_chunk(allocator)?;
        if let Some(common_chunk) = self.free_common_chunks.pop() {
            cold_path();

            let res = self.free_large_chunks.push(large_chunk, allocator, None); // unused
            test_assert!(
                res.is_ok(),
                "Pushing large chunk to free_large_chunks should never fail in `get_common_chunk_slow`"
            );

            return Some(common_chunk);
        }

        let common_chunks = split_large_chunk_to_common(large_chunk);
        let common_chunks_len = common_chunks.len();

        let res = self
            .free_common_chunks
            .extend_from_slice(&common_chunks[..common_chunks_len - 1], allocator, None);
        if unlikely(res.is_err()) {
            return None;
        }

        Some(*unwrap_or_bug_hint(common_chunks.get(common_chunks_len - 1)))
    }

    pub(super) fn try_get_common_chunk(&mut self) -> Option<NonNull<CommonRawChunk>> {
        self.free_common_chunks.pop()
    }

    pub(crate) fn get_common_chunk<NLPM: NonLocalPtrManager>(
        &mut self,
        allocator: &ThreadLocalAllocator<NLPM>
    ) -> Option<NonNull<CommonRawChunk>> {
        if let Some(chunk) = self.try_get_common_chunk() {
            return Some(chunk);
        }

        self.get_common_chunk_slow(allocator)
    }

    #[must_use]
    pub(super) fn prepare_to_handle_state_and_return_if_allocated_more<NLPM: NonLocalPtrManager>(
        &mut self,
        allocator: &ThreadLocalAllocator<NLPM>,
        state: &mut <ThreadLocalAllocator<NLPM> as AllocatorForSelfAllocations>::State
    ) -> bool {
        let mut did_allocate_more = false;
        
        let res = self
            .regions
            .reserve_without_counting_limit(
                1,
                allocator,
                Some(state)
            );
        
        match res {
            Ok(did_allocated_) => {
                did_allocate_more |= did_allocated_;
            }
            Err(_) => {
                panic!(
                    "Failed to reserve a slot for region in `ChunkManager::prepare_to_handle_state`, \
                    a memory leak occurred."
                )
            }
        }

        let res = self
            .free_large_chunks
            .reserve_without_counting_limit(
                state.free_large_chunks_len() as u32,
                allocator,
                Some(state)
            );
        
        match res {
            Ok(did_allocated_) => {
                did_allocate_more |= did_allocated_;
            }
            Err(_) => {
                panic!(
                    "Failed to reserve slots for free large chunks in \
                    `ChunkManager::prepare_to_handle_state`, a memory leak occurred."
                )
            }
        }

        let res = self.free_common_chunks.reserve_without_counting_limit(
                state.free_common_chunks_len() as u32,
                allocator,
                Some(state)
            );
        
        match res {
            Ok(did_allocated_) => {
                did_allocate_more |= did_allocated_;
            }
            Err(_) => {
                panic!(
                    "Failed to reserve slots for free common chunks in \
                    `ChunkManager::prepare_to_handle_state`, a memory leak occurred."
                )
            }
        }
        
        did_allocate_more
    }

    pub(super) fn handle_state<NLPM: NonLocalPtrManager>(
        &mut self,
        mut state: <ThreadLocalAllocator<NLPM> as AllocatorForSelfAllocations>::State
    ) {
        test_assert!(
            !state.has_next_state(),
            "`next_state` should be empty in `ChunkManager::handle_state` \
             because they are processed in `ThreadLocalAllocator::handle_state`"
        );

        test_assert!(
            state.used_common_chunks_len() == 0,
            "`used_common_chunks` should be empty in `ChunkManager::handle_state` \
             because they are processed in `ThreadLocalAllocator::handle_state`"
        );

        test_assert!(
            state.used_large_chunks_len() == 0,
            "`used_large_chunk` should be empty in `ChunkManager::handle_state`\
             because they are processed in `ThreadLocalAllocator::handle_state`"
        );

        let region = state.region_ptr();
        let _ = self.regions.try_push(
            RegionView {
                ptr: region,
                number_of_common_chunks_occupied: MAX_COMMON_CHUNKS_IN_REGION,
            },
        ).map_err(|_| unreachable_hint());
        
        state.clear_free_common_chunks(|chunks| {
            let _ = self.free_common_chunks.try_extend_from_slice(chunks).map_err(|_| unreachable_hint());
        });

        state.clear_free_large_chunks(|chunks| {
            let _ = self.free_large_chunks.try_extend_from_slice(chunks).map_err(|_| unreachable_hint());
        });

        test_assert!(state.is_processed());
    }

    pub(crate) fn free_common(&mut self, chunk_ptr: NonNull<CommonRawChunk>) {
        let region_ptr_for_chunk = region_ptr_from_chunk_ptr(chunk_ptr.cast());

        let idx = unwrap_or_bug_hint(
            self.regions
                .iter()
                .position(|region_view| region_view.ptr == region_ptr_for_chunk),
        );

        let region_view = unwrap_or_bug_hint(self.regions.get_mut(idx));

        region_view.number_of_common_chunks_occupied -= 1;

        if region_view.number_of_common_chunks_occupied == 0 {
            unsafe {
                dealloc_region(region_view.ptr);
            }

            self.regions.swap_remove(idx);
        }
    }

    pub(crate) fn free_large(&mut self, chunk_ptr: NonNull<LargeRawChunk>) {
        let region_ptr_for_chunk = region_ptr_from_chunk_ptr(chunk_ptr.cast());

        let idx = unwrap_or_bug_hint(
            self.regions
                .iter()
                .position(|region_view| region_view.ptr == region_ptr_for_chunk),
        );

        let region_view = unwrap_or_bug_hint(self.regions.get_mut(idx));

        region_view.number_of_common_chunks_occupied -= COMMON_CHUNKS_IN_LARGE_CHUNK;

        if region_view.number_of_common_chunks_occupied == 0 {
            unsafe {
                dealloc_region(region_view.ptr);
            }

            self.regions.swap_remove(idx);
        }
    }

    pub(crate) fn free_unused_memory(&mut self) {
        while let Some(common_chunk) = self.free_common_chunks.pop() {
            self.free_common(common_chunk);
        }

        while let Some(large_chunk) = self.free_large_chunks.pop() {
            self.free_large(large_chunk);
        }
    }

    pub(crate) unsafe fn free_all_memory(&mut self) {
        // Now we can just forget about all chunks and let the allocator free all regions.
        unsafe { self.free_common_chunks.force_become_null(); }
        unsafe { self.free_large_chunks.force_become_null(); }
        
        let mut regions = VecInOs::new();
        
        regions.reserve(self.regions.len());

        for region in self.regions.iter() {
            if cfg!(test) {
                let ptr = region.ptr.as_ptr();

                assert!(ptr.addr() & (crate::region::REGION_SIZE - 1) == 0);
            }
            
            regions.push(region.ptr);
        }
        
        for region_ptr in regions.iter().copied() {
            unsafe { dealloc_region(region_ptr) };
        }

        unsafe { self.regions.force_become_null() };
    }
}
