use std::cmp::max;
use std::ptr::NonNull;
use crate::AllocatedInSelfList;
use crate::region::REGION_SIZE;

const MIN_SUPPORTED_CHUNK_SIZE: usize = REGION_SIZE;

pub(crate) const fn calc_grades(min: usize, max: usize) -> usize {
    assert!(min.is_power_of_two());
    assert!(max.is_power_of_two());

    (max / min).ilog2() as usize
}

// MIN = MIN_CHUNK_SIZE.
// MAX = MAX_CHUNK_SIZE.
// For example, for MIN = 16 KB and MAX = 1 GB we have 16 grades = at most 16 search iterations.
pub(crate) struct BuddyCache<const MIN: usize, const MAX: usize, const GRADES: usize> {
    free_lists: [AllocatedInSelfList; GRADES],
}

impl<const MIN: usize, const MAX: usize, const GRADES: usize> BuddyCache<MIN, MAX, GRADES> {
    pub(crate) const fn new() -> Self {
        assert!(calc_grades(MIN, MAX) == GRADES, "`GRADES` should be equal to `calc_grades(MIN, MAX)`");

        Self {
            free_lists: [const { AllocatedInSelfList::new() }; GRADES],
        }
    }

    pub(crate) fn get(&mut self, size: usize, align: usize) -> Option<NonNull<u8>> {
        debug_assert!(size >= MIN, "`size` should be greater than or equal to `MIN` ({} >= {})", size, MIN);
        debug_assert!(size <= MAX, "`size` should be less than or equal to `MAX` ({} <= {})", size, MAX);

        let size = max(
            size.next_power_of_two(),
            max(align, size_of::<usize>()),
        );
        let grade = size.trailing_zeros() as usize;

        for i in grade..GRADES {
            if let Some(ptr) = self.free_lists[i].pop() {
                return None;
            }
        }

        return None;
    }
}