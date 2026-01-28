use core::fmt;
use crate::{NonLocalPtrManager, ThreadLocalAllocator};
use crate::stats::lazy_detailed_stats::LazyDetailedStats;

pub struct ShortStats {
    allocated_memory_from_os: usize,
    used_memory: usize,
}

impl ShortStats {
    pub(crate) fn new<NLPM: NonLocalPtrManager>(allocator: &ThreadLocalAllocator<NLPM>) -> Self {
        let lazy = LazyDetailedStats::new(allocator);

        lazy.map(|info| {
            Self {
                allocated_memory_from_os: info.memory_allocated(),
                used_memory: info.memory_used()
            }
        }).fold(
            Self { allocated_memory_from_os: 0, used_memory: 0 },
            |mut acc, x| {
                acc.used_memory += x.used_memory;
                acc.allocated_memory_from_os += x.allocated_memory_from_os;

                acc
            })
    }

    pub fn allocated_memory_from_os(&self) -> usize {
        self.allocated_memory_from_os
    }

    pub fn used_memory(&self) -> usize {
        self.used_memory
    }
}

impl fmt::Display for ShortStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "allocated_memory_from_os: {}, used_memory: {}",
            self.allocated_memory_from_os,
            self.used_memory
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::DefaultThreadLocalAllocator;
    use crate::sizes::{SizeClass, COMMON_CHUNK_SIZE, COMMON_SIZE_MAX, LARGE_CHUNK_SIZE};
    use crate::test_utilities::do_test_default_thread_local_allocator;

    fn basic_(allocator: &mut DefaultThreadLocalAllocator) {
        let data = [(9, 123), (511, 12), (2000, 5), (15123, 3), (200_000, 3), (543_210, 2)];
        let ptrs = data
            .iter()
            .map(|(size, number)| {
                (*size, (0..*number).map(|_| allocator.alloc_or_panic(*size)).collect())
            })
            .collect::<Vec<(usize, Vec<_>)>>();
        let stats = allocator.short_stats();
        let mut minimal_used_size = 0;
        let mut minimal_allocated_size = 0;

        for (size, number) in data {
            minimal_used_size += size * number;

            match SizeClass::define(size) {
                SizeClass::Common => {
                    assert!(size * number <= COMMON_CHUNK_SIZE);
                    minimal_allocated_size += COMMON_CHUNK_SIZE;
                }
                SizeClass::Large => {
                    assert!(size * number <= LARGE_CHUNK_SIZE);
                    minimal_allocated_size += LARGE_CHUNK_SIZE;
                }
                SizeClass::ExtraLarge => {
                    minimal_allocated_size += size * number;
                }
            }
        }

        assert!(
            stats.used_memory() >= minimal_used_size,
            "stats.used_memory() = {} while should be at least {}",
            stats.used_memory(),
            minimal_used_size
        );

        assert!(
            stats.allocated_memory_from_os() >= minimal_allocated_size,
            "stats.allocated_memory_from_os() = {} while should be at least {}",
            stats.allocated_memory_from_os(),
            minimal_allocated_size
        );

        for (size, ptrs) in ptrs {
            for ptr in ptrs {
                unsafe { allocator.dealloc(ptr, size); }
            }
        }
    }

    #[test]
    fn basic() {
        do_test_default_thread_local_allocator(basic_);
    }
}