use criterion::{criterion_group, criterion_main, Criterion};
use easy_alloc::{deregister_thread_local_default_allocator, generate_thread_local_allocator_functions, thread_local_default_allocator, DefaultThreadLocalAllocator, NonLocalPtrData, NonLocalPtrManager, ThreadLocalAllocator};
use mimalloc::MiMalloc;
use std::alloc::{alloc, dealloc, GlobalAlloc, Layout, System};
use std::cell::UnsafeCell;
use std::hint::black_box;
use std::mem::ManuallyDrop;

use std::ptr::NonNull;
use std::thread;
use std::time::Duration;
use orengine_utils::hints::unwrap_or_bug_message_hint;

const SIZES: [usize; 7] = [40, 4096, 511 * 1024, 10 * 1024 * 1024, 64 * 1024 * 1024, 512 * 1024 * 1024, 2 * 1024 * 1024 * 1024];

fn touch(ptr: *mut u8, size: usize) {
    const STEP: usize = 4096;

    let work = || {
        let mut curr = ptr;

        for _ in (0..size).step_by(STEP) {
            unsafe { curr.write(0); }

            curr = unsafe { curr.byte_add(STEP) };
        }
    };

    work(); // make OS to allocate the memory physically
    work(); // check how well an allocator chooses regions (huge tables or not?)
}

struct SysAlloc;

unsafe impl GlobalAlloc for SysAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
}

struct IdealPoolAllocator {
    inner: UnsafeCell<Vec<NonNull<u8>>>,
    size: usize,
}

impl IdealPoolAllocator {
    fn new(size: usize) -> Self {
        Self {
            inner: UnsafeCell::new(Vec::new()),
            size,
        }
    }

    fn release_memory(&mut self) {
        for ptr in unsafe { &mut *self.inner.get() }.drain(..) {
            unsafe {
                System.dealloc(
                    ptr.as_ptr(),
                    Layout::from_size_align_unchecked(self.size, 1),
                )
            };
        }
    }
}

unsafe impl GlobalAlloc for IdealPoolAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if let Some(ptr) = unsafe { &mut *self.inner.get() }.pop() {
            ptr.as_ptr()
        } else {
            System.alloc(layout)
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        unsafe { &mut *self.inner.get() }.push(NonNull::new_unchecked(ptr));
    }
}

struct DummyNonLocalPtrManager;

impl Default for DummyNonLocalPtrManager {
    fn default() -> Self {
        Self
    }
}

impl NonLocalPtrManager for DummyNonLocalPtrManager {
    const SHOULD_AUTO_CHECK_FOR_INCOMING_NON_LOCAL_PTRS: bool = false;

    fn deallocate_non_local_ptr(&self, allocator: &ThreadLocalAllocator<Self>, other_manager_ptr: &Self, non_local_ptr_data: NonLocalPtrData) {
        todo!()
    }

    fn check_for_incoming_non_local_ptrs(&self, allocator: &ThreadLocalAllocator<Self>, non_local_ptrs_handle: impl FnMut(&[NonLocalPtrData])) {
        todo!()
    }

    fn self_allocated_memory<'iter>(&'iter self) -> impl Iterator<Item=usize> + 'iter {
        std::iter::empty()
    }

    fn self_drop(&mut self) {
        todo!()
    }
}

fn bench_alloc_and_dealloc_from_one_thread(c: &mut Criterion) {
    let mut group = c.benchmark_group("alloc_dealloc_one_thread");

    for &size in SIZES.iter() {
        let layout = Layout::from_size_align(size, 8).unwrap();

        group.bench_function(format!("ideal_pool_{}", size), |b| {
            let pool = IdealPoolAllocator::new(size);

            b.iter(|| unsafe {
                let p = pool.alloc(layout);

                touch(p, size);

                pool.dealloc(p, layout);
            });
        });

        group.bench_function(format!("sys_alloc_{}", size), |b| {
            b.iter(|| unsafe {
                let p = SysAlloc.alloc(layout);

                touch(p, size);

                SysAlloc.dealloc(p, layout);
            });
        });

        group.bench_function(format!("std_alloc_{}", size), |b| {
            b.iter(|| unsafe {
                let p = alloc(layout);

                touch(p, size);

                dealloc(p, layout);
            });
        });

        type TLAllocator = ThreadLocalAllocator<DummyNonLocalPtrManager>;

        generate_thread_local_allocator_functions!(
            Allocator,
            TLAllocator,
            get_allocator_mut,
            get_allocator,
            deregister_allocator
        );

        group.bench_function(format!("optimistic_alloc_{}", size), |b| {
            static ALLOCATOR: Allocator = Allocator{};

            b.iter(|| unsafe {
                let p = ALLOCATOR.alloc(layout);

                touch(p, size);

                ALLOCATOR.dealloc(p, layout);
            });
        });

        // group.bench_function(format!("optimistic_alloc_{}_with_tls_get", size), |b| {
        //     b.iter(|| unsafe {
        //         let p = get_allocator().try_alloc(layout.size()).unwrap();
        //
        //         touch(p, size);
        //
        //         get_allocator().dealloc(p, layout.size());
        //     });
        // });

        group.bench_function(format!("mimalloc_{}", size), |b| {
            b.iter(|| unsafe {
                let p = MiMalloc.alloc(layout);

                touch(p, size);

                MiMalloc.dealloc(p, layout);
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_alloc_and_dealloc_from_one_thread);
criterion_main!(benches);
