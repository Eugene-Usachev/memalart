use std::fmt::{Debug, Formatter};
use std::marker::PhantomData;
use std::{fmt, ptr, slice};
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;
use orengine_utils::hints::{likely, unlikely, unwrap_or_bug_hint};
use crate::sizes::{recommended_size, COMMON_SIZE_MAX, LARGE_SIZE_MAX};
use crate::test_assert::test_assert;
use crate::AllocatorForSelfAllocations;

pub(crate) struct SelfAllocatedVec<T> {
    ptr: *mut T,
    len: u32,
    cap: u32,
}

impl<T> SelfAllocatedVec<T> {
    pub(crate) const fn new() -> Self {
        Self { ptr: ptr::null_mut(), len: 0, cap: 0 }
    }

    pub(crate) fn len(&self) -> usize {
        self.len as usize
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn capacity(&self) -> usize {
        self.cap as usize
    }

    pub(crate) fn vacant(&self) -> usize {
        self.cap as usize - self.len as usize
    }
    
    pub(crate) fn bytes_allocated(&self) -> usize {
        let size = self.cap as usize * size_of::<T>();

        recommended_size(size)
    }
    
    pub(crate) fn is_null(&self) -> bool {
        self.ptr.is_null()
    }

    fn alloc_with<A: AllocatorForSelfAllocations, const WITH_COUNTING_LIMIT: bool>(
        mut min_cap: u32,
        allocator: &A,
        maybe_state: Option<&mut A::State>
    ) -> (Option<NonNull<T>>, u32) {
        let mut size = (min_cap as usize) * size_of::<T>();

        size = recommended_size(size.next_power_of_two());
        min_cap = (size / size_of::<T>()) as u32;

        let alloc_res = if WITH_COUNTING_LIMIT {
            allocator.self_alloc(size, maybe_state)
        } else {
            allocator.self_alloc_without_counting_limit(size, maybe_state)
        };
        
        if unlikely(alloc_res.is_none()) {
            return (None, min_cap);
        }

        (alloc_res.map(NonNull::cast), min_cap)
    }

    fn maybe_dealloc<A: AllocatorForSelfAllocations>(
        ptr: *mut T,
        cap: u32,
        allocator: &A,
        maybe_state: Option<&mut A::State>
    ) {
        if cap != 0 {
            unsafe {
                allocator.self_dealloc(
                    NonNull::new_unchecked(ptr.cast()),
                    (cap as usize) * size_of::<T>(),
                    maybe_state
                )
            };
        }
    }

    #[cold]
    #[inline(never)]
    fn reserve_slow<A: AllocatorForSelfAllocations, const WITH_COUNTING_LIMIT: bool>(
        &mut self,
        additional_capacity: u32,
        allocator: &A,
        mut maybe_state: Option<&mut A::State>
    ) -> Result<(), ()> {
        let mut min_cap = self.len + additional_capacity;

        test_assert!(min_cap > self.cap);

        loop {
            let (new_ptr_, new_cap) = Self::alloc_with::<A, WITH_COUNTING_LIMIT>(
                min_cap,
                allocator,
                maybe_state.as_deref_mut()
            );

            min_cap = self.len + additional_capacity;

            if unlikely(min_cap > new_cap) {
                if let Some(new_ptr) = new_ptr_ {
                    Self::maybe_dealloc(
                        new_ptr.as_ptr(),
                        new_cap,
                        allocator,
                        maybe_state.as_deref_mut()
                    );
                }

                // During this allocation, somebody has added more elements
                // Retry
                continue;
            }

            let Some(new_ptr) = new_ptr_ else {
                return Err(());
            };

            if !self.is_empty() {
                unsafe {
                    ptr::copy_nonoverlapping(self.ptr, new_ptr.as_ptr(), self.len as usize);
                }
            }

            let old_ptr = self.ptr;
            let old_cap = self.cap;

            // The current thread may reborrow mutable self multiple times
            // in `maybe_dealloc` with a `State`.
            // So we need to first update `self` and then deallocate memory.
            // It means that these four lines of code cannot be reordered in the future.
            // Never try to write
            // ```
            // Self::maybe_dealloc(self.ptr, self.cap, allocator, maybe_state.as_deref_mut());
            // self.cap = new_cap;
            // self.ptr = new_ptr;
            // ```
            // `
            // It is illegal for Rust, but it is the most performance way to do it.

            self.cap = new_cap;
            self.ptr = new_ptr.as_ptr();

            Self::maybe_dealloc(old_ptr, old_cap, allocator, maybe_state.as_deref_mut());

            min_cap = self.len + additional_capacity;
            if min_cap > new_cap {
                // TODO is it possible?

                // While alloc somebody has added more elements
                // Retry
                continue;
            }

            break Ok(());
        }
    }

    /// Returns if it allocated memory or not.
    #[inline]
    pub(crate) fn reserve<A: AllocatorForSelfAllocations>(
        &mut self,
        additional_capacity: u32,
        allocator: &A,
        maybe_state: Option<&mut A::State>
    ) -> Result<bool, ()> {
        if likely(self.cap >= self.len + additional_capacity) {
            return Ok(false);
        }

        self.reserve_slow::<A, true>(additional_capacity, allocator, maybe_state).map(|_| true)
    }

    /// Returns if it allocated memory or not.
    #[inline]
    pub(crate) fn reserve_without_counting_limit<A: AllocatorForSelfAllocations>(
        &mut self,
        additional_capacity: u32,
        allocator: &A,
        maybe_state: Option<&mut A::State>
    ) -> Result<bool, ()> {
        if likely(self.cap >= self.len + additional_capacity) {
            return Ok(false);
        }
        
        self.reserve_slow::<A, false>(additional_capacity, allocator, maybe_state).map(|_| true)
    }

    pub(crate) fn try_push(&mut self, elem: T) -> Result<(), T> {
        if unlikely(self.vacant() == 0) {
            return Err(elem);
        }
        
        unsafe { self.ptr.add(self.len as usize).write(elem) };

        self.len += 1;

        Ok(())
    }
    
    #[inline]
    pub(crate) fn push<A: AllocatorForSelfAllocations>(
        &mut self,
        elem: T,
        allocator: &A,
        maybe_state: Option<&mut A::State>
    ) -> Result<(), ()> {
        let res = self.reserve(1, allocator, maybe_state);
        if unlikely(res.is_err()) {
            return Err(());
        }

        unsafe { self.ptr.add(self.len as usize).write(elem) };

        self.len += 1;

        Ok(())
    }

    pub(crate) fn push_without_counting_limit<A: AllocatorForSelfAllocations>(
        &mut self,
        elem: T,
        allocator: &A,
        maybe_state: Option<&mut A::State>
    ) -> Result<(), ()> {
        let res = self.reserve_without_counting_limit(1, allocator, maybe_state);
        if unlikely(res.is_err()) {
            return Err(());
        }

        unsafe { self.ptr.add(self.len as usize).write(elem) };

        self.len += 1;

        Ok(())
    }

    #[inline]
    pub(crate) fn pop(&mut self) -> Option<T> {
        if likely(self.len > 0) {
            test_assert!(!self.ptr.is_null());

            self.len -= 1;

            return Some(unsafe { self.ptr.add(self.len as usize).read() });
        }

        None
    }

    pub(crate) fn try_extend_from_slice(&mut self, slice: &[T]) -> Result<(), ()> {
        if unlikely(self.len + slice.len() as u32 > self.cap) {
            return Err(());
        }

        unsafe {
            self.ptr.add(self.len as usize).copy_from_nonoverlapping(slice.as_ptr(), slice.len())
        };

        self.len += slice.len() as u32;

        Ok(())
    }

    pub(crate) fn extend_from_slice<A: AllocatorForSelfAllocations>(
        &mut self,
        slice: &[T],
        allocator: &A,
        maybe_state: Option<&mut A::State>
    ) -> Result<(), ()> {
        let res = self.reserve(slice.len() as u32, allocator, maybe_state);
        if unlikely(res.is_err()) {
            return Err(());
        }

        unsafe {
            self.ptr.add(self.len as usize).copy_from_nonoverlapping(slice.as_ptr(), slice.len())
        };

        self.len += slice.len() as u32;

        Ok(())
    }

    pub(crate) fn swap_remove(&mut self, index: usize) -> T {
        test_assert!((self.len as usize) > index);

        let item = unsafe { self.ptr.add(index).read() };

        self.len = self.len - 1;

        unsafe { self.ptr.add(index).write(self.ptr.add(self.len as usize).read()) };

        item
    }

    pub(crate) fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        if likely(index < self.len as usize) {
            test_assert!(!self.ptr.is_null());

            return Some(unsafe { &mut *self.ptr.add(index) });
        }

        None
    }
    
