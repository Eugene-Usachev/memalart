mod chunk_manager;
mod thread_local_allocator;
mod allocator_for_self_allocations;
mod thread_local_self_alloc_impl;
mod numbers_of_used_objects_with_size;

pub type DefaultNonLocalPtrManager = crate::non_local_ptr_manager::LockFreeNonLocalPtrManager;

pub type DefaultThreadLocalAllocator = ThreadLocalAllocator<DefaultNonLocalPtrManager>;

use std::mem::ManuallyDrop;
pub use thread_local_allocator::*;

pub(crate) use allocator_for_self_allocations::*;

#[allow(
    unused,
    reason = "I want it to be easy to understand what's going on even if some platforms uses it implicitly"
)]
pub(super) type DefaultThreadLocalAllocatorForTLS = ManuallyDrop<DefaultThreadLocalAllocator>;

const _: () = {
    assert!(
        !core::mem::needs_drop::<DefaultThreadLocalAllocatorForTLS>(),
        "DefaultThreadLocalAllocatorForTLS needs drop"
    );
};

const _: () = {
    struct LargeNLPM {
        data: [u8; 123456],
    }
    
    impl Default for LargeNLPM {
        fn default() -> Self {
            Self {
                data: [0; 123456],
            }
        }
    }
    
    impl NonLocalPtrManager for LargeNLPM {
        const SHOULD_AUTO_CHECK_FOR_INCOMING_NON_LOCAL_PTRS: bool = false;

        fn deallocate_non_local_ptr(
            &self,
            _allocator: &ThreadLocalAllocator<Self>,
            _other_manager_ptr: &Self,
            _non_local_ptr_data: NonLocalPtrData
        ) {
            unimplemented!()
        }

        fn check_for_incoming_non_local_ptrs(
            &self,
            _allocator: &ThreadLocalAllocator<Self>,
            _non_local_ptrs_handle: impl FnMut(&[NonLocalPtrData])
        ) {
            unimplemented!()
        }

        fn self_allocated_memory<'iter>(&'iter self) -> impl Iterator<Item=usize> + 'iter {
            std::iter::empty()
        }

        fn self_drop(&mut self) {
            unimplemented!()
        }
    }
    
    assert!(
        size_of::<DefaultThreadLocalAllocatorForTLS>() == size_of::<ThreadLocalAllocator<LargeNLPM>>(),
        "`ThreadLocalAllocator` size should not depend on the size of `NonLocalPtrManager`"
    )  
};

mod tls;
mod info_for_size;

pub use tls::*;
use crate::{NonLocalPtrData, NonLocalPtrManager};
pub use info_for_size::*;