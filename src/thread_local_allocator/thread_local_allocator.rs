use core::slice;
use std::alloc::{GlobalAlloc, Layout};
use std::cell::UnsafeCell;
use crate::limit::{mark_deallocated, may_allocate};
use crate::non_local_ptr_manager::{LockFreeNonLocalPtrManager, NonLocalPtrManager, OffsetToSlotInTable};
use crate::thread_local_allocator::chunk_manager::{ChunkManager, MAX_NUMBER_OF_CHUNK_MANAGER_SELF_ALLOCATIONS};
use crate::region::{alloc_region_without_counting_limit, chunk_ptr_to_chunk_metadata_ptr, init_metadata_for_common_chunk, init_metadata_for_large_chunk, ChunkMetadata};
use crate::sizes::{common_size_to_index, large_size_to_id_and_real_size, CommonRawChunk, LargeObjectTable, LargeRawChunk, SizeClass, COMMON_CHUNK_SIZE, COMMON_SIZE_MAX, COMMON_SIZE_TABLE_LEN, LARGE_CHUNK_SIZE, LARGE_SIZE_MAX, MAX_ALLOCATIONS_FOR_LARGE_OBJECT_TABLE, REAL_LARGE_SIZES, REAL_NUMBER_OF_LARGE_OBJECT_VECTORS};
use crate::test_assert::test_assert;
use crate::{DefaultThreadLocalAllocator, NonLocalPtrData};
use orengine_utils::hints::{assert_hint, cold_path, likely, unlikely, unwrap_or_bug_hint};
use std::hint::assert_unchecked;
use std::ptr;
use std::cmp::min;
use std::marker::PhantomData;
use std::mem::{needs_drop, offset_of, ManuallyDrop, MaybeUninit};
use std::ptr::{null_mut, NonNull};
use std::time::Duration;
use crate::AllocatorForSelfAllocations;
use crate::thread_local_allocator::numbers_of_used_objects_with_size::{UsedObjectsFixed};
use crate::os::{alloc_from_os, deregister_thread_local_allocator_destructor, register_thread_local_allocator_destructor, spawn_thread};
use crate::self_allocated_structs::{btree, OptionSelfAllocatedBox, SelfAllocatedBox, SelfAllocatedVec};
use crate::self_allocated_structs::btree::map::SelfAllocatedBTreeMap;
use crate::stats::{LazyDetailedStats, ShortStats, SnapshotDetailedStats};
use crate::thread_local_allocator::info_for_size::InfoForSize;
use crate::thread_local_allocator::InfoForSizeIter;

type CommonChunkTable = [SelfAllocatedVec<NonNull<CommonRawChunk>>; COMMON_SIZE_TABLE_LEN];
type LargeChunkTable = [SelfAllocatedVec<NonNull<LargeRawChunk>>; REAL_NUMBER_OF_LARGE_OBJECT_VECTORS];

#[repr(C)]
pub(crate) struct ThreadLocalAllocatorInner<NLPM = ()> {
    pub(super) non_local_ptr_manager: OptionSelfAllocatedBox<NLPM>,

    pub(super) chunk_manager: ChunkManager,

    pub(crate) common_object_table: [SelfAllocatedVec<NonNull<u8>>; COMMON_SIZE_TABLE_LEN],
    pub(super) common_chunk_table: OptionSelfAllocatedBox<CommonChunkTable>,

    pub(crate) large_object_table: LargeObjectTable,
    pub(super) large_chunk_table: OptionSelfAllocatedBox<LargeChunkTable>,

    pub(super) allocated_large_sizes: SelfAllocatedBTreeMap<usize, usize>,

    pub(super) is_borrowed_for_unknown_time: bool,
    pub(super) is_already_destructured: bool,
    pub(super) is_destructor_registered: bool,
}

impl<NLPM> ThreadLocalAllocatorInner<NLPM> {
    pub(crate) fn common_chunk_object_vector_for_size(
        &self,
        size: usize
    ) -> &SelfAllocatedVec<NonNull<u8>> {
        test_assert!(size <= COMMON_SIZE_MAX as usize);

        let idx = common_size_to_index(size as u16);

        test_assert!(self.common_object_table.len() > idx);

        unsafe { self.common_object_table.get_unchecked(idx) }
    }

    pub(crate) fn mut_common_chunk_object_vector_for_size(
        &mut self,
        size: usize
    ) -> &mut SelfAllocatedVec<NonNull<u8>> {
        test_assert!(size <= COMMON_SIZE_MAX as usize);

        let idx = common_size_to_index(size as u16);

        test_assert!(self.common_object_table.len() > idx);

        unsafe { self.common_object_table.get_unchecked_mut(idx) }
    }
}

unsafe impl<NLPM: NonLocalPtrManager> Send for ThreadLocalAllocatorInner<NLPM> {}
unsafe impl<NLPM: NonLocalPtrManager> Sync for ThreadLocalAllocatorInner<NLPM> {}

pub(super) const MAX_NUMBER_OF_SELF_ALLOCATED_OBJECTS: usize = 1 /* for non_local_ptr_manager */
    + COMMON_SIZE_TABLE_LEN /* for common_object_table */
    + MAX_NUMBER_OF_CHUNK_MANAGER_SELF_ALLOCATIONS
    + 1 /* common_chunk_table */
    + COMMON_SIZE_TABLE_LEN /* for CommonChunkTable */
    + MAX_ALLOCATIONS_FOR_LARGE_OBJECT_TABLE /* for large_object_table */
    + 1 /* for large_chunk_table */
    + REAL_NUMBER_OF_LARGE_OBJECT_VECTORS /* LargeChunkTable */
;

