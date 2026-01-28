use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;
use crate::AllocatorForSelfAllocations;
use crate::sizes::{recommended_size, SizeClass};

#[derive(Debug)]
pub struct SelfAllocatedBox<T> {
    ptr: NonNull<T>,
}

impl<T> SelfAllocatedBox<T> {
    pub(crate) const fn real_size() -> usize {
        recommended_size(size_of::<T>())
    }
    
    pub(crate) const fn from_ptr(ptr: NonNull<T>) -> Self {
        Self { ptr }
    }

    fn alloc_with<A: AllocatorForSelfAllocations, const WITH_COUNTING_LIMIT: bool>(
        allocator: &A,
        maybe_state: Option<&mut A::State>
    ) -> Option<NonNull<T>> {
        assert_ne!(size_of::<T>(), 0, "Zero-sized types are not supported");

        let size = Self::real_size();

        let alloc_res = if WITH_COUNTING_LIMIT {
            allocator.self_alloc(size, maybe_state)
        } else {
            allocator.self_alloc_without_counting_limit(size, maybe_state)
        };

        alloc_res.map(NonNull::cast)
    }

    fn new_<A: AllocatorForSelfAllocations, const WITH_COUNTING_LIMIT: bool>(
        value: T,
        allocator: &A,
        maybe_state: Option<&mut A::State>
    ) -> Result<Self, T> {
        let ptr = Self::alloc_with::<A, WITH_COUNTING_LIMIT>(allocator, maybe_state);

        match ptr {
            Some(ptr) => unsafe {
                ptr.as_ptr().write(value);

                Ok(Self { ptr })
            },
            None => Err(value),
        }
    }

    pub(crate) fn new<A: AllocatorForSelfAllocations>(
        value: T,
        allocator: &A,
        maybe_state: Option<&mut A::State>
    ) -> Result<Self, T> {
        Self::new_::<A, true>(value, allocator, maybe_state)
    }

    pub(crate) fn new_without_counting_limit<A: AllocatorForSelfAllocations>(
        value: T,
        allocator: &A,
        maybe_state: Option<&mut A::State>
    ) -> Result<Self, T> {
        Self::new_::<A, false>(value, allocator, maybe_state)
    }

    pub(crate) fn as_ref(&self) -> &T {
        unsafe { self.ptr.as_ref() }
    }

    pub(crate) fn as_mut(&mut self) -> &mut T {
        unsafe { self.ptr.as_mut() }
    }

    pub(crate) fn into_raw(self) -> NonNull<T> {
        ManuallyDrop::new(self).ptr
    }

    pub(crate) fn dealloc<A: AllocatorForSelfAllocations>(
        &mut self,
        allocator: &A,
        maybe_state: Option<&mut A::State>
    ) {
        unsafe {
            self.ptr.as_ptr().drop_in_place();

            allocator.self_dealloc(
                self.ptr.cast(),
                size_of::<T>(),
                maybe_state
            );
        }
    }

    pub(crate) fn force_become_null(&mut self) {
        #[cfg(test)]
        {
            self.ptr = NonNull::dangling();
        }
    }
}

impl<T> Deref for SelfAllocatedBox<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl<T> DerefMut for SelfAllocatedBox<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use crate::ThreadLocalAllocatorFake;

    #[derive(Debug)]
    struct DropCounter {
        was_dropped: &'static AtomicBool,
        value: u32,
    }

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.was_dropped.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn test_new_and_dealloc_common_size() {
        let allocator = ThreadLocalAllocatorFake::new();

        let mut boxed = SelfAllocatedBox::new(42u64, &allocator, None)
            .expect("Allocation should succeed");

        assert_eq!(*boxed, 42);
        assert_eq!(*boxed.as_ref(), 42);

        boxed.dealloc(&allocator, None);
    }

    #[test]
    fn test_new_and_dealloc_with_struct() {
        static WAS_DROPPED: AtomicBool = AtomicBool::new(false);
        let allocator = ThreadLocalAllocatorFake::new();

        let counter = DropCounter {
            was_dropped: &WAS_DROPPED,
            value: 123,
        };

        let mut boxed = SelfAllocatedBox::new(counter, &allocator, None)
            .expect("Allocation should succeed");

        assert_eq!(boxed.value, 123);
        assert!(!WAS_DROPPED.load(Ordering::SeqCst));

        boxed.dealloc(&allocator, None);

        // Drop should have been called during dealloc
        assert!(WAS_DROPPED.load(Ordering::SeqCst));
    }

    #[test]
    fn test_new_without_counting_limit() {
        let allocator = ThreadLocalAllocatorFake::new();

        let mut boxed = SelfAllocatedBox::new_without_counting_limit(
            999u32,
            &allocator,
            None
        ).expect("Allocation should succeed");

        assert_eq!(*boxed, 999);

        boxed.dealloc(&allocator, None);
    }

    #[test]
    fn test_as_ref_and_as_mut() {
        let allocator = ThreadLocalAllocatorFake::new();

        let mut boxed = SelfAllocatedBox::new(100u32, &allocator, None)
            .expect("Allocation should succeed");

        assert_eq!(*boxed.as_ref(), 100);

        *boxed.as_mut() = 200;
        assert_eq!(*boxed.as_ref(), 200);

        boxed.dealloc(&allocator, None);
    }

    #[test]
    fn test_deref_and_deref_mut() {
        let allocator = ThreadLocalAllocatorFake::new();

        let mut boxed = SelfAllocatedBox::new(50u64, &allocator, None)
            .expect("Allocation should succeed");

        assert_eq!(*boxed, 50);

        *boxed = 75;
        assert_eq!(*boxed, 75);

        boxed.dealloc(&allocator, None);
    }

    #[test]
    fn test_large_struct() {
        let allocator = ThreadLocalAllocatorFake::new();

        // Test with a large struct
        #[derive(Debug, PartialEq)]
        struct LargeStruct {
            data: [u8; 4090],
        }

        let large = LargeStruct { data: [42; 4090] };
        let mut boxed = SelfAllocatedBox::new(large, &allocator, None)
            .expect("Allocation should succeed");

        assert_eq!(boxed.data[0], 42);
        assert_eq!(boxed.data[4080], 42);

        boxed.data[500] = 99;
        assert_eq!(boxed.data[500], 99);

        boxed.dealloc(&allocator, None);
    }

    #[test]
    fn test_real_size_calculations() {
        // Test various sizes to ensure size class calculation is correct
        assert!(SelfAllocatedBox::<u8>::real_size() >= size_of::<u8>());
        assert!(SelfAllocatedBox::<[u8; 8]>::real_size() >= size_of::<[u8; 8]>());
        assert!(SelfAllocatedBox::<[u8; 64]>::real_size() >= size_of::<[u8; 64]>());
        assert!(SelfAllocatedBox::<[u8; 512]>::real_size() >= size_of::<[u8; 512]>());
        assert!(SelfAllocatedBox::<[u8; 4000]>::real_size() >= size_of::<[u8; 4000]>());
    }
}