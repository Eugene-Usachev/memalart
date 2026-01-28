use std::array;
use std::hint::assert_unchecked;
use std::mem::MaybeUninit;
use std::ptr::{copy_nonoverlapping, NonNull};
use orengine_utils::hints::{assert_hint, unlikely};
use crate::self_allocated_structs::SelfAllocatedVec;
use crate::sizes::{CommonRawChunk, COMMON_CHUNK_SIZE, COMMON_SIZE_MAX};
use crate::test_assert::test_assert;

pub(crate) const fn find_large_slot_id(mut size: usize) -> usize {
    // This function uses quadruple precision.
    // This decision is not based on intuition but on a quantitative trade-off analysis.
    //
    // The following configurations were evaluated:
    //
    //  Double precision:
    //    - Average max-loss: 18%
    //    - Worst-case max-loss: 25%
    //    - Theoretical waste for rarely used blocks: up to 12 MB
    //    - Metadata overhead: 920 bytes
    //    - Maximum manageable memory: 64 KB
    //
    //  Triple precision:
    //    - Average max-loss: 9%
    //    - Worst-case max-loss: 19%
    //    - Theoretical waste for rarely used blocks: up to 24 MB
    //    - Metadata overhead: 1,864 bytes
    //    - Maximum manageable memory: 96 KB
    //
    //  Quadruple precision:
    //    - Average max-loss: 5%
    //    - Worst-case max-loss: 18%
    //    - Theoretical waste for rarely used blocks: up to 44 MB
    //    - Metadata overhead: 3,552 bytes
    //    - Maximum manageable memory: 100 KB
    //
    //  Quintuple precision:
    //    - Average max-loss: 3%
    //    - Worst-case max-loss: 18%
    //    - Theoretical waste for rarely used blocks: up to 74 MB
    //    - Metadata overhead: 6,656 bytes
    //    - Maximum manageable memory: 102 KB
    //
    // Quadruple precision represents the optimal balance between fragmentation loss,
    // metadata overhead, and upper memory bounds. Increasing precision further yields
    // diminishing returns in loss reduction while significantly increasing worst-case
    // unused memory and bookkeeping cost.

    size -= 1;

    let b = (usize::BITS - 1 - size.leading_zeros()) as usize;

    (b << 4) + ((size >> (b - 4)) & 0x0F)
}

pub(crate) const LARGE_CHUNK_SIZE: usize = 512 * 1024;
pub(crate) const COMMON_CHUNKS_IN_LARGE_CHUNK: usize = LARGE_CHUNK_SIZE / COMMON_CHUNK_SIZE;
pub(crate) type LargeRawChunk = [u8; LARGE_CHUNK_SIZE];

pub(crate) fn split_large_chunk_to_common(
    ptr: NonNull<LargeRawChunk>,
) -> [NonNull<CommonRawChunk>; COMMON_CHUNKS_IN_LARGE_CHUNK] {
    let mut curr = ptr.cast();

    array::from_fn(|_| {
        let old = curr;

        curr = unsafe { curr.byte_add(COMMON_CHUNK_SIZE) };

        old
    })
}

#[cfg(test)]
fn generate_blocks() -> [(usize /* size */, usize /* id */); 143] {
    let mut arr = [(0, 0); 143];
    let mut arr_len = 0;
    let mut size = COMMON_SIZE_MAX as usize;
    let mut prev_id = find_large_slot_id(size);

    while size != LARGE_CHUNK_SIZE + 1 {
        let slot_id = find_large_slot_id(size);
        if slot_id == prev_id {
            size += 1;

            continue;
        }

        if prev_id != find_large_slot_id(COMMON_SIZE_MAX as usize) {
            arr[arr_len] = (size - 1, slot_id - 1);

            arr_len += 1;
        }

        prev_id = slot_id;
        size += 1;
    }

    arr
}

// TODO test it

// TODO real should start from 1089