// It is safe to be zeroed when NLPM is safe to be zeroed.
#[repr(C)]
pub struct ThreadLocalAllocator<NLPM: NonLocalPtrManager> {
    inner: UnsafeCell<ThreadLocalAllocatorInner<NLPM>>,
    non_send_sync: PhantomData<*mut ()>,
}

impl<NLPM: NonLocalPtrManager> ThreadLocalAllocator<NLPM> {
    // TODO new is not pub, but Allocator provides a public getter to  `ThreadLocalAllocator`.

    // TODO ThreadLocalAllocator is fixed sized so TLS is enough for each ThreadLocalAllocator<NLPM>

    // TODo pub(crate)
    pub const fn new() -> Self {
        Self {
            inner: UnsafeCell::new(ThreadLocalAllocatorInner {
                non_local_ptr_manager: OptionSelfAllocatedBox::none(),
                chunk_manager: ChunkManager::new(),
                common_object_table: [const { SelfAllocatedVec::new() }; COMMON_SIZE_TABLE_LEN],
                common_chunk_table: OptionSelfAllocatedBox::none(),
                large_object_table: LargeObjectTable::new(),
                large_chunk_table: OptionSelfAllocatedBox::none(),
                allocated_large_sizes: SelfAllocatedBTreeMap::new(),
                is_borrowed_for_unknown_time: false,
                is_already_destructured: false,
                is_destructor_registered: false,
            }),
            non_send_sync: PhantomData,
        }
    }