    pub(crate) fn clear_with<A: AllocatorForSelfAllocations>(
        &mut self,
        mut f: impl FnMut(T),
        allocator: &A,
        maybe_state: Option<&mut A::State>
    ) {
        for i in 0..self.len as usize {
            unsafe {
                f(self.ptr.add(i).read());
            }
        }

        Self::maybe_dealloc(self.ptr, self.cap, allocator, maybe_state);

        self.len = 0;
        self.cap = 0;
        self.ptr = ptr::null_mut();
    }

    #[cfg(test)]
    pub(crate) fn clear<A: AllocatorForSelfAllocations>(
        &mut self,
        allocator: &A,
        maybe_state: Option<&mut A::State>
    ) {
        if core::mem::needs_drop::<T>() {
            for i in 0..self.len as usize {
                unsafe {
                    self.ptr.add(i).drop_in_place();
                }
            }
        }

        Self::maybe_dealloc(self.ptr, self.cap, allocator, maybe_state);

        self.len = 0;
        self.cap = 0;
        self.ptr = ptr::null_mut();
    }

    pub(crate) unsafe fn force_become_null(&mut self) {
        self.ptr = ptr::null_mut();
        self.len = 0;
        self.cap = 0;
    }

    pub(crate) fn iter<'iter>(&'iter self) -> impl Iterator<Item = &'iter T>
        where T: 'iter
    {
        struct Iter<'iter, T> {
            curr: *const T,
            end: *const T,
            phantom_data: PhantomData<&'iter ()>
        }

        impl<'iter, T: 'iter> Iterator for Iter<'iter, T> {
            type Item = &'iter T;

            fn next(&mut self) -> Option<Self::Item> {
                if self.curr != self.end {
                    let res = unsafe { &*self.curr };

                    self.curr = unsafe { self.curr.add(1) };

                    return Some(res);
                }

                None
            }
        }

        let curr = self.ptr;
        let end = unsafe { curr.add(self.len as usize) };

        Iter { curr, end, phantom_data: PhantomData }
    }

    pub(crate) fn iter_mut<'iter>(&'iter mut self) -> impl Iterator<Item = &'iter mut T>
    where T: 'iter
    {
        struct IterMut<'iter, T> {
            curr: *mut T,
            end: *mut T,
            phantom_data: PhantomData<&'iter mut ()>
        }

        impl<'iter, T: 'iter> Iterator for IterMut<'iter, T> {
            type Item = &'iter mut T;

            fn next(&mut self) -> Option<Self::Item> {
                if self.curr != self.end {
                    let res = unsafe { &mut *self.curr };

                    self.curr = unsafe { self.curr.add(1) };

                    return Some(res);
                }

                None
            }
        }

        let curr = self.ptr;
        let end = unsafe { curr.add(self.len as usize) };

        IterMut { curr, end, phantom_data: PhantomData }
    }
}

