use orengine_utils::hints::assert_hint;
use crate::{InfoForSize, NonLocalPtrManager, ThreadLocalAllocator};
use crate::stats::lazy_detailed_stats::LazyDetailedStats;

pub struct SnapshotDetailedStats(Vec<InfoForSize>);

impl SnapshotDetailedStats {
    pub fn new<NLPM: NonLocalPtrManager>(allocator: &ThreadLocalAllocator<NLPM>) -> Self {
        let lazy = LazyDetailedStats::new(allocator);
        let mut vec = Vec::with_capacity(lazy.size_hint().0);

        for info in lazy {
            // Because this method is sync, the lower bound of size_hint is correct.
            assert_hint(
                vec.len() < vec.capacity(),
                "the lower bound of size_hint is incorrect"
            );

            vec.push(info);
        }

        Self(vec)
    }

    pub fn iter(&self) -> impl Iterator<Item = InfoForSize> + use<'_> {
        self.0.iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use crate::DefaultThreadLocalAllocator;
    use crate::sizes::{recommended_size, SizeClass, COMMON_CHUNK_SIZE, COMMON_SIZE_MAX, LARGE_CHUNK_SIZE};
    use crate::test_utilities::do_test_default_thread_local_allocator;

    fn basic_(allocator: &mut DefaultThreadLocalAllocator) {
        let data = [(9, 123), (511, 12), (2000, 5), (15123, 3), (200_000, 3), (543_210, 2)];
        let ptrs = data
            .iter()
            .map(|(size, number)| {
                (*size, (0..*number).map(|_| allocator.alloc_or_panic(*size)).collect())
            })
            .collect::<Vec<(usize, Vec<_>)>>();
        let stats = allocator.snapshot_detailed_stats();

        for (size, number) in data {
            let objects_used = stats
                .iter()
                .find(|info| info.size_of_elem() == recommended_size(size))
                .expect("size is not allocated or is not in lazy stats")
                .objects_used();

            assert!(objects_used >= number);
        }

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