    // TODO private
    pub(crate) const fn get_inner_with_any_nlpm<'any, AnyNLPM>(
        &self
    ) -> &'any mut ThreadLocalAllocatorInner<AnyNLPM> {
        unsafe { &mut *self.inner.get().cast::<ThreadLocalAllocatorInner<AnyNLPM>>() }
    }

    pub(crate) const fn get_inner<'any>(
        &self
    ) -> &'any mut ThreadLocalAllocatorInner<NLPM> {
        self.get_inner_with_any_nlpm()
    }

    pub(crate) const fn get_inner_ref_any_nlpm<AnyNLPM>(
        &self
    ) -> &UnsafeCell<ThreadLocalAllocatorInner<AnyNLPM>> {
        test_assert!(
           size_of::<ThreadLocalAllocatorInner<AnyNLPM>>() == size_of_val(&self.inner),
            "size of ThreadLocalAllocatorInner<AnyNLPM> != size of `ThreadLocalAllocator.inner`"
        );

        unsafe { &*(&raw const self.inner).cast::<UnsafeCell<ThreadLocalAllocatorInner<AnyNLPM>>>() }
    }

    pub(crate) fn is_already_destructured(&self) -> bool {
        self.get_inner().is_already_destructured
    }

    pub(super) fn allocated_for_self(
        &self
    ) -> UsedObjectsFixed<MAX_NUMBER_OF_SELF_ALLOCATED_OBJECTS> {
        let mut arr = UsedObjectsFixed::new();
        let inner = self.get_inner();

        if let Some(nlpm) = inner.non_local_ptr_manager.as_ref() {
            if size_of::<NLPM>() != 0 {
                arr.write_used_number_and_size(SelfAllocatedBox::<NLPM>::real_size(), 1);
            }

            for size in nlpm.self_allocated_memory() {
                arr.write_used_number_and_size(size, 1);
            }
        }

        inner
            .chunk_manager
            .write_allocated_memory(arr.as_mut_slice());

        for table in inner.common_object_table.iter() {
            arr.write_used_number_and_size(table.bytes_allocated(), 1);
        }

        if let Some(common_chunk_table) = inner.common_chunk_table.as_ref() {
            arr.write_used_number_and_size(SelfAllocatedBox::<CommonChunkTable>::real_size(), 1);

            for chunks in common_chunk_table.iter() {
                arr.write_used_number_and_size(chunks.bytes_allocated(), 1);
            }
        }

        for bytes in inner.large_object_table.self_allocated_memory() {
            arr.write_used_number_and_size(bytes, 1);
        }

        if let Some(large_chunk_table) = inner.large_chunk_table.as_ref() {
            arr.write_used_number_and_size(SelfAllocatedBox::<LargeChunkTable>::real_size(), 1);

            for chunks in large_chunk_table.iter() {
                arr.write_used_number_and_size(chunks.bytes_allocated(), 1);
            }
        }

        for (size, number) in inner.allocated_large_sizes.self_allocated_memory() {
            arr.write_used_number_and_size(size, number);
        }

        arr
    }

    // TODO pub(crate)
    pub fn get_or_init_and_get_non_local_ptr_manager_ptr(
        &self,
        maybe_state: Option<&mut <ThreadLocalAllocator<NLPM> as AllocatorForSelfAllocations>::State>
    ) -> NonNull<NLPM> {
        #[inline(never)]
        #[cold]
        fn slow_path<NLPM: NonLocalPtrManager>(
            thread_local_allocator: &ThreadLocalAllocator<NLPM>,
            maybe_state: Option<&mut <ThreadLocalAllocator<NLPM> as AllocatorForSelfAllocations>::State>
        ) -> NonNull<NLPM> {
            let mut maybe_allocated_now_state = None;
            let inner = thread_local_allocator.get_inner();
            let non_local_ptr_manager = NLPM::default();
            if !inner.is_destructor_registered {
                thread_local_allocator.register_destructor();
            }

            if size_of::<NLPM>() == 0 {
                inner.non_local_ptr_manager = OptionSelfAllocatedBox::some(
                    SelfAllocatedBox::from_ptr(NonNull::dangling())
                );

                return NonNull::from(unwrap_or_bug_hint(inner.non_local_ptr_manager.as_mut()));
            }

            let state = if maybe_state.is_none() {
                cold_path();

                let region = NonNull::new(unsafe { alloc_region_without_counting_limit() })
                    .expect("Failed to allocate memory for the first allocation.");
                maybe_allocated_now_state = Some(
                    <ThreadLocalAllocator<NLPM> as AllocatorForSelfAllocations>::state_from_region(region)
                );

                unwrap_or_bug_hint(maybe_allocated_now_state.as_mut())
            } else {
                unwrap_or_bug_hint(maybe_state)
            };

            state.set_is_allocating_non_local_ptr_manager_now(true);

            test_assert!(thread_local_allocator.get_inner().non_local_ptr_manager.is_null());
            
            inner.non_local_ptr_manager = OptionSelfAllocatedBox::new(
                non_local_ptr_manager,
                thread_local_allocator,
                Some(state)
            ).unwrap_or_else(|_| panic!("Failed to allocate `returning_local_ptr_handler`."));

            state.set_is_allocating_non_local_ptr_manager_now(false);

            if let Some(state) = maybe_allocated_now_state.take() {
                thread_local_allocator.handle_state(state);
            }
            
            NonNull::from(unwrap_or_bug_hint(inner.non_local_ptr_manager.as_mut()))
        }

        test_assert!(
            !needs_drop::<NLPM>(),
            "`NonLocalPtrManager` should not have a default `Drop` implementation.\
             Use its own `self_drop` instead and ensure it does not deallocate any memory.\
             `NonLocalPtrManager` is allocated in the allocator and its memory gets freed \
             when the allocator destructs without explicitly deallocations of its own memory."
        );

        let inner = self.get_inner();
        let non_local_ptr_manager_ = inner.non_local_ptr_manager.as_ref();
        if let Some(ptr) = non_local_ptr_manager_ {
            return NonNull::from(ptr);
        }

        slow_path(self, maybe_state)
    }

    pub(super) fn common_chunk_table(
        &self,
        maybe_state: Option<&mut <ThreadLocalAllocator<NLPM> as AllocatorForSelfAllocations>::State>
    ) -> &mut CommonChunkTable {
        #[cold]
        #[inline(never)]
        fn slow_path<'a, NLPM: NonLocalPtrManager + 'a>(
            thread_local_allocator: &'a ThreadLocalAllocator<NLPM>,
            maybe_state: Option<&mut <ThreadLocalAllocator<NLPM> as AllocatorForSelfAllocations>::State>
        ) -> &'a mut CommonChunkTable {
            let inner = thread_local_allocator.get_inner();
            let res = SelfAllocatedBox::new(
                [const { SelfAllocatedVec::new() }; COMMON_SIZE_TABLE_LEN],
                thread_local_allocator,
                maybe_state
            );

            match res {
                Ok(common_chunk_table) => {
                    inner.common_chunk_table = OptionSelfAllocatedBox::some(common_chunk_table);

                    unwrap_or_bug_hint(inner.common_chunk_table.as_mut())
                }
                Err(_) => {
                    cold_path();

                    panic!("Failed to allocate `common_chunk_table`.");
                }
            }
        }
        
        if let Some(common_chunk_table) = self.get_inner().common_chunk_table.as_mut() {
            return common_chunk_table;
        }

        slow_path(self, maybe_state)
    }

    #[inline(always)]
    pub(super) fn common_chunk_object_vector_for_size(
        &self,
        size: usize
    ) -> &mut SelfAllocatedVec<NonNull<u8>> {
        self.get_inner().mut_common_chunk_object_vector_for_size(size)
    }

    pub(super) fn large_chunk_table(
        &self,
        maybe_state: Option<&mut <ThreadLocalAllocator<NLPM> as AllocatorForSelfAllocations>::State>
    ) -> &mut LargeChunkTable {
        #[cold]
        #[inline(never)]
        fn slow_path<'a, NLPM: NonLocalPtrManager + 'a>(
            thread_local_allocator: &'a ThreadLocalAllocator<NLPM>,
            maybe_state: Option<&mut <ThreadLocalAllocator<NLPM> as AllocatorForSelfAllocations>::State>
        ) -> &'a mut LargeChunkTable {
            let inner = thread_local_allocator.get_inner();
            let res = SelfAllocatedBox::new(
                [const { SelfAllocatedVec::new() }; REAL_NUMBER_OF_LARGE_OBJECT_VECTORS],
                thread_local_allocator,
                maybe_state
            );

            test_assert!(inner.large_chunk_table.is_null());

            match res {
                Ok(large_chunk_table) => {
                    inner.large_chunk_table = OptionSelfAllocatedBox::some(large_chunk_table);

                    unwrap_or_bug_hint(inner.large_chunk_table.as_mut())
                }
                Err(_) => {
                    cold_path();

                    panic!("Failed to allocate `common_chunk_table`.");
                }
            }
        }

        if let Some(large_chunk_table) = self.get_inner().large_chunk_table.as_mut() {
            return large_chunk_table;
        }

        slow_path(self, maybe_state)
    }

    pub fn check_non_local_ptrs(&self) {
        match self.get_inner().non_local_ptr_manager.as_ref() {
            Some(non_local_ptr_manager) => {
                non_local_ptr_manager
                    .check_for_incoming_non_local_ptrs(
                        self,
                        |ptrs| {
                            for ptr in ptrs {
                                let slot = ptr.offset_to_slot_in_table().get_vec(self);
                                if unlikely(slot.push_without_counting_limit(ptr.ptr(), self, None).is_err()) {
                                    panic!("Failed to return the pointer back.");
                                }
                            }
                        }
                    )
            }
            None => {
                cold_path();

                return;
            }
        }
    }

    fn set_deallocated(&mut self, with_drop_previous_state: bool) {
        // Reset it to the default state.
        if with_drop_previous_state {
            *self = Self::new();
        } else {
            unsafe { ptr::from_mut(self).write(Self::new()) }
        }

        let inner = self.get_inner();

        inner.is_already_destructured = true;
    }

    pub(crate) fn iter_allocated_size(
        &self,
    ) -> InfoForSizeIter {
        InfoForSizeIter::new(self)
    }
}