const GENERATED_SIZES_AND_INDEXES_LEN: usize = 143;
// Precalculated by `generate_blocks` for `COMMON_SIZE_MAX` equals 1024 and `LARGE_CHUNK_SIZE`
// equals 512 KB. It is checked in tests.
const GENERATED_SIZES_AND_INDEXES: [(usize /* size */, usize /* id */); GENERATED_SIZES_AND_INDEXES_LEN] = [
    (1088, 160), (1152, 161), (1216, 162), (1280, 163), (1344, 164), (1408, 165), (1472, 166),
    (1536, 167), (1600, 168), (1664, 169), (1728, 170), (1792, 171), (1856, 172), (1920, 173),
    (1984, 174), (2048, 175), (2176, 176), (2304, 177), (2432, 178), (2560, 179), (2688, 180),
    (2816, 181), (2944, 182), (3072, 183), (3200, 184), (3328, 185), (3456, 186), (3584, 187),
    (3712, 188), (3840, 189), (3968, 190), (4096, 191), (4352, 192), (4608, 193), (4864, 194),
    (5120, 195), (5376, 196), (5632, 197), (5888, 198), (6144, 199), (6400, 200), (6656, 201),
    (6912, 202), (7168, 203), (7424, 204), (7680, 205), (7936, 206), (8192, 207), (8704, 208),
    (9216, 209), (9728, 210), (10240, 211), (10752, 212), (11264, 213), (11776, 214), (12288, 215),
    (12800, 216), (13312, 217), (13824, 218), (14336, 219), (14848, 220), (15360, 221),
    (15872, 222), (16384, 223), (17408, 224), (18432, 225), (19456, 226), (20480, 227),
    (21504, 228), (22528, 229), (23552, 230), (24576, 231), (25600, 232), (26624, 233),
    (27648, 234), (28672, 235), (29696, 236), (30720, 237), (31744, 238), (32768, 239),
    (34816, 240), (36864, 241), (38912, 242), (40960, 243), (43008, 244), (45056, 245),
    (47104, 246), (49152, 247), (51200, 248), (53248, 249), (55296, 250), (57344, 251),
    (59392, 252), (61440, 253), (63488, 254), (65536, 255), (69632, 256), (73728, 257),
    (77824, 258), (81920, 259), (86016, 260), (90112, 261), (94208, 262), (98304, 263),
    (102400, 264), (106496, 265), (110592, 266), (114688, 267), (118784, 268), (122880, 269),
    (126976, 270), (131072, 271), (139264, 272), (147456, 273), (155648, 274), (163840, 275),
    (172032, 276), (180224, 277), (188416, 278), (196608, 279), (204800, 280), (212992, 281),
    (221184, 282), (229376, 283), (237568, 284), (245760, 285), (253952, 286), (262144, 287),
    (278528, 288), (294912, 289), (311296, 290), (327680, 291), (344064, 292), (360448, 293),
    (376832, 294), (393216, 295), (409600, 296), (425984, 297), (442368, 298), (458752, 299),
    (475136, 300), (491520, 301), (507904, 302)
];

const FIRST_ID: usize = GENERATED_SIZES_AND_INDEXES[0].1;
const LAST_ID: usize = GENERATED_SIZES_AND_INDEXES[GENERATED_SIZES_AND_INDEXES_LEN - 1].1;
pub(crate) const REAL_NUMBER_OF_LARGE_OBJECT_VECTORS: usize = 88;
pub(crate) const MAX_ALLOCATIONS_FOR_LARGE_OBJECT_TABLE: usize = REAL_NUMBER_OF_LARGE_OBJECT_VECTORS;
pub(crate) const LARGE_SIZE_MAX: usize = 100 * 1024;
type RealTable = [SelfAllocatedVec<NonNull<u8>>; REAL_NUMBER_OF_LARGE_OBJECT_VECTORS];

// But it is totally unoptimized because for sizes like `507,905` and `491,521` the allocator
// uses one block. So, it makes sense to keep only `507,905`.

