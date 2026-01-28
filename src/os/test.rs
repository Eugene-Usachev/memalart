use crate::os::{alloc_aligned_from_os, free};
use crate::test_utilities::acquire_test_lock;
use std::ptr::null_mut;

#[test]
fn test_alloc_aligned_from_os() {
    let test_lock = acquire_test_lock();

    for _ in 0..20 {
        let (ptr, _) = unsafe {
            alloc_aligned_from_os(
                null_mut(),
                1024 * 1024, // size
                false,       // commit
                false,       // huge_only
                1024 * 1024,
            )
        };

        assert!(!ptr.is_null());

        assert_eq!(
            ptr as usize % (1024 * 1024),
            0,
            "ptr must be aligned to 1MB"
        );

        unsafe { free(ptr, 1024) };
    }

    drop(test_lock);
}