// region alloc

impl<NLPM: NonLocalPtrManager> ThreadLocalAllocator<NLPM> {
    #[cold]
    #[inline(never)]
    pub(super) unsafe fn alloc_common_slow<const IS_ALLOCATING_NLPM: bool>(
        &self,
        real_size: usize,
        get_common_chunk_fn: impl Fn(&mut ThreadLocalAllocatorInner<NLPM>) -> Option<NonNull<CommonRawChunk>>,
        mut maybe_state: Option<&mut <ThreadLocalAllocator<NLPM> as AllocatorForSelfAllocations>::State>,
    ) -> Option<NonNull<u8>> {
        test_assert!(!IS_ALLOCATING_NLPM || self.get_inner().non_local_ptr_manager.is_null());
        test_assert!(real_size % 8 == 0);

        let size_idx = common_size_to_index(real_size as u16);
        let inner = self.get_inner();

        if !IS_ALLOCATING_NLPM && NLPM::SHOULD_AUTO_CHECK_FOR_INCOMING_NON_LOCAL_PTRS {
            self.check_non_local_ptrs();

            let vec = self.common_chunk_object_vector_for_size(real_size);

            if let Some(ptr) = vec.pop() {
                cold_path();

                return Some(ptr);
            }
        }

        let common_chunk = get_common_chunk_fn(inner)?;

        // Maybe we updated the allocator, let's check again
        if let Some(ptr) = self.common_chunk_object_vector_for_size(real_size).pop() {
            cold_path();

            inner.chunk_manager.free_common(common_chunk);

            return Some(ptr);
        }

        let res = self
            .common_chunk_table(maybe_state.as_deref_mut())[size_idx]
            .push(common_chunk, self, maybe_state.as_deref_mut());
        if unlikely(res.is_err()) {
            return None;
        }

        let mut curr = common_chunk.cast::<u8>();
        let vec = self.common_chunk_object_vector_for_size(real_size);
        let additional_size = (COMMON_CHUNK_SIZE / real_size) - 1;
        let res = vec.reserve(additional_size as u32, self, maybe_state.as_deref_mut());

        if unlikely(res.is_err()) {
            return None;
        }

        for _ in 0..additional_size {
            unsafe { assert_unchecked(vec.capacity() > vec.len()) };

            let res = vec.try_push(curr);

            test_assert!(res.is_ok());

            curr = unsafe { curr.byte_add(real_size) };
        }

        if !IS_ALLOCATING_NLPM {
            let non_local_ptr_manager = self
                .get_or_init_and_get_non_local_ptr_manager_ptr(maybe_state);

            init_metadata_for_common_chunk(
                non_local_ptr_manager,
                OffsetToSlotInTable::new_for_common_chunk(size_idx),
                curr,
            );
        } else {
            init_metadata_for_common_chunk::<NLPM>(
                curr.cast(),
                OffsetToSlotInTable::new_for_common_chunk(size_idx),
                curr,
            );
        }

        Some(curr)
    }

    unsafe fn alloc_common(&self, mut size: usize) -> Option<NonNull<u8>> {
        test_assert!(size <= COMMON_SIZE_MAX as usize);

        let vec = self.common_chunk_object_vector_for_size(size);
        if let Some(ptr) = vec.pop() {
            return Some(ptr);
        }

        size = (size + 7) & !7; // round it to a multiple of 8

        unsafe {
            self.alloc_common_slow::<false>(
                size,
                |inner| {
                    inner.chunk_manager.get_common_chunk(self)
                },
                None
            )
        }
    }

    #[cold]
    #[inline(never)]
    pub(super) unsafe fn alloc_large_slow<const IS_ALLOCATING_NLPM: bool>(
        &self,
        size_id: usize,
        real_size: usize,
        get_large_chunk_fn: impl Fn(&mut ThreadLocalAllocatorInner<NLPM>) -> Option<NonNull<LargeRawChunk>>,
        mut maybe_state: Option<&mut <ThreadLocalAllocator<NLPM> as AllocatorForSelfAllocations>::State>
    ) -> Option<NonNull<u8>> {
        test_assert!(!IS_ALLOCATING_NLPM || self.get_inner().non_local_ptr_manager.is_null());

        let inner = self.get_inner();

        if !IS_ALLOCATING_NLPM && NLPM::SHOULD_AUTO_CHECK_FOR_INCOMING_NON_LOCAL_PTRS {
            self.check_non_local_ptrs();

            let vec = inner.large_object_table.get_vec_mut_by_id(size_id);

            if let Some(ptr) = vec.pop() {
                cold_path();

                return Some(ptr);
            }
        }

        let large_chunk = get_large_chunk_fn(inner)?;

        // Maybe we updated the allocator, let's check again
        if let Some(ptr) = inner.large_object_table.get_vec_mut_by_id(size_id).pop() {
            cold_path();

            inner.chunk_manager.free_large(large_chunk);

            return Some(ptr);
        }

        let idx = inner.large_object_table.get_idx_by_id(size_id);
        let res = self
            .large_chunk_table(maybe_state.as_deref_mut())[idx]
            .push(large_chunk, self, maybe_state.as_deref_mut());
        if unlikely(res.is_err()) {
            return None;
        }

        let mut curr = large_chunk;
        let vec = inner.large_object_table.get_vec_mut_by_id(size_id);
        let additional_size = (LARGE_CHUNK_SIZE / real_size) - 1;
        let res = vec.reserve(additional_size as u32, self, maybe_state.as_deref_mut());
        if unlikely(res.is_err()) {
            return None;
        }

        for _ in 0..additional_size {
            unsafe { assert_unchecked(vec.capacity() > vec.len()) };

            let res = vec.try_push(curr.cast());

            debug_assert!(res.is_ok());

            curr = unsafe { curr.byte_add(real_size) };
        }

        if !IS_ALLOCATING_NLPM {
            init_metadata_for_large_chunk(
                self.get_or_init_and_get_non_local_ptr_manager_ptr(maybe_state),
                OffsetToSlotInTable::new_for_large_chunk(idx),
                curr.cast(),
            );
        } else {
            init_metadata_for_large_chunk::<NLPM>(
                curr.cast(),
                OffsetToSlotInTable::new_for_large_chunk(idx),
                curr.cast(),
            );
        }


        Some(curr.cast())
    }