const REAL_LARGE_SIZES_LEN: usize = 265;
type LookupTable =  [usize; REAL_LARGE_SIZES_LEN];

#[cfg(test)]
struct RealLargeSizesAndLookupTable {
    sizes: [usize; REAL_LARGE_SIZES_LEN],
    lookup_table: LookupTable
}

// TODO test
#[cfg(test)]
const fn generate_real_large_sizes_and_lookup_table() -> RealLargeSizesAndLookupTable {
    let mut real_large_sizes_and_lookup_table = RealLargeSizesAndLookupTable {
        sizes: [0; REAL_LARGE_SIZES_LEN],
        lookup_table: [0; REAL_LARGE_SIZES_LEN]
    };
    let mut i = 0;
    let mut real_idx = 0;
    let mut group_start = 0;
    let mut prev_blocks = LARGE_CHUNK_SIZE / GENERATED_SIZES_AND_INDEXES[0].0;

    loop {
        let size = GENERATED_SIZES_AND_INDEXES[i].0;

        if size > LARGE_SIZE_MAX {
            let winner = GENERATED_SIZES_AND_INDEXES[i - 1].0;

            let mut j = group_start;
            while j < i {
                real_large_sizes_and_lookup_table.sizes[j + FIRST_ID - 1] = winner;
                j += 1;
            }

            break;
        }

        let blocks = LARGE_CHUNK_SIZE / size;

        if blocks != prev_blocks {
            // finalize previous group
            let winner = GENERATED_SIZES_AND_INDEXES[i - 1].0;

            let mut j = group_start;
            while j < i {
                real_large_sizes_and_lookup_table.sizes[j + FIRST_ID - 1] = winner;
                j += 1;
            }

            group_start = i;
            prev_blocks = blocks;
            real_idx += 1;
        }

        real_large_sizes_and_lookup_table.lookup_table[i + FIRST_ID - 1] = real_idx;

        i += 1;
    }

    real_large_sizes_and_lookup_table
}

// TODO fix usage
pub(crate) const LARGE_SIZES_FOR_ID: [usize; REAL_LARGE_SIZES_LEN] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    // The unused data is above. It is used as a filler allowing us to map an id to the real size
    // without any shifts with only `REAL_LARGE_SIZES[id]`
    1088, 1152, 1216, 1280, 1344, 1408, 1472, 1536, 1600, 1664, 1728, 1792, 1856, 1920, 1984, 2048,
    2176, 2304, 2432, 2560, 2688, 2816, 2944, 3072, 3200, 3328, 3456, 3584, 3712, 3840, 3968, 4096,
    4352, 4608, 4864, 5120, 5376, 5632, 5888, 6144, 6400, 6656, 6912, 7168, 7424, 7680, 7936, 8192,
    8704, 9216, 9728, 10240, 10752, 11264, 11776, 12288, 12800, 13312, 13824, 14336, 14848, 15360,
    15872, 16384, 17408, 18432, 19456, 20480, 21504, 22528, 23552, 24576, 25600, 26624, 28672,
    28672, 30720, 30720, 32768, 32768, 34816, 36864, 38912, 43008, 43008, 47104, 47104, 51200,
    51200, 57344, 57344, 57344, 65536, 65536, 65536, 65536, 73728, 73728, 86016, 86016, 86016,
    102400, 102400, 102400, 102400
];
pub(crate) const LOOKUP_TABLE: LookupTable = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    // The unused data is above. It is used as a filler allowing us to map an id to the index of
    // the vector without any shifts with only `LOOKUP_TABLE[id]`
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48,
    49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71,
    72, 73, 74, 74, 75, 75, 76, 76, 77, 78, 79, 80, 80, 81, 81, 82, 82, 83, 83, 83, 84, 84, 84,
    84, 85, 85, 86, 86, 86, 87, 87, 87, 87
];
pub(crate) const REAL_LARGE_SIZES: [usize; REAL_NUMBER_OF_LARGE_OBJECT_VECTORS] = {
    let mut arr = [0; REAL_NUMBER_OF_LARGE_OBJECT_VECTORS];
    let mut arr_len = 0;
    let mut i = 0;
    let mut prev = 0;
    
    while i < LARGE_SIZES_FOR_ID.len() {
        let curr = LARGE_SIZES_FOR_ID[i];
        if curr != prev {
            arr[arr_len] = curr;
            arr_len += 1;
            prev = curr;
        }
        
        i += 1;
    }
    
    assert!(arr_len == REAL_NUMBER_OF_LARGE_OBJECT_VECTORS);
    
    arr
};

