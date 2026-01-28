use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};

static LIMIT: AtomicUsize = AtomicUsize::new(usize::MAX);
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
pub(crate) const MIN_TEST_LIMIT: usize = 2 * 1024 * 1024;
#[cfg(test)]
pub(crate) const TEST_LIMIT: usize = MIN_TEST_LIMIT * 8000;

pub fn set_allocator_limit(size: usize) {
    LIMIT.store(size, Relaxed);
}

pub fn load_allocated() -> usize {
    // Expect from `may_allocate` this code does not contain any depends on the atomic operation,
    // and even with no side effects in this function, we must use `Acquire`
    // because the caller almost always needs it.
    ALLOCATED.load(Acquire)
}

pub(crate) fn mark_deallocated(size: usize) {
    // Expect from `may_allocate` this code does not contain any depends on the atomic operation,
    // and even with no side effects in this function, we must use `Release`
    // because the caller almost always needs it.
    ALLOCATED.fetch_sub(size, Release);
}

pub(crate) fn may_allocate(size: usize) -> bool {
    // We count that the limit is not changed after the app start
    let limit = LIMIT.load(Relaxed);
    let mut allocated = ALLOCATED.load(Relaxed);

    loop {
        let new_allocated = allocated + size;
        if new_allocated > limit {
            return false;
        }

        let res = ALLOCATED.compare_exchange_weak(allocated, new_allocated, Relaxed, Relaxed);

        match res {
            Ok(_) => return true,
            Err(current) => allocated = current,
        }
    }
}

pub(crate) fn allocate_any_way(size: usize) {
    ALLOCATED.fetch_add(size, Relaxed);
}