use std::cell::UnsafeCell;
use std::env;
use crate::test_assert::test_assert;
use libc::{c_int};
use orengine_utils::hints::unlikely;
use std::ffi::c_void;
use std::ptr::{null, null_mut, NonNull};
use std::sync::atomic::{AtomicUsize, Ordering};
use orengine_utils::backoff::Backoff;
use crate::{NonLocalPtrManager, ThreadLocalAllocator};

unsafe fn unix_madvise(addr: *mut u8, size: usize, advice: i32) -> i32 {
    unsafe { libc::madvise(addr.cast(), size, advice) }
}

unsafe fn munmap_raw(addr: *mut u8, size: usize) {
    debug_assert!(size > 0, "Attempt to free zero bytes");

    unsafe { libc::munmap(addr.cast(), size as _) };
}

pub(crate) unsafe fn free(addr: *mut u8, size: usize) {
    debug_assert!(size > 0, "Attempt to free zero bytes");

    libc::munmap(addr.cast(), size as _);
}

fn mmap_fd() -> c_int {
    if cfg!(target_os = "macos") {
        254 // On macOS, 254 is MAP_ANON’s fd placeholder
    } else {
        -1
    }
}

pub(crate) enum MmapHugePageResult {
    HugePage,
    NotHugePage,
}

unsafe fn mmap_not_huge(addr: *mut u8, size: usize, protect_flags: i32, flags: i32) -> *mut u8 {
    let ptr = unsafe { libc::mmap(addr.cast(), size, protect_flags, flags, mmap_fd(), 0).cast() };

    if ptr == libc::MAP_FAILED {
        return null_mut();
    }

    ptr.cast::<u8>()
}

const SHOULD_TRY_1GB_HUGE_PAGE: usize = 2;
const SHOULD_TRY_2MB_HUGE_PAGE: usize = 1;
const SHOULD_NOT_TRY_HUGE_PAGE: usize = 0;

const CAN_USE_1GB_HUGE_PAGE: bool = cfg!(target_os = "linux");

static SHOULD_TRY_OPTION: AtomicUsize = AtomicUsize::new(if CAN_USE_1GB_HUGE_PAGE {
    SHOULD_TRY_1GB_HUGE_PAGE
} else {
    SHOULD_TRY_2MB_HUGE_PAGE
});

pub(crate) unsafe fn custom_mmap(
    addr: *mut u8,
    size: usize,
    protect_flags: i32,
    commit: bool,
    huge_only: bool,
) -> (*mut u8, MmapHugePageResult) {
    debug_assert!(size > 0, "Attempt to mmap zero bytes");

    let mut ptr: *mut c_void;
    let mut flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS;
    if !commit {
        flags |= libc::MAP_NORESERVE
    }

    let current_should_try_option = SHOULD_TRY_OPTION.load(Ordering::Relaxed);
    let may_be_2mb_huge_page =
        size % (2 * 1024 * 1024) == 0 && current_should_try_option == SHOULD_TRY_2MB_HUGE_PAGE;
    let may_be_1gb_huge_page = size % (1024 * 1024 * 1024) == 0
        && current_should_try_option == SHOULD_TRY_1GB_HUGE_PAGE
        && CAN_USE_1GB_HUGE_PAGE;
    let should_try_alloc_huge = huge_only | may_be_2mb_huge_page | may_be_1gb_huge_page;

    if should_try_alloc_huge {
        let mut huge_page_flags = flags & !libc::MAP_NORESERVE;

        #[cfg(target_os = "linux")]
        {
            huge_page_flags |= libc::MAP_HUGETLB;

            if may_be_1gb_huge_page {
                huge_page_flags |= libc::MAP_HUGE_1GB;
            } else {
                huge_page_flags |= libc::MAP_HUGE_2MB;
            }
        }

        #[cfg(target_os = "macos")]
        {
            huge_page_flags |= libc::VM_FLAGS_SUPERPAGE_SIZE_2MB;
        }

        ptr = unsafe {
            libc::mmap(
                addr.cast(),
                size,
                protect_flags,
                huge_page_flags,
                mmap_fd(),
                0,
            )
        };

        #[cfg(target_os = "linux")]
        {
            // 1GB Huge pages are possibly unsupported
            if (ptr == null_mut() || ptr == libc::MAP_FAILED)
                && huge_page_flags & libc::MAP_HUGE_1GB != 0
            {
                SHOULD_TRY_OPTION.store(SHOULD_TRY_2MB_HUGE_PAGE, Ordering::Relaxed);

                huge_page_flags = (huge_page_flags & !libc::MAP_HUGE_1GB) | libc::MAP_HUGE_2MB;

                ptr = unsafe {
                    libc::mmap(
                        addr.cast(),
                        size,
                        protect_flags,
                        huge_page_flags,
                        mmap_fd(),
                        0,
                    )
                };
            }
        }

        if ptr != libc::MAP_FAILED && ptr != null_mut() {
            return (ptr.cast(), MmapHugePageResult::HugePage);
        } else {
            SHOULD_TRY_OPTION.store(SHOULD_NOT_TRY_HUGE_PAGE, Ordering::Relaxed);

            if huge_only {
                return (null_mut(), MmapHugePageResult::NotHugePage);
            }
        } // else fallback to mmap_not_huge
    }

    (
        unsafe { mmap_not_huge(addr, size, protect_flags, flags) },
        MmapHugePageResult::NotHugePage,
    )
}

unsafe fn alloc_from_os_(
    hint_addr: *mut u8,
    size: usize,
    commit: bool,
    huge_only: bool,
) -> (*mut u8, MmapHugePageResult) {
    let protect_flags = if commit {
        libc::PROT_READ | libc::PROT_WRITE
    } else {
        libc::PROT_NONE
    };

    unsafe { custom_mmap(hint_addr, size, protect_flags, commit, huge_only) }
}

