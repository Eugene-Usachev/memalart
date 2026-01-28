use std::{iter, mem, ptr};
use std::iter::repeat_n;
use std::mem::{offset_of, ManuallyDrop};
use std::ptr::{null_mut, NonNull};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};
use orengine_utils::cache_padded::CachePaddedAtomicPtr;
use crate::{NonLocalPtrData, NonLocalPtrManager, ThreadLocalAllocator};
use crate::non_local_ptr_manager::OffsetToSlotInTable;
use crate::ThreadLocalAllocatorInner;
use crate::region::{chunk_ptr_to_chunk_metadata_ptr, ChunkMetadata};
use crate::self_allocated_structs::SelfAllocatedVec;
use crate::sizes::{common_size_to_index, COMMON_SIZE_MAX};
use crate::test_assert::test_assert;

struct Node<T> {
    next: *mut Node<T>,
    value: T,
}


pub struct LockFreeNonLocalPtrManager {
    head_of_list: CachePaddedAtomicPtr<Node<NonLocalPtrData>>,
    // TODO #[cfg(test)]
    // TODO not pub
    pub is_deallocated: ManuallyDrop<Arc<(Mutex<bool>, Condvar)>>,
}

impl LockFreeNonLocalPtrManager {
    pub(crate) fn new() -> Self {
        Self {
            head_of_list: CachePaddedAtomicPtr::new(null_mut()),
            // TODO #[cfg(test)]
            is_deallocated: ManuallyDrop::new(Arc::new((Mutex::new(false), Condvar::new()))),
        }
    }

    fn add_new_node_ptr(&self, mut node_ptr: NonNull<Node<NonLocalPtrData>>) {
        let mut head = self.head_of_list.load(Ordering::Relaxed);

        loop {
            unsafe { node_ptr.as_mut().next = head };

            match self.head_of_list.compare_exchange_weak(
                head,
                node_ptr.as_ptr(),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(curr) => head = curr,
            }
        }
    }

    // TODO #[cfg(test)]
    // TODO pub(crate)
    pub fn clone_is_deallocated(&self) -> Arc<(Mutex<bool>, Condvar)> {
        (*self.is_deallocated).clone()
    }

    #[cfg(test)]
    pub(crate) fn wait_until_deallocated(&self) {
        let (mutex, condvar) = &**self.is_deallocated;
        let mut guard = mutex.lock().unwrap();
        while !*guard {
            guard = condvar.wait(guard).unwrap();
        }
    }

    // TODO #[cfg(test)]
    fn set_deallocated(&self) {
        let (mutex, condvar) = &**self.is_deallocated;
        let mut guard = mutex.lock().unwrap();
        *guard = true;
        condvar.notify_all();
    }
}

impl Default for LockFreeNonLocalPtrManager {
    fn default() -> Self {
        Self::new()
    }
}

impl NonLocalPtrManager for LockFreeNonLocalPtrManager {
    const SHOULD_AUTO_CHECK_FOR_INCOMING_NON_LOCAL_PTRS: bool = true;

    fn deallocate_non_local_ptr(
        &self,
        allocator: &ThreadLocalAllocator<Self>,
        other_manager_ptr: &Self,
        non_local_ptr_data: NonLocalPtrData,
    ) {
        let mut node_ptr = allocator
            .try_alloc(size_of::<Node<NonLocalPtrData>>())
            .expect("Failed to allocate memory for internal usage")
            .cast::<Node<NonLocalPtrData>>();

        unsafe {
            node_ptr.as_mut().value = non_local_ptr_data
        };

        other_manager_ptr.add_new_node_ptr(node_ptr);
    }

    fn check_for_incoming_non_local_ptrs(
        &self,
        allocator: &ThreadLocalAllocator<Self>,
        mut non_local_ptrs_handle: impl FnMut(&[NonLocalPtrData]),
    ) {
        let head = self.head_of_list.swap(null_mut(), Ordering::Acquire);
        let mut cur = head;

        while !cur.is_null() {
            unsafe {
                let mut node_ptr = NonNull::new_unchecked(cur);
                let node = node_ptr.read();
                if node.value.ptr().addr().get() == cur.addr() {
                    // The pointer has been returned back to us after deallocation a non-local pointer.

                    const OFFSET_TO_NODE_SLOT_IN_TABLE: OffsetToSlotInTable = OffsetToSlotInTable::new_for_common_chunk(common_size_to_index(size_of::<Node<NonLocalPtrData>>() as u16));

                    let vec = OFFSET_TO_NODE_SLOT_IN_TABLE.get_vec(allocator);

                    vec
                        .push(node_ptr.cast(), allocator, None)
                        .expect("Failed to allocate memory for internal usage");

                    cur = node.next;

                    continue;
                }

                non_local_ptrs_handle(&[node.value]);

                // Time to deallocate node_ptr.
                // It is a non-local pointer.
                // And we need to be careful not to create an infinity deallocation cycle.

                let metadata: ChunkMetadata = chunk_ptr_to_chunk_metadata_ptr(node_ptr.cast()).read();

                if cfg!(test) {
                    #[allow(useless_ptr_null_checks, reason = "It is better to be checked.")]
                    {
                        test_assert!(!(metadata.non_local_ptr_manager() as *const Self).is_null());
                    }
                }

                let other_nlpm: &Self = metadata.non_local_ptr_manager();
                let non_local_ptr_data = NonLocalPtrData::new(node_ptr.cast(), OffsetToSlotInTable::random());

                node_ptr.as_mut().value = non_local_ptr_data;

                other_nlpm.add_new_node_ptr(node_ptr);

                cur = node.next;
            }
        }
    }

    fn self_allocated_memory<'iter>(&'iter self) -> impl Iterator<Item=usize> + 'iter {
        iter::empty()
    }

    fn self_drop(&mut self) {
        #[cfg(test)]
        {
            assert!(!*self.is_deallocated.0.lock().unwrap());

            unsafe { ManuallyDrop::drop(&mut self.is_deallocated); }

            self.set_deallocated();
        }
    }
}
