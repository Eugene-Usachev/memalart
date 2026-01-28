use std::alloc::Layout;
use orengine_utils::hints::UnwrapOrPanic;
use crate::DefaultThreadLocalAllocator;

pub fn unwrap_or_bug_message_hint<T>(item: impl UnwrapOrPanic<T>, message: &'static str) -> T {
    orengine_utils::hints::unwrap_or_bug_message_hint(item, message)
}

#[macro_export]
macro_rules! generate_thread_local_allocator_functions {
    (
        $global_allocator_name:ident,
        $thread_local_allocator_type:ident,
        $getter_mut_fn_name:ident,
        $getter_fn_name:ident,
        $deregister_fn_name:ident
    ) => {
        #[inline(always)]
        pub(crate) unsafe fn $getter_mut_fn_name() -> &'static mut $thread_local_allocator_type {
            thread_local! {
                static GEN_ALLOCATOR__: std::cell::UnsafeCell<std::mem::ManuallyDrop<$thread_local_allocator_type >> = const {
                    std::cell::UnsafeCell::new(std::mem::ManuallyDrop::new($thread_local_allocator_type::new()))
                };
            }

            $crate::unwrap_or_bug_message_hint(
                GEN_ALLOCATOR__.try_with(|a| unsafe { &mut *a.get() }),
                "Failed to get the thread-local static allocator."
            )
        }

        #[inline(always)]
        pub fn $getter_fn_name() -> &'static $thread_local_allocator_type {
            unsafe { $getter_mut_fn_name() }
        }

        // TODO is unsafe
        pub fn $deregister_fn_name() {
            unsafe { $crate::deregister_thread_local_allocator_destructor(); }

            unsafe {
                $crate::ThreadLocalAllocator::destructor($getter_mut_fn_name());
            }
        }
        
        struct $global_allocator_name {}
        
        unsafe impl core::alloc::GlobalAlloc for $global_allocator_name {
            unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
                $getter_fn_name()
                    .try_alloc(layout.size())
                    .map(core::ptr::NonNull::as_ptr)
                    .unwrap_or(core::ptr::null_mut())
            }
            
            unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
                unsafe {
                    $getter_fn_name().dealloc(core::ptr::NonNull::new_unchecked(ptr), layout.size());
                }
            }
        }
    };
}

generate_thread_local_allocator_functions!(
    DefaultAllocator,
    DefaultThreadLocalAllocator,
    thread_local_default_allocator_mut,
    thread_local_default_allocator,
    deregister_thread_local_default_allocator
);

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};
    use super::*;

    #[test]
    fn tls_alloc() {
        const NUM_THREADS: usize = 10;

        let ptrs = Arc::new(Mutex::new(Vec::with_capacity(NUM_THREADS)));
        let mut handles = Vec::with_capacity(NUM_THREADS);

        for _ in 0..NUM_THREADS {
            let ptrs = ptrs.clone();

            handles.push(std::thread::spawn(move || {
                let ptr = thread_local_default_allocator() as *const _ as usize;
                let ptr2 = thread_local_default_allocator() as *const _ as usize;

                assert_eq!(
                    ptr, ptr2,
                    "thread_local_allocator returned different pointers"
                );

                ptrs.lock().unwrap().push(ptr);
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let mut ptrs = ptrs.lock().unwrap();

        let mut set = HashSet::new();

        for ptr in ptrs.drain(..) {
            assert!(
                set.insert(ptr),
                "Non-unique ptr was returned from thread_local_allocator"
            );
        }
    }
}
