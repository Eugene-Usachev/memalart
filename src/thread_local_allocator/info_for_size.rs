use core::fmt;
use std::cell::UnsafeCell;
use orengine_utils::hints::{unlikely, unwrap_or_bug_hint};
use crate::{NonLocalPtrManager, ThreadLocalAllocator};
use crate::self_allocated_structs::btree;
use crate::sizes::{SizeClass, COMMON_CHUNK_SIZE, COMMON_SIZE_TABLE_LEN, LARGE_CHUNK_SIZE, REAL_LARGE_SIZES, REAL_NUMBER_OF_LARGE_OBJECT_VECTORS};
use crate::thread_local_allocator::numbers_of_used_objects_with_size::UsedObjectsFixed;
use crate::thread_local_allocator::{ThreadLocalAllocatorInner, MAX_NUMBER_OF_SELF_ALLOCATED_OBJECTS};

#[derive(Copy, Clone)]
pub struct InfoForSize {
    pub(super) memory_allocated: usize,
    pub(super) size_of_elem: usize,
    pub(super) objects_allocated: usize,
    pub(super) objects_used: usize,
    pub(super) allocated_for_self_objects: usize,
}

impl InfoForSize {
    pub fn memory_used(&self) -> usize {
        self.objects_used * self.size_of_elem
    }
    
    pub fn memory_allocated(&self) -> usize {
        self.memory_allocated
    }

    pub fn size_of_elem(&self) -> usize {
        self.size_of_elem
    }

    pub fn objects_allocated(&self) -> usize {
        self.objects_allocated
    }

    pub fn objects_used(&self) -> usize {
        self.objects_used
    }

    pub fn allocated_for_self_objects(&self) -> usize {
        self.allocated_for_self_objects
    }

    /// Calculates the utilization percentage of the allocated objects.
    pub fn utilization(&self) -> f64 {
        if self.objects_allocated == 0 {
            0.0
        } else {
            (self.objects_used as f64 / self.objects_allocated as f64) * 100.0
        }
    }
}

impl fmt::Display for InfoForSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "size: {} bytes: {} bytes allocated, {}/{} objects used ({:.2}% utilization)",
            self.size_of_elem,
            self.memory_allocated,
            self.objects_used,
            self.objects_allocated,
            self.utilization()
        )
    }
}

enum CurrIter<'allocator> {
    CommonSize(usize),
    LargeSize(usize),
    ExtraLargeSize(btree::map::Iter<'allocator, usize, usize>),
}

pub(crate) struct InfoForSizeIter<'allocator> {
    inner: &'allocator ThreadLocalAllocatorInner,
    inner_ref: &'allocator UnsafeCell<ThreadLocalAllocatorInner>,
    allocated_for_self: UsedObjectsFixed<MAX_NUMBER_OF_SELF_ALLOCATED_OBJECTS>,
    curr_iter: CurrIter<'allocator>
}

impl<'allocator> InfoForSizeIter<'allocator> {
    pub(super) fn new<NLPM: NonLocalPtrManager>(
        allocator: &'allocator ThreadLocalAllocator<NLPM>
    ) -> Self {
        let inner = allocator.get_inner_with_any_nlpm();
        let allocated_for_self = allocator.allocated_for_self();

        inner.is_borrowed_for_unknown_time = true;

        let curr_iter = if !inner.common_chunk_table.is_null() {
            CurrIter::CommonSize(0)
        } else if !inner.large_chunk_table.is_null() {
            CurrIter::LargeSize(0)
        } else {
            CurrIter::ExtraLargeSize(inner.allocated_large_sizes.iter())
        };

        Self {
            inner,
            inner_ref: allocator.get_inner_ref_any_nlpm(),
            allocated_for_self,
            curr_iter
        }
    }
}

impl<'allocator> Iterator for InfoForSizeIter<'allocator> {
    type Item = (SizeClass, InfoForSize);