    unsafe fn alloc_large(&self, size: usize) -> Option<NonNull<u8>> {
        test_assert!(size > COMMON_SIZE_MAX as usize);
        test_assert!(size <= LARGE_SIZE_MAX);

        let (id, real_size) = large_size_to_id_and_real_size(size);
        let inner = self.get_inner();
        let vec = inner.large_object_table.get_vec_mut_by_id(id);

        if let Some(ptr) = vec.pop() {
            return Some(ptr);
        }

        unsafe {
            self.alloc_large_slow::<false>(
                id,
                real_size,
                |inner| {
                    inner.chunk_manager.get_large_chunk(self)
                },
                None
            )
        }
    }

    #[cold]
    pub(super) unsafe fn alloc_extra_large(&self, size: usize) -> Option<NonNull<u8>> {
        test_assert!(size > LARGE_SIZE_MAX);

        if unlikely(!may_allocate(size)) {
            return None;
        }

        let (ptr, _) = alloc_from_os(
            null_mut(),
            size,
            true,  // commit
            false, // huge_only
        );

        if unlikely(ptr.is_null()) {
            mark_deallocated(size);
        }

        self
            .get_inner()
            .allocated_large_sizes
            .entry(size)
            .and_modify(|used_number| {
                *used_number += 1;
            })
            .or_insert(1, self, None)
            .expect("failed to allocate memory for internal usage");

        NonNull::new(ptr)
    }

    #[inline(always)]
    pub fn try_alloc(&self, size: usize) -> Option<NonNull<u8>> {
        match SizeClass::define(size) {
            SizeClass::Common => unsafe { self.alloc_common(size) },
            SizeClass::Large => unsafe { self.alloc_large(size) },
            SizeClass::ExtraLarge => unsafe { self.alloc_extra_large(size) },
        }
    }

    #[inline]
    pub fn alloc_or_panic(&self, size: usize) -> NonNull<u8> {
        self.try_alloc(size).expect("Failed to allocate memory")
    }
}

// endregion

// region dealloc

impl<NLPM: NonLocalPtrManager> ThreadLocalAllocator<NLPM> {
    #[cold]
    fn dealloc_non_local_ptr(&self, ptr: NonNull<u8>, metadata: ChunkMetadata) {
        unsafe {
            let nlpm = self.get_or_init_and_get_non_local_ptr_manager_ptr(None).as_ref();

            nlpm.deallocate_non_local_ptr(
                self,
                metadata.non_local_ptr_manager(),
                NonLocalPtrData::new(ptr, metadata.offset_to_slot_in_table())
            )
        }
    }

    fn is_local_ptr(&self, metadata: ChunkMetadata) -> bool {
        let raw_ptr = self
            .get_inner()
            .non_local_ptr_manager
            .as_ptr()
            .map(|ptr| ptr.as_ptr())
            .unwrap_or(null_mut());

        metadata.non_local_ptr_manager() as *const _ == raw_ptr as *const _
    }

    pub(super) unsafe fn dealloc_common(
        &self,
        ptr: NonNull<u8>,
        size: usize,
    ) {
        test_assert!(size <= COMMON_SIZE_MAX as usize);

        let metadata = unsafe { chunk_ptr_to_chunk_metadata_ptr(ptr).read() };
        let inner = self.get_inner();

        test_assert!(metadata.non_local_ptr_manager_exist());

        if likely(self.is_local_ptr(metadata)) {
            let idx = common_size_to_index(size as u16);
            let object_vec = unwrap_or_bug_hint(inner.common_object_table.get_mut(idx));

            if unlikely(object_vec.push_without_counting_limit(ptr, self, None).is_err()) {
                panic!("Failed to return the pointer back.");
            }

            return;
        }

        test_assert!(metadata.offset_to_slot_in_table().is_common());

        self.dealloc_non_local_ptr(ptr, metadata);
    }

    pub(super) unsafe fn dealloc_large(
        &self,
        ptr: NonNull<u8>,
        size: usize
    ) {
        test_assert!(size > COMMON_SIZE_MAX as usize, "size: {size}");
        test_assert!(size <= LARGE_SIZE_MAX, "size: {size}");

        let metadata = unsafe { chunk_ptr_to_chunk_metadata_ptr(ptr).read() };
        let inner = self.get_inner();

        test_assert!(metadata.non_local_ptr_manager_exist());

        if likely(self.is_local_ptr(metadata)) {
            let (id, _real_size) = large_size_to_id_and_real_size(size);
            if unlikely(inner.large_object_table.get_vec_mut_by_id(id).push_without_counting_limit(ptr, self, None).is_err()) {
                panic!("Failed to return the pointer back.");
            }

            return;
        }

        test_assert!(!metadata.offset_to_slot_in_table().is_common());

        self.dealloc_non_local_ptr(ptr, metadata);
    }

