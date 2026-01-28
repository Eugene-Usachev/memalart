use std::ptr::NonNull;

/// A vector allocated directly from the OS allocator.
pub(crate) struct VecInOs<T> {
    ptr: NonNull<T>,
    len: usize,
    cap: usize,
}

impl<T> VecInOs<T> {
    /// Creates a new empty vector.
    pub(crate) fn new() -> Self {
        Self {
            ptr: NonNull::dangling(),
            len: 0,
            cap: 0,
        }
    }

    /// Reserves capacity for at least `additional` more elements.
    pub(crate) fn reserve(&mut self, additional: usize) {
        let required = self.len.checked_add(additional).expect("capacity overflow");
        if required <= self.cap {
            return;
        }

        let new_cap = required.max(self.cap * 2).max(4);
        let new_size = new_cap * size_of::<T>();

        let new_ptr = unsafe {
            let (ptr, _) = crate::os::alloc_from_os(
                std::ptr::null_mut(),
                new_size,
                true,
                false,
            );
            if ptr.is_null() {
                panic!("allocation failed");
            }
            ptr.cast::<T>()
        };

        if self.cap > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(self.ptr.as_ptr(), new_ptr, self.len);

                crate::os::free(
                    self.ptr.as_ptr().cast(),
                    self.cap * size_of::<T>(),
                );
            }
        }

        self.ptr = NonNull::new(new_ptr).unwrap();
        self.cap = new_cap;
    }

    /// Appends an element to the back of the vector.
    pub(crate) fn push(&mut self, value: T) {
        if self.len == self.cap {
            self.reserve(1);
        }
        unsafe {
            self.ptr.as_ptr().add(self.len).write(value);
        }
        self.len += 1;
    }

    /// Returns an iterator over the vector.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &T> {
        struct Iter<'a, T> {
            ptr: *const T,
            end: *const T,
            _marker: std::marker::PhantomData<&'a T>,
        }

        impl<'a, T> Iterator for Iter<'a, T> {
            type Item = &'a T;

            fn next(&mut self) -> Option<Self::Item> {
                if self.ptr == self.end {
                    None
                } else {
                    unsafe {
                        let item = &*self.ptr;
                        self.ptr = self.ptr.add(1);
                        Some(item)
                    }
                }
            }
        }

        Iter {
            ptr: self.ptr.as_ptr(),
            end: unsafe { self.ptr.as_ptr().add(self.len) },
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T> Drop for VecInOs<T> {
    fn drop(&mut self) {
        if self.cap > 0 {
            unsafe {
                std::ptr::drop_in_place(std::ptr::slice_from_raw_parts_mut(
                    self.ptr.as_ptr(),
                    self.len,
                ));

                crate::os::free(
                    self.ptr.as_ptr().cast(),
                    self.cap * size_of::<T>(),
                );
            }
        }
    }
}