const _: () = {
    assert!(LARGE_SIZE_MAX <= LARGE_CHUNK_SIZE);
};

pub(crate) struct LargeObjectTable {
    real_table: RealTable,
}

impl LargeObjectTable {
    pub(crate) const fn new() -> Self {
        Self {
            real_table: [const { SelfAllocatedVec::new() }; REAL_NUMBER_OF_LARGE_OBJECT_VECTORS],
        }
    }

    pub(crate) fn get_idx_by_id(&self, id: usize) -> usize {
        test_assert!(id >= FIRST_ID);
        test_assert!(id <= LAST_ID);

        unsafe { *LOOKUP_TABLE.get_unchecked(id) }
    }

    /// # Safety
    ///
    /// You know the real index, not only id.
    pub(crate) unsafe fn get_vec_by_real_idx(&self, idx: usize) -> &SelfAllocatedVec<NonNull<u8>> {
        test_assert!(self.real_table.len() > idx);

        unsafe {
            self.real_table.get_unchecked(idx)
        }
    }

    /// # Safety
    ///
    /// You know the real index, not only id.
    pub(crate) unsafe fn get_vec_mut_by_idx(&mut self, idx: usize) -> &mut SelfAllocatedVec<NonNull<u8>> {
        test_assert!(self.real_table.len() > idx);

        unsafe {
            self.real_table.get_unchecked_mut(idx)
        }
    }

    pub(crate) fn get_vec_mut_by_id(
        &mut self,
        id: usize
    ) -> &mut SelfAllocatedVec<NonNull<u8>> {
        let idx = self.get_idx_by_id(id); // `get_idx_by_id` checks if the id is valid

        unsafe {
            self.real_table.get_unchecked_mut(idx)
        }
    }

    pub(crate) fn self_allocated_memory(&self) -> impl Iterator<Item=usize> + use<'_> {
        self.real_table.iter().map(|v| v.bytes_allocated())
    }

    pub(crate) unsafe fn force_become_null(&mut self) {
        for vec in self.real_table.iter_mut() {
            unsafe { vec.force_become_null(); }
        }
    }
}

