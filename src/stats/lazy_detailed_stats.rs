use crate::thread_local_allocator::InfoForSizeIter;
use crate::{InfoForSize, NonLocalPtrManager, ThreadLocalAllocator};

pub struct LazyDetailedStats<'allocator>(InfoForSizeIter<'allocator>);

impl<'allocator> LazyDetailedStats<'allocator> {
    pub(crate) fn new<NLPM: NonLocalPtrManager>(
        allocator: &'allocator ThreadLocalAllocator<NLPM>
    ) -> Self {
        Self(allocator.iter_allocated_size())
    }
}

impl<'allocator> Iterator for LazyDetailedStats<'allocator> {
    type Item = InfoForSize;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(_, info)| info)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
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
        let stats = allocator.lazy_detailed_stats().collect::<Vec::<_>>();

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
