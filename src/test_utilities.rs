use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::Duration;
use crate::limit::{load_allocated, set_allocator_limit};
use crate::{DefaultThreadLocalAllocator};
use crate::thread_local_default_allocator_mut;

static TEST_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn acquire_test_lock() -> MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|e| {
        TEST_LOCK.clear_poison();

        e.into_inner()
    })
}


pub(crate) fn do_test_with_lock(f: impl FnOnce()) {
    let test_lock = acquire_test_lock();

    set_allocator_limit(crate::limit::TEST_LIMIT);

    assert_eq!(
        load_allocated(),
        0,
        "Memory has not been deallocated before the test start, do not count this test."
    );

    f();

    let mut allocated = load_allocated();

    for _ in 0..3000 { // Let other threads spawned in this test release the memory.
        if allocated == 0 {
            break;
        }

        thread::sleep(Duration::from_millis(1));

        allocated = load_allocated();
    }


    assert_eq!(allocated, 0);

    drop(test_lock);
}

#[cfg(test)]
pub(crate) fn do_test_default_thread_local_allocator(f: impl FnOnce(&mut DefaultThreadLocalAllocator)) {
    do_test_with_lock(|| {
        let mut allocator = unsafe { thread_local_default_allocator_mut() };

        f(&mut allocator);

        if !allocator.is_already_destructured() {
            assert!(allocator.try_free_all_memory().is_ok());
        }
    });
}