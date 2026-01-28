use std::ptr::NonNull;
#[cfg(test)]
use std::alloc::Layout;
use std::cell::Cell;
use crate::region::RawRegion;

// TODO pub(crate)
pub trait AllocatorForSelfAllocations {
    type State;

    fn state_from_region(region: NonNull<RawRegion>) -> Self::State;

    fn self_alloc(
        &self,
        size: usize,
        maybe_state: Option<&mut Self::State>
    ) -> Option<NonNull<u8>>;

    fn self_alloc_without_counting_limit(
        &self, 
        size: usize, 
        maybe_state: Option<&mut Self::State>
    ) -> Option<NonNull<u8>>;
    unsafe fn self_dealloc(
        &self,
        ptr: NonNull<u8>,
        size: usize,
        maybe_state: Option<&mut Self::State>
    );
}

#[cfg(test)]
pub(crate) struct ThreadLocalAllocatorFake {
    allocated_memory: Cell<usize>,
}

#[cfg(test)]
impl ThreadLocalAllocatorFake {
    pub(crate) fn new() -> Self {
        Self { allocated_memory: Cell::new(0) }
    }

    pub(crate) fn allocated_memory(&self) -> usize {
        self.allocated_memory.get()
    }
}

#[cfg(test)]
impl AllocatorForSelfAllocations for ThreadLocalAllocatorFake {
    type State = NonNull<RawRegion>;

    fn state_from_region(region: NonNull<RawRegion>) -> Self::State {
        region
    }

    fn self_alloc(
        &self,
        size: usize,
        maybe_state: Option<&mut Self::State>
    ) -> Option<NonNull<u8>> {
        Self::self_alloc_without_counting_limit(self, size, maybe_state)
    }

    fn self_alloc_without_counting_limit(
        &self,
        size: usize,
        _maybe_state: Option<&mut Self::State>
    ) -> Option<NonNull<u8>> {
        self.allocated_memory.set(self.allocated_memory.get() + size);

        NonNull::new(unsafe { std::alloc::alloc(Layout::from_size_align_unchecked(size, 8)) })
    }

    unsafe fn self_dealloc(&self, ptr: NonNull<u8>, size: usize, _maybe_state: Option<&mut Self::State>) {
        let old = self.allocated_memory.get();

        assert!(old >= size, "Attempt to deallocate more memory than allocated in `ThreadLocalAllocatorFake`.");

        self.allocated_memory.set(old - size);

        std::alloc::dealloc(ptr.as_ptr(), Layout::from_size_align_unchecked(size, 8))
    }
}