pub(crate) unsafe fn alloc_from_os(
    hint_addr: *mut u8,
    size: usize,
    commit: bool,
    huge_only: bool,
) -> (*mut u8, MmapHugePageResult) {
    let (ptr, mmap_huge_page_result) = unsafe { 
        alloc_from_os_(hint_addr, size, commit, huge_only)
    };
    if unlikely(ptr.is_null()) {
        return (ptr, mmap_huge_page_result);
    }

    (ptr, mmap_huge_page_result)
}

pub(crate) unsafe fn alloc_aligned_from_os(
    hint_addr: *mut u8,
    size: usize,
    commit: bool,
    huge_only: bool,
    align: usize,
) -> (*mut u8, MmapHugePageResult) {
    test_assert!(align.is_power_of_two(), "align must be power of two");
    test_assert!(align != 0, "align must be non-zero");

    let extra = align - 1;

    let alloc_size = extra + size;
    let (base_ptr, mmap_huge_page_result) = unsafe {
        alloc_from_os_(
            hint_addr, alloc_size, false, // commit
            huge_only,
        )
    };

    if unlikely(base_ptr.is_null()) {
        return (base_ptr, mmap_huge_page_result);
    }

    let base = base_ptr as usize;

    // compute aligned start inside [base..base + alloc_size)
    let aligned = (base + (align - 1)) & !(align - 1);
    let aligned_ptr = aligned as *mut u8;

    test_assert!(aligned <= base + size);

    let new_ptr = if commit {
        libc::mmap(
            aligned_ptr.cast(),
            size,
            libc::PROT_READ | libc::PROT_WRITE, // commit it
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE | libc::MAP_FIXED,
            mmap_fd(),
            0,
        )
    } else {
        aligned_ptr.cast()
    };

    test_assert!(new_ptr == aligned_ptr.cast()); // if they are equal, they both are valid

    // free head
    if aligned > base {
        unsafe { munmap_raw(base_ptr, aligned - base) };
    }

    // free tail
    let end = aligned + size;
    let alloc_end = base + alloc_size;
    if end < alloc_end {
        munmap_raw(end as *mut u8, alloc_end - end);
    }

    (aligned_ptr, mmap_huge_page_result)
}

const FIRST_CALL: usize = 0;
const INITIALIZING: usize = 1;
const INITIALIZED: usize = 2;

static mut KEY: libc::pthread_key_t = 0; // TODO is ONCE enough
static ONCE: AtomicUsize = AtomicUsize::new(0);

#[inline(never)]
pub(crate) fn register_thread_local_allocator_destructor<NLPM: NonLocalPtrManager>(
    allocator: NonNull<ThreadLocalAllocator<NLPM>>,
) {
    // TODO thread allocator may be created only from the Allocator and Allocator is guaranteed to be created exactly once.

    match ONCE.compare_exchange(FIRST_CALL, INITIALIZING, Ordering::SeqCst, Ordering::SeqCst) {
        Ok(_) => {
            unsafe extern "C" fn destructor<NLPM: NonLocalPtrManager>(ptr: *mut c_void) {
                let Some(allocator_ptr) = NonNull::new(ptr) else {
                    return;
                };

                unsafe { allocator_ptr.cast::<ThreadLocalAllocator<NLPM>>().as_mut() }.destructor();
            }

            let res = unsafe { libc::pthread_key_create(&raw mut KEY, Some(destructor::<NLPM>)) };
            if res != 0 {
                panic!("Failed to create pthread key");
            }

            ONCE.store(INITIALIZED, Ordering::SeqCst);
        }
        Err(curr) => {
            if curr != INITIALIZED {
                let backoff = Backoff::new();
                while ONCE.load(Ordering::SeqCst) != INITIALIZED {
                    backoff.spin_or(|| unsafe { libc::sched_yield(); });
                }
            } // else go on
        }
    }

    let curr = unsafe { libc::pthread_getspecific(KEY) };
    assert!(
        curr.is_null(),
        "Attempt to register more than one ThreadLocalAllocator!"
    );

    unsafe { libc::pthread_setspecific(KEY, allocator.cast().as_ptr()) };
}

pub unsafe fn deregister_thread_local_allocator_destructor () {
    let is_init = ONCE.load(Ordering::SeqCst) == INITIALIZED;
    if !is_init {
        return;
    }
    
    unsafe { libc::pthread_setspecific(KEY, null_mut()) };
}

pub(crate) fn spawn_thread<T, F>(f: F)
where
    F: FnOnce() -> T,
    F: Send + 'static,
    T: Send + 'static,
{
    extern "C" fn start<T, F>(data: *mut c_void) -> *mut c_void
    where
        F: FnOnce() -> T,
        F: Send + 'static,
        T: Send + 'static,
    {
        unsafe {
            let f = data.cast::<F>().read();

            f();

            munmap_raw(data.cast(), size_of::<F>());
        }

        null_mut()
    }

    let data = unsafe {
        alloc_from_os_(
            null_mut(),
            size_of::<F>(),
            true, // commit
            false, // huge_only
        )
            .0
            .cast::<F>()
    };

    unsafe {
        data.write(f);
    }

    unsafe {
        let mut thread_id: libc::pthread_t = 0;

        let result = libc::pthread_create(
            &mut thread_id,
            null(), // Default attributes
            start::<T, F>,
            data as *mut _,
        );

        if result == 0 {
            libc::pthread_detach(thread_id);
        } else {
            data.drop_in_place();

            munmap_raw(data.cast(), size_of::<F>());

            panic!("Failed to spawn a new thread.")
        }
    }
}