    fn next(&mut self) -> Option<Self::Item> {
        let allocated_for_self_of_size = |size_of_elem| {
            self
                .allocated_for_self
                .find_size(size_of_elem)
                .map(|(n, _)| n.unwrap().get())
                .unwrap_or(0)
                as usize
        };

        match &mut self.curr_iter {
            CurrIter::CommonSize(idx_ref) => {
                let idx = *idx_ref;
                if unlikely(idx == COMMON_SIZE_TABLE_LEN) {
                    if !self.inner.large_chunk_table.is_null() {
                        self.curr_iter = CurrIter::LargeSize(0);
                    } else {
                        self.curr_iter = CurrIter::ExtraLargeSize(self.inner.allocated_large_sizes.iter());
                    }

                    return self.next();
                }

                *idx_ref += 1;

                let common_chunk_table = unwrap_or_bug_hint(self.inner.common_chunk_table.as_ref());
                let chunks_allocated = common_chunk_table[idx].len();
                let memory_allocated = COMMON_CHUNK_SIZE * chunks_allocated;
                let size_of_elem = (idx + 1) * 8;
                let objects_allocated = memory_allocated / size_of_elem;
                let object_vec = self.inner.common_chunk_object_vector_for_size(size_of_elem);
                let objects_used = objects_allocated - object_vec.len();
                let info = InfoForSize {
                    memory_allocated,
                    size_of_elem,
                    objects_allocated,
                    objects_used,
                    allocated_for_self_objects: allocated_for_self_of_size(size_of_elem),
                };

                Some((SizeClass::Common, info))
            }
            CurrIter::LargeSize(idx_ref) => {
                let idx = *idx_ref;
                if unlikely(idx == REAL_NUMBER_OF_LARGE_OBJECT_VECTORS) {
                    self.curr_iter = CurrIter::ExtraLargeSize(self.inner.allocated_large_sizes.iter());

                    return self.next();
                }

                *idx_ref += 1;

                let large_chunk_table = unwrap_or_bug_hint(self.inner.large_chunk_table.as_ref());
                let chunks_allocated = large_chunk_table[idx].len();
                let memory_allocated = chunks_allocated * LARGE_CHUNK_SIZE;
                let size_of_elem = REAL_LARGE_SIZES[idx];
                let objects_allocated = memory_allocated / size_of_elem;
                let object_vec = unsafe {
                    self.inner.large_object_table.get_vec_by_real_idx(idx)
                };
                let objects_used = objects_allocated - object_vec.len();
                let info = InfoForSize {
                    memory_allocated,
                    size_of_elem,
                    objects_allocated,
                    objects_used,
                    allocated_for_self_objects: allocated_for_self_of_size(size_of_elem),
                };

                Some((SizeClass::Large, info))
            }
            CurrIter::ExtraLargeSize(ref mut iter) => {
                let (size, count) = iter.next()?;
                let info = InfoForSize {
                    memory_allocated: size * count,
                    size_of_elem: *size,
                    objects_allocated: *count,
                    objects_used: *count,
                    allocated_for_self_objects: allocated_for_self_of_size(*size)
                };

                Some((SizeClass::ExtraLarge, info))
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.curr_iter {
            CurrIter::CommonSize(idx) => {
                let common_sizes = COMMON_SIZE_TABLE_LEN - idx;
                let large_sizes = REAL_NUMBER_OF_LARGE_OBJECT_VECTORS;
                let extra_large_sizes = self.inner.allocated_large_sizes.len();
                
                (common_sizes + large_sizes + extra_large_sizes, None)
            }
            CurrIter::LargeSize(idx) => {
                let large_sizes = REAL_NUMBER_OF_LARGE_OBJECT_VECTORS - idx;
                let extra_large_sizes = self.inner.allocated_large_sizes.len();

                (large_sizes + extra_large_sizes, None)
            }
            CurrIter::ExtraLargeSize(iter) => {
                (iter.len(), None)
            }
        }
    }
}

impl<'allocator> Drop for InfoForSizeIter<'allocator> {
    fn drop(&mut self) {
        unsafe { &mut *self.inner_ref.get() }.is_borrowed_for_unknown_time = false;
    }
}