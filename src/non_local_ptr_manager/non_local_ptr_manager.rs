use std::hint::black_box;
use std::mem::offset_of;
use std::{mem, ptr};
use std::ptr::NonNull;
use orengine_utils::hints::unwrap_or_bug_hint;
use crate::ThreadLocalAllocatorInner;
use crate::self_allocated_structs::SelfAllocatedVec;
use crate::ThreadLocalAllocator;

/// An offset from the start of allocator to a pointers slot
/// in a pointer table (common or large).
///
/// It looks similar to an index, but it allows us not to check if we need to look at a large or
/// common table.
///
/// We cannot directly use a pointer to a slot because the table can be moved in the destructor
/// or the allocator.
#[derive(Copy, Clone, Debug)]
pub(crate) struct OffsetToSlotInTable {
    is_common: bool,
    idx: usize
}

impl OffsetToSlotInTable {
    pub(crate) const fn new_for_common_chunk(idx: usize) -> Self {
        Self {
            is_common: true,
            idx
        }
    }

    pub(crate) const fn new_for_large_chunk(idx: usize) -> Self {
        Self {
            is_common: false,
            idx
        }
    }
    
    pub(crate) fn is_common(self) -> bool {
        self.is_common
    }

    pub(crate) const fn random() -> Self {
        Self {
            is_common: false,
            idx: 16_000_000
        }
    }

    pub(crate) fn get_vec<NLPM: NonLocalPtrManager>(
        self,
        allocator: &ThreadLocalAllocator<NLPM>
    ) -> &mut SelfAllocatedVec<NonNull<u8>> {
        if self.is_common {
            unwrap_or_bug_hint(allocator.get_inner().common_object_table.get_mut(self.idx))
        } else {
            unsafe { allocator.get_inner().large_object_table.get_vec_mut_by_idx(self.idx) }
        }
    }
}

#[derive(Copy, Clone)]
pub struct NonLocalPtrData {
    ptr: NonNull<u8>,
    offset_to_slot_in_table: OffsetToSlotInTable,
}

impl NonLocalPtrData {
    pub(crate) fn new(ptr: NonNull<u8>, offset_to_slot_in_table: OffsetToSlotInTable) -> Self {
        Self {
            ptr,
            offset_to_slot_in_table,
        }
    }

    pub fn ptr(&self) -> NonNull<u8> {
        self.ptr
    }

    pub(crate) fn offset_to_slot_in_table(self) -> OffsetToSlotInTable {
        self.offset_to_slot_in_table
    }
}

pub trait NonLocalPtrManager: Default + Sync + 'static {
    const SHOULD_AUTO_CHECK_FOR_INCOMING_NON_LOCAL_PTRS: bool;

    fn deallocate_non_local_ptr(
        &self,
        allocator: &ThreadLocalAllocator<Self>,
        other_manager_ptr: &Self,
        non_local_ptr_data: NonLocalPtrData,
    );
    fn check_for_incoming_non_local_ptrs(
        &self,
        allocator: &ThreadLocalAllocator<Self>,
        non_local_ptrs_handle: impl FnMut(&[NonLocalPtrData]),
    );
    fn self_allocated_memory<'iter>(&'iter self) -> impl Iterator<Item=usize> + 'iter;
    fn self_drop(&mut self);
}