    pub(super) unsafe fn dealloc_extra_large(&self, ptr: NonNull<u8>, size: usize) {
        unsafe {
            crate::os::free(ptr.as_ptr(), size);
        };

        mark_deallocated(size);

        let mut was_last = false;
        self
            .get_inner()
            .allocated_large_sizes
            .entry(size)
            .and_modify(|used_number| {
                if *used_number == 1 {
                    was_last = true;
                }

                *used_number -= 1;
            });

        if was_last {
            self.get_inner().allocated_large_sizes.remove(&size, self, None);
        }
    }

    #[inline(always)]
    pub unsafe fn dealloc(&self, ptr: NonNull<u8>, size: usize) {
        match SizeClass::define(size) {
            SizeClass::Common => unsafe { self.dealloc_common(ptr, size) },
            SizeClass::Large => unsafe { self.dealloc_large(ptr, size) },
            SizeClass::ExtraLarge => unsafe { self.dealloc_extra_large(ptr, size) },
        }
    }
}

// endregion

// region destructor
impl<NLPM: NonLocalPtrManager> ThreadLocalAllocator<NLPM> {
    #[inline(never)]
    pub(crate) fn try_free_all_memory(&mut self) -> Result<(), ()> {
        assert!(!self.is_already_destructured());

        let inner = self.get_inner();
        assert!(
            !inner.is_borrowed_for_unknown_time,
            "trying to free memory while the allocator is borrowed (probably for the statistic iterator)"
        );

        for (size_class, info) in self.iter_allocated_size() {
            if unlikely(matches!(size_class, SizeClass::ExtraLarge)) {
                return Ok(()); // extra large sizes may be deallocated without `ThreadLocalAllocator`
            }

            let may_be_freed = info.objects_used == info.allocated_for_self_objects;

            if unlikely(!may_be_freed) {
                return Err(());
            }
        }

        // If we are here, we have freed all memory.

        unsafe { self.get_or_init_and_get_non_local_ptr_manager_ptr(None).as_mut().self_drop() };

        unsafe {
            inner.chunk_manager.free_all_memory();
            inner.non_local_ptr_manager.force_set_null();
            inner.common_chunk_table.force_set_null();
            inner.large_chunk_table.force_set_null();
        }

        for object_vec in inner.common_object_table.iter_mut() {
            unsafe { object_vec.force_become_null() };
        }

        unsafe { inner.large_object_table.force_become_null(); }

        unsafe { inner.allocated_large_sizes.force_become_null(); }

        self.set_deallocated(true);
        inner.is_destructor_registered = false;

        unsafe { deregister_thread_local_allocator_destructor(); }

        Ok(())
    }

    unsafe fn move_to_another_thread(&mut self) {
        let allocator_inner = unsafe { ptr::from_mut(&mut self.inner).read() };

        spawn_thread(move || {
            let mut allocator = ThreadLocalAllocator {
                inner: allocator_inner,
                non_send_sync: PhantomData,
            };

            let mut timeout = Duration::from_micros(16);

            loop {
                allocator.check_non_local_ptrs();
                if allocator.try_free_all_memory().is_ok() {
                    break;
                }

                std::thread::sleep(timeout);

                timeout = min(timeout * 2, Duration::from_secs(1));
            }

            let _ = ManuallyDrop::new(allocator);
        });

        self.set_deallocated(false);

        // Now all deallocations in the current thread sees that the previous allocations were.
    }

    pub fn destructor(&mut self) {
        if std::thread::panicking() || self.is_already_destructured(){
            return;
        }

        if self.try_free_all_memory().is_ok() {
            return;
        }

        unsafe { self.move_to_another_thread() };
    }

    fn register_destructor(&self) {
        let inner = self.get_inner();

        assert!(
            !inner.is_destructor_registered,
            "An attempt to register ThreadLocalAllocator twice"
        );

        inner.is_destructor_registered = true;
        inner.is_already_destructured = false;

        register_thread_local_allocator_destructor(NonNull::from(self));
    }
}

// endregion

// region stats

impl<NLPM: NonLocalPtrManager> ThreadLocalAllocator<NLPM> {
    pub fn short_stats(&self) -> ShortStats {
        ShortStats::new(self)
    }

    pub fn lazy_detailed_stats(&self) -> LazyDetailedStats {
        LazyDetailedStats::new(self)
    }

    pub fn snapshot_detailed_stats(&self) -> Box<[InfoForSize]> {
        let lazy = LazyDetailedStats::new(self);
        let mut slice = Box::new_uninit_slice(lazy.size_hint().0);
        let mut curr_len = 0;

        for info in lazy {
            // Because this method is sync, the lower bound of size_hint is correct.
            assert_hint(
                curr_len < slice.len(),
                "the lower bound of size_hint is incorrect"
            );

            slice[curr_len].write(info);
            curr_len += 1;
        }

        unsafe { slice.assume_init() }
    }
}

// endregion

impl DefaultThreadLocalAllocator {
    pub const fn new_default() -> Self {
        let allocator: Self = Self::new();

        if cfg!(test) {
            let inner = allocator.get_inner();

            assert!(inner.non_local_ptr_manager.is_null());
            assert!(inner.large_chunk_table.is_null());
            assert!(inner.common_chunk_table.is_null());
        }

        allocator
    }
}

