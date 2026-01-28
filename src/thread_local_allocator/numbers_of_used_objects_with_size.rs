use std::num::NonZeroU32;
use orengine_utils::hints::assert_hint;

use crate::sizes::{large_size_to_id_and_real_size, recommended_size, COMMON_SIZE_MAX, LARGE_SIZE_MAX};
use crate::test_assert::test_assert;

/// Element stored in the array: (count, real_size)
pub(crate) type UsedObjEntry = (Option<NonZeroU32>, usize);

/// Binary search by real_size
///
/// Returns:
///
/// - `Ok(index)` if found
///
/// - `Err(insert_index)` if not found; insert at this index.
#[inline]
fn binary_search(arr: &[UsedObjEntry], real_size: usize, len: usize) -> Result<usize, usize> {
    let mut low = 0usize;
    let mut high = len;

    while low < high {
        let mid = (low + high) / 2;
        let mid_val = arr[mid].1;

        if mid_val < real_size {
            low = mid + 1;
        } else if mid_val > real_size {
            high = mid;
        } else {
            return Ok(mid);
        }
    }

    Err(low)
}

pub struct UsedObjectsSlice<'a> {
    arr: &'a [UsedObjEntry],
    len: &'a usize,
}

impl<'a> UsedObjectsSlice<'a> {
    pub(crate) fn as_slice(&self) -> &[UsedObjEntry] {
        &self.arr[..*self.len]
    }

    #[inline]
    pub(crate) fn find_size(&self, size: usize) -> Result<UsedObjEntry, usize> {
        let idx = binary_search(self.arr, size, *self.len)?;

        Ok(unsafe { *self.arr.get_unchecked(idx) })
    }
}

pub struct UsedObjectsMutSlice<'a> {
    arr: &'a mut [UsedObjEntry],
    len: &'a mut usize,
}

impl<'a> UsedObjectsMutSlice<'a> {
    pub(crate) fn write_used_number_and_size(&mut self, size: usize, n: u32) {
        if size == 0 || n == 0 { return; }

        test_assert!(size == recommended_size(size));

        match binary_search(self.arr, size, *self.len) {
            Ok(idx) => {
                let number = self.arr[idx].0.unwrap();
                let new_number = unsafe { NonZeroU32::new_unchecked(number.get() + n) };
                self.arr[idx].0 = Some(new_number);
            }
            Err(ins_idx) => {
                assert_hint(*self.len < self.arr.len(), "Array too small");

                // TODO memcpy

                unsafe {
                    let p = self.arr.as_mut_ptr().add(ins_idx);

                    std::ptr::copy(p, p.add(1), *self.len - ins_idx);
                }

                self.arr[ins_idx] = (Some(unsafe { NonZeroU32::new_unchecked(n) }), size);
                *self.len += 1;
            }
        }
    }
}

pub(crate) struct UsedObjectsFixed<const N: usize> {
    arr: [UsedObjEntry; N],
    len: usize,
}

impl<const N: usize> UsedObjectsFixed<N> {
    pub(crate) const fn new() -> Self {
        Self {
            arr: [(None, 0); N],
            len: 0,
        }
    }

    #[inline]
    pub(crate) fn as_slice(&self) -> UsedObjectsSlice {
        UsedObjectsSlice {
            arr: &self.arr[..self.len],
            len: &self.len,
        }
    }

    #[inline]
    pub(crate) fn as_mut_slice(&mut self) -> UsedObjectsMutSlice {
        UsedObjectsMutSlice {
            arr: &mut self.arr,
            len: &mut self.len,
        }
    }

    pub(crate) fn find_size(&self, size: usize) -> Result<UsedObjEntry, usize> {
        self.as_slice().find_size(size)
    }

    pub(crate) fn write_used_number_and_size(&mut self, size: usize, number: u32) {
        self.as_mut_slice().write_used_number_and_size(size, number);
    }
}