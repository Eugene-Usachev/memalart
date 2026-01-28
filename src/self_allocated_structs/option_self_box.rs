use std::ptr::NonNull;
use crate::AllocatorForSelfAllocations;
use crate::self_allocated_structs::SelfAllocatedBox;
use crate::test_assert::test_assert;

pub(crate) struct OptionSelfAllocatedBox<T> {
    inner: Option<SelfAllocatedBox<T>>,
}

const RANDOM_PTR: *mut () = 0x12345678 as *mut ();

impl<T> OptionSelfAllocatedBox<T> {
    pub(crate) fn some(value: SelfAllocatedBox<T>) -> Self {
        Self { inner: Some(value) }
    }

    pub(crate) const fn none() -> Self {
        Self { inner: None }
    }
    
    pub(crate) const fn random_ptr() -> Self {
        Self {
            inner: Some(SelfAllocatedBox::from_ptr(unsafe { NonNull::new_unchecked(RANDOM_PTR.cast()) }))
        }
    }

    pub(crate) fn new<A: AllocatorForSelfAllocations>(
        value: T,
        allocator: &A,
        maybe_state: Option<&mut A::State>
    ) -> Result<Self, T> {
        SelfAllocatedBox::new(value, allocator, maybe_state)
            .map(|boxed| Self { inner: Some(boxed) })
    }
    
    pub(crate) fn from_ptr(ptr: NonNull<T>) -> Self {
        Self {
            inner: Some(SelfAllocatedBox::from_ptr(ptr))
        }
    }

    pub(crate) fn as_ptr(&self) -> Option<NonNull<T>> {
        self
            .inner
            .as_ref()
            .map(|b| NonNull::from(&**b))
    }

    pub(crate) const fn is_null(&self) -> bool {
        self.inner.is_none()
    }

    pub(crate) fn as_ref(&self) -> Option<&T> {
        test_assert!(
            self.is_null() || self.as_ptr().unwrap().as_ptr() != RANDOM_PTR.cast(),
            "Random ptr is used in OptionSelfAllocatedBox::as_ref() which is not allowed."
        );
        
        self.inner.as_ref().map(|b| b.as_ref())
    }

    pub(crate) fn as_mut(&mut self) -> Option<&mut T> {
        test_assert!(
            self.is_null() || self.as_ptr().unwrap().as_ptr() != RANDOM_PTR.cast(),
            "Random ptr is used in OptionSelfAllocatedBox::as_mut() which is not allowed."
        );
        
        self.inner.as_mut().map(|b| b.as_mut())
    }

    pub(crate) fn take(&mut self) -> Option<SelfAllocatedBox<T>> {
        self.inner.take()
    }

    pub(crate) unsafe fn force_set_null(&mut self) {
        if let Some(ref mut boxed) = self.inner {
            boxed.force_become_null();
        }

        self.inner = None;
    }

    #[cfg(test)]
    pub(crate) fn dealloc_if_needed<A: AllocatorForSelfAllocations>(
        &mut self,
        allocator: &A,
        maybe_state: Option<&mut A::State>
    ) {
        test_assert!(
            self.is_null() || self.as_ptr().unwrap().as_ptr() != RANDOM_PTR.cast(),
            "Random ptr is used in OptionSelfAllocatedBox::dealloc_if_needed() which is not allowed."
        );
        
        if let Some(ref mut boxed) = self.inner {
            boxed.dealloc(allocator, maybe_state);
        }

        self.inner = None;
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
    }

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.was_dropped.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn test_none() {
        let opt_box: OptionSelfAllocatedBox<u32> = OptionSelfAllocatedBox::none();
        assert!(opt_box.is_null());
        assert!(opt_box.as_ref().is_none());
    }

    #[test]
    fn test_some() {
        let allocator = ThreadLocalAllocatorFake::new();

        let mut opt_box = OptionSelfAllocatedBox::new(42u32, &allocator, None)
            .expect("Allocation should succeed");

        assert!(!opt_box.is_null());
        assert_eq!(opt_box.as_ref(), Some(&42));

        opt_box.dealloc_if_needed(&allocator, None);
    }

    #[test]
    fn test_as_ref_and_as_mut() {
        let allocator = ThreadLocalAllocatorFake::new();

        let mut opt_box = OptionSelfAllocatedBox::new(100u64, &allocator, None)
            .expect("Allocation should succeed");

        assert_eq!(opt_box.as_ref(), Some(&100));

        if let Some(val) = opt_box.as_mut() {
            *val = 200;
        }

        assert_eq!(opt_box.as_ref(), Some(&200));

        opt_box.dealloc_if_needed(&allocator, None);
    }

    #[test]
    fn test_is_null() {
        let allocator = ThreadLocalAllocatorFake::new();

        let opt_box_none: OptionSelfAllocatedBox<u32> = OptionSelfAllocatedBox::none();
        assert!(opt_box_none.is_null());

        let mut opt_box_some = OptionSelfAllocatedBox::new(42u32, &allocator, None)
            .expect("Allocation should succeed");
        assert!(!opt_box_some.is_null());

        opt_box_some.dealloc_if_needed(&allocator, None);
    }

    #[test]
    fn test_dealloc_with_drop() {
        static WAS_DROPPED: AtomicBool = AtomicBool::new(false);
        let allocator = ThreadLocalAllocatorFake::new();

        let counter = DropCounter {
            was_dropped: &WAS_DROPPED,
        };

        let mut opt_box = OptionSelfAllocatedBox::new(counter, &allocator, None)
            .expect("Allocation should succeed");

        assert!(!WAS_DROPPED.load(Ordering::SeqCst));

        opt_box.dealloc_if_needed(&allocator, None);

        assert!(WAS_DROPPED.load(Ordering::SeqCst));
        assert!(opt_box.is_null());
    }

    #[test]
    fn test_dealloc_if_needed_on_none() {
        let allocator = ThreadLocalAllocatorFake::new();

        let mut opt_box: OptionSelfAllocatedBox<u32> = OptionSelfAllocatedBox::none();

        // Should not panic
        opt_box.dealloc_if_needed(&allocator, None);

        assert!(opt_box.is_null());
    }

    #[test]
    fn test_some_with_existing_box() {
        let allocator = ThreadLocalAllocatorFake::new();

        let boxed = SelfAllocatedBox::new(777u64, &allocator, None)
            .expect("Allocation should succeed");

        let mut opt_box = OptionSelfAllocatedBox::some(boxed);

        assert!(!opt_box.is_null());
        assert_eq!(opt_box.as_ref(), Some(&777));

        opt_box.dealloc_if_needed(&allocator, None);
    }
}