impl<T> Deref for SelfAllocatedVec<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        unsafe { slice::from_raw_parts(self.ptr, self.len as usize) }
    }
}

impl<T> DerefMut for SelfAllocatedVec<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { slice::from_raw_parts_mut(self.ptr, self.len as usize) }
    }
}

impl<T: Debug> Debug for SelfAllocatedVec<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Debug::fmt(&**self, f)
    }
}

#[cfg(test)]
impl<T> Drop for SelfAllocatedVec<T> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            return;
        }
        
        test_assert!(
            self.len == 0 && self.ptr.is_null(),
            "SelfAllocatedVec should be empty on drop"
        );
    }
}

#[cfg(test)]
mod tests {
    use crate::ThreadLocalAllocatorFake;
    use super::SelfAllocatedVec;

    #[test]
    fn test_vec_new_zero_state() {
        let v: SelfAllocatedVec<i32> = SelfAllocatedVec::new();

        assert_eq!(v.len(), 0);
        assert_eq!(v.capacity(), 0);

        assert!(v.is_empty());
    }

    #[test]
    fn test_vec_push_and_pop() {
        const N: usize = 100_000;

        let mut v = SelfAllocatedVec::new();
        let mut allocator = ThreadLocalAllocatorFake::new();

        for i in 0..N {
            v.push(i, &mut allocator, None).unwrap();
        }

        assert_eq!(v.len(), N);

        for i in (0..N).rev() {
            assert_eq!(v.pop(), Some(i));
        }

        assert_eq!(v.len(), 0);

        v.clear(&mut allocator, None);
    }

    #[test]
    fn test_vec_reserve() {
        let mut v: SelfAllocatedVec<usize> = SelfAllocatedVec::new();
        let mut allocator = ThreadLocalAllocatorFake::new();

        v.reserve(10, &mut allocator, None).unwrap();

        assert!(v.capacity() >= 10);

        v.clear(&mut allocator, None);
    }

    #[test]
    fn test_vec_extend_from_slice() {
        let mut v = SelfAllocatedVec::new();
        let mut allocator = ThreadLocalAllocatorFake::new();

        v.extend_from_slice(&[1, 2, 3, 4], &mut allocator, None).unwrap();

        assert_eq!(v.len(), 4);

        let collected: Vec<_> = v.iter().cloned().collect();

        assert_eq!(collected, vec![1, 2, 3, 4]);

        v.clear(&mut allocator, None);
    }

    #[test]
    fn test_vec_swap_remove() {
        let mut v = SelfAllocatedVec::new();
        let mut allocator = ThreadLocalAllocatorFake::new();

        v.push(10, &mut allocator, None).unwrap();
        v.push(20, &mut allocator, None).unwrap();
        v.push(30, &mut allocator, None).unwrap();

        let removed = v.swap_remove(0);

        assert_eq!(removed, 10);
        assert_eq!(v.len(), 2);

        let mut out: Vec<_> = v.iter().cloned().collect();
        out.sort();
        assert_eq!(out, vec![20, 30]);

        v.clear(&mut allocator, None);
    }

    #[test]
    fn test_vec_get_mut() {
        let mut v = SelfAllocatedVec::new();
        let mut allocator = ThreadLocalAllocatorFake::new();

        v.push(10, &mut allocator, None).unwrap();
        v.push(20, &mut allocator, None).unwrap();

        *v.get_mut(1).unwrap() = 999;

        let collected: Vec<_> = v.iter().cloned().collect();

        assert_eq!(collected, vec![10, 999]);

        v.clear(&mut allocator, None);
    }
}