#[cfg(test)]
mod tests {
    use std::ptr::NonNull;
    use std::sync::{Arc, Condvar, Mutex};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender};
    use std::thread;
    use std::time::Duration;
    use crate::{deregister_thread_local_default_allocator, thread_local_default_allocator, DefaultThreadLocalAllocator, NonLocalPtrData, NonLocalPtrManager, ThreadLocalAllocator};
    use crate::{thread_local_default_allocator_mut, DefaultNonLocalPtrManager};
    use crate::test_utilities::{do_test_default_thread_local_allocator, do_test_with_lock};

    fn one_thread_(allocator: &mut DefaultThreadLocalAllocator) {
        let common_size1 = 15;
        let common_size2 = 987;
        let large_size1 = 4090;
        let large_size2 = 12_345;

        let common_ptr1 = allocator.alloc_or_panic(common_size1);
        // TODO let common_ptr2 = allocator.alloc_or_panic(common_size2);
        // TODO let large_ptr1 = allocator.alloc_or_panic(large_size1);
        // TODO let large_ptr2 = allocator.alloc_or_panic(large_size2);

        // TODO assert!(allocator.try_free_all_memory().is_err(), "Memory is freed while all memory is allocated.");

        unsafe { allocator.dealloc(common_ptr1, common_size1) };
        // TODO assert!(allocator.try_free_all_memory().is_err(), "Memory is freed after the first deallocation.");

        // TODO unsafe { allocator.dealloc(large_ptr1, large_size1) };
        // TODO assert!(allocator.try_free_all_memory().is_err(), "Memory is freed after the second deallocation.");

        // TODO unsafe { allocator.dealloc(common_ptr2, common_size2) };
        // TODO assert!(allocator.try_free_all_memory().is_err(), "Memory is freed after the third deallocation.");

        // TODO unsafe { allocator.dealloc(large_ptr2, large_size2) };
        assert!(allocator.try_free_all_memory().is_ok(), "Memory is not freed after the fourth deallocation.");
    }

    #[test]
    fn one_thread() {
        do_test_default_thread_local_allocator(one_thread_);
    }

    fn two_threads_destructors_called_() {
        fn spawn_thread_worker() -> (Arc<(Mutex<bool>, Condvar)>, thread::JoinHandle<()>) {
            let (is_deallocated_sender, is_deallocated_receiver) = channel::<Arc<(Mutex<bool>, Condvar)>>();

            let t = thread::spawn(move || {
                let allocator = thread_local_default_allocator();
                let nlpm: &DefaultNonLocalPtrManager = unsafe {
                    allocator.get_or_init_and_get_non_local_ptr_manager_ptr(None).as_ref()
                };

                is_deallocated_sender.send(nlpm.clone_is_deallocated()).unwrap();
            });

            (is_deallocated_receiver.recv().unwrap(), t)
        }

        let (is_deallocated1, t1) = spawn_thread_worker();
        let (is_deallocated2, t2) = spawn_thread_worker();

        t1.join().unwrap();
        t2.join().unwrap();

        assert!(*is_deallocated1.0.lock().unwrap());
        assert!(*is_deallocated2.0.lock().unwrap());
    }

    #[test]
    fn two_threads_destructors_called() {
        do_test_with_lock(two_threads_destructors_called_);
    }

    fn two_threads_correct_destruction_with_ptr_sharing_() {
        fn spawn_thread_worker(
            sender: SyncSender<usize>,
            receiver: Receiver<usize>,
            is_other_deallocated_receiver: Receiver<Arc<(Mutex<bool>, Condvar)>>
        ) -> (Arc<(Mutex<bool>, Condvar)>, thread::JoinHandle<()>) {
            let (is_deallocated_sender, is_deallocated_receiver) = channel::<Arc<(Mutex<bool>, Condvar)>>();

            let t = thread::spawn(move || {
                let allocator = thread_local_default_allocator();
                let nlpm: &DefaultNonLocalPtrManager = unsafe {
                    allocator.get_or_init_and_get_non_local_ptr_manager_ptr(None).as_ref()
                };

                is_deallocated_sender.send(nlpm.clone_is_deallocated()).unwrap();

                let ptr1 = thread_local_default_allocator().alloc_or_panic(25);
                let ptr2 = thread_local_default_allocator().alloc_or_panic(3111);
                let ptr3 = thread_local_default_allocator().alloc_or_panic(560_000);

                sender.send(ptr1.addr().get()).unwrap();
                sender.send(ptr2.addr().get()).unwrap();
                sender.send(ptr3.addr().get()).unwrap();

                let other_ptr1 = NonNull::new(receiver.recv().unwrap() as *mut u8).unwrap();
                let other_ptr2 = NonNull::new(receiver.recv().unwrap() as *mut u8).unwrap();
                let other_ptr3 = NonNull::new(receiver.recv().unwrap() as *mut u8).unwrap();

                thread::sleep(Duration::from_millis(1));

                let is_other_deallocated = is_other_deallocated_receiver.recv().unwrap();

                assert!(!*is_other_deallocated.0.lock().unwrap());

                unsafe {
                    other_ptr1.write(1);
                    other_ptr2.write(1);
                    other_ptr3.write(1);

                    thread_local_default_allocator().dealloc(other_ptr1, 25);
                    thread_local_default_allocator().dealloc(other_ptr2, 3111);
                    thread_local_default_allocator().dealloc(other_ptr3, 560_000);
                }
            });

            (is_deallocated_receiver.recv().unwrap(), t)
        }

        let (sender1, receiver1) = sync_channel(3);
        let (sender2, receiver2) = sync_channel(3);
        let (is_other_deallocated_sender1, is_other_deallocated_receiver1) = sync_channel(1);
        let (is_other_deallocated_sender2, is_other_deallocated_receiver2) = sync_channel(1);

        let (is_deallocated1, t1) = spawn_thread_worker(sender1, receiver2, is_other_deallocated_receiver2);
        let (is_deallocated2, t2) = spawn_thread_worker(sender2, receiver1, is_other_deallocated_receiver1);

        is_other_deallocated_sender2.send(is_deallocated1.clone()).unwrap();
        is_other_deallocated_sender1.send(is_deallocated2.clone()).unwrap();

        t1.join().unwrap();
        t2.join().unwrap();

        let wait_is_deallocated_with_timeout = |is_deallocated: Arc<(Mutex<bool>, Condvar)>| {
            let is_deallocated_ = is_deallocated.0.lock().unwrap();
            if !*is_deallocated_ {
                let (
                    is_deallocated_,
                    is_timed_out
                ) = is_deallocated.1.wait_timeout(is_deallocated_, Duration::from_secs(3)).unwrap();

                assert!(!is_timed_out.timed_out() && *is_deallocated_);
            }
        };

        wait_is_deallocated_with_timeout(is_deallocated1);
        wait_is_deallocated_with_timeout(is_deallocated2);
    }

    #[test]
    fn two_threads_correct_destruction_with_ptr_sharing() {
        do_test_with_lock(two_threads_correct_destruction_with_ptr_sharing_);
    }

    fn many_tls_value_use_allocator_() {
        struct TestContainer {
            ptr: NonNull<[u8; 1024]>,
            is_deallocated: Arc<(Mutex<bool>, Condvar)>
        }

        impl Drop for TestContainer {
            fn drop(&mut self) {
                unsafe { thread_local_default_allocator().dealloc(self.ptr.cast(), 1024); }

                *self.is_deallocated.0.lock().unwrap() = true;
                self.is_deallocated.1.notify_all();
            }
        }

        fn thread_worker(is_deallocated_sender: SyncSender<Arc<(Mutex<bool>, Condvar)>>) {
            macro_rules! generate_tls_value {
                ($($name:ident),*) => {
                    $(
                        thread_local! {
                            static $name: TestContainer = TestContainer {
                                ptr: thread_local_default_allocator().alloc_or_panic(1024).cast(),
                                is_deallocated: Arc::new((Mutex::new(false), Condvar::new()))
                            };
                        }
                    )*
                }
            }

            generate_tls_value!(A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z);

            macro_rules! use_tls_value {
                ($is_deallocated_sender:expr, $($name:ident),*) => {
                    $({
                        $name.with(|value| unsafe {
                            value.ptr.write_bytes(1, 1);

                            is_deallocated_sender
                                .send(value.is_deallocated.clone())
                                .expect("Failed to write into the channel.");
                        });
                    })*
                };
            }

            use_tls_value!(is_deallocated_sender, A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z);
        }

        let (is_deallocated_sender, is_deallocated_receiver) = sync_channel(26);

        thread::spawn(move || thread_worker(is_deallocated_sender));

        for _ in 0..26 {
            let res = is_deallocated_receiver.recv_timeout(Duration::from_secs(3)).unwrap();

            let is_deallocated = res.0.lock().unwrap();
            if !*is_deallocated {
                let (
                    is_deallocated,
                    is_timed_out
                ) = res.1.wait_timeout(is_deallocated, Duration::from_secs(3)).unwrap();

                assert!(!is_timed_out.timed_out() && *is_deallocated);
            }
        }
    }

    #[test]
    fn many_tls_value_use_allocator() {
        do_test_with_lock(many_tls_value_use_allocator_);
    }

    fn use_allocator_after_destructing_() {
        for _ in 0..10 {
            let ptr1 = thread_local_default_allocator().alloc_or_panic(1024);
            unsafe { thread_local_default_allocator().dealloc(ptr1, 1024); }

            deregister_thread_local_default_allocator();
        }
    }

    #[test]
    fn use_allocator_after_destructing() {
        do_test_with_lock(use_allocator_after_destructing_);
    }

    fn use_allocator_after_destructing_with_two_threads_() {
        for _ in 0..10 {
            let ptr1 = thread_local_default_allocator().alloc_or_panic(1024).addr();

            let (sender1, receiver1) = sync_channel(1);

            thread::spawn(move || {
                let ptr1 = NonNull::new(ptr1.get() as *mut u8).unwrap();

                receiver1.recv().unwrap();

                unsafe { thread_local_default_allocator().dealloc(ptr1, 1024); }
            });

            deregister_thread_local_default_allocator();

            sender1.send(()).unwrap();

            // and now we can either wait until the deallocation or allocate a new ptr in a new
            // thread local allocator
        }
    }

    #[test]
    fn use_allocator_after_destructing_with_two_threads() {
        do_test_with_lock(use_allocator_after_destructing_with_two_threads_);
    }

    #[derive(Default)]
    struct ZeroSizedNLPM {}

    static WAS_DESTRUCTED: AtomicBool = AtomicBool::new(false);

    impl NonLocalPtrManager for ZeroSizedNLPM {
        const SHOULD_AUTO_CHECK_FOR_INCOMING_NON_LOCAL_PTRS: bool = true;

        fn deallocate_non_local_ptr(&self, _allocator: &ThreadLocalAllocator<Self>, _other_manager_ptr: &Self, _non_local_ptr_data: NonLocalPtrData) {
            unreachable!()
        }

        fn check_for_incoming_non_local_ptrs(&self, _allocator: &ThreadLocalAllocator<Self>, _non_local_ptrs_handle: impl FnMut(&[NonLocalPtrData])) {
            return;
        }

        fn self_allocated_memory<'iter>(&'iter self) -> impl Iterator<Item=usize> + 'iter {
            std::iter::empty()
        }

        fn self_drop(&mut self) {
            WAS_DESTRUCTED.store(true, Ordering::SeqCst);
        }
    }


    fn with_zero_sized_nlpm_() {
        deregister_thread_local_default_allocator();

        let mut allocator = ThreadLocalAllocator::<ZeroSizedNLPM>::new();

        let ptr = allocator.alloc_or_panic(8);
        unsafe { allocator.dealloc(ptr, 8); }

        allocator.destructor();

        assert!(WAS_DESTRUCTED.load(Ordering::SeqCst));
    }

    #[test]
    fn with_zero_sized_nlpm() {
        do_test_with_lock(with_zero_sized_nlpm_);
    }

    // TODO aligment
}