pub(crate) const fn large_size_to_id_and_real_size(size: usize) -> (usize, usize) {
    test_assert!(size > COMMON_SIZE_MAX as usize);
    test_assert!(size <= LARGE_SIZE_MAX);

    if unlikely(size.is_power_of_two()) {
        const TABLE_LEN: usize = {
            let mut num = (COMMON_SIZE_MAX as usize + 1).next_power_of_two();
            let mut res = 0;

            while num <= LARGE_SIZE_MAX {
                res += 1;
                num *= 2;
            }

            res
        };
        const FIRST_NUMBER_OF_TWO_IDX: usize = (COMMON_SIZE_MAX as usize + 1).next_power_of_two().trailing_zeros() as usize;
        const TABLE: [usize; FIRST_NUMBER_OF_TWO_IDX + TABLE_LEN] = {
            let mut num = (COMMON_SIZE_MAX as usize + 1).next_power_of_two();
            let mut res = [usize::MAX; FIRST_NUMBER_OF_TWO_IDX + TABLE_LEN];
            let mut res_len = 0;

            while num <= LARGE_SIZE_MAX {
                res[FIRST_NUMBER_OF_TWO_IDX + res_len] = find_large_slot_id(num);
                res_len += 1;
                num *= 2;
            }

            res
        };

        unsafe { assert_unchecked((size.trailing_zeros() as usize) < TABLE.len()) };

        return (TABLE[size.trailing_zeros() as usize], size);
    }

    let id = find_large_slot_id(size);

    unsafe { assert_unchecked(LARGE_SIZES_FOR_ID.len() > id); }

    (id, LARGE_SIZES_FOR_ID[id])
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const GENERATED_VECTOR_SIZES: [usize; REAL_NUMBER_OF_LARGE_OBJECT_VECTORS] = {
        let mut arr = [0; REAL_NUMBER_OF_LARGE_OBJECT_VECTORS];

        unsafe {
            copy_nonoverlapping(
                LARGE_SIZES_FOR_ID.as_ptr().add(FIRST_ID),
                arr.as_mut_ptr(),
                REAL_NUMBER_OF_LARGE_OBJECT_VECTORS
            );
        }

        arr
    };

    #[test]
    fn test_large_size_to_index_exhaustive() {
        for (size, expected_size, expected_idx) in [(1025, 1088, 0), (1260, 1280, 3), (63 * 1024, 64 * 1024, 84)] {
            let (id, real_size) = large_size_to_id_and_real_size(size);

            assert_eq!(real_size, expected_size);

            let real_size_by_id = LARGE_SIZES_FOR_ID[id];

            assert_eq!(real_size_by_id, expected_size);

            let idx = LOOKUP_TABLE[id];

            assert_eq!(idx, expected_idx);
        }

        let start = COMMON_SIZE_MAX as usize + 1;
        let end = LARGE_SIZE_MAX;

        for size in start..=end {
            let (id, real_size) = large_size_to_id_and_real_size(size);

            assert!(real_size >= size);

            assert!(
                id <= LAST_ID && id  >= FIRST_ID,
                "Id out of range for size {size}, id: {id}, FIRST_ID: {FIRST_ID}"
            );

            let chosen = LARGE_SIZES_FOR_ID[id];
            assert!(
                chosen >= size,
                "Returned size {} is smaller than requested {} for id {}",
                chosen,
                size,
                id
            );

            let real_chosen = LARGE_SIZES_FOR_ID[id];
            assert!(
                real_chosen >= size,
                "Returned size {} is smaller than requested {} for id {}",
                real_chosen,
                size,
                id
            );

            if id > FIRST_ID {
                let prev = GENERATED_SIZES_AND_INDEXES[id - FIRST_ID - 1].0;
                assert!(
                    prev < size,
                    "Previous size {} with id {} is not < requested {}, current {}",
                    prev,
                    id,
                    size,
                    chosen
                );

                let s = GENERATED_VECTOR_SIZES; // TODO r

                let real_prev = GENERATED_VECTOR_SIZES[LOOKUP_TABLE[id - 1]];
                assert!(
                    real_prev < size || real_prev == chosen,
                    "Previous size {} with id {} is not < requested {}, current {}",
                    real_prev,
                    id,
                    size,
                    chosen
                );
            }
        }
    }

    #[test]
    fn test_large_size_to_index_pow2_exact() {
        let pow2s = [
            2048usize, 4096, 8192, 16384, 32768, 65536
        ];

        for &p in &pow2s {
            let (id, real_size) = large_size_to_id_and_real_size(p);

            assert!(id < LAST_ID);
            assert_eq!(p, real_size);
        }
    }

    #[test]
    fn test_split_large_chunk_to_common() {
        let large_chunk = [0u8; LARGE_CHUNK_SIZE];
        let large_chunk_ptr = NonNull::from(&large_chunk);
        let commons = split_large_chunk_to_common(large_chunk_ptr);

        for i in 0..COMMON_CHUNKS_IN_LARGE_CHUNK {
            assert_eq!(commons[i].as_ptr() as usize, large_chunk_ptr.as_ptr() as usize + COMMON_CHUNK_SIZE * i);
        }
    }
}