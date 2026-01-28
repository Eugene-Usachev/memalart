use crate::limit::{allocate_any_way, mark_deallocated, may_allocate};
use crate::sizes::{CommonRawChunk, LargeRawChunk, COMMON_CHUNKS_IN_LARGE_CHUNK, COMMON_CHUNK_SIZE, LARGE_CHUNK_SIZE};
use crate::{NonLocalPtrManager, OffsetToSlotInTable};
use orengine_utils::hints::{unlikely, unwrap_or_bug_hint};
use std::array;
use std::ptr::{null_mut, NonNull};

#[cfg(not(test))]
pub(crate) const REGION_SIZE: usize = 32 * 1024 * 1024;

#[cfg(test)]
pub(crate) const REGION_SIZE: usize = crate::limit::MIN_TEST_LIMIT;

pub(crate) type RawRegion = [u8; REGION_SIZE];

// One chunk is for metadata of the region.
pub(crate) const LARGE_CHUNKS_IN_REGION: usize = (REGION_SIZE / LARGE_CHUNK_SIZE) - 1;
pub(crate) const REST_COMMON_CHUNKS_IN_REGION: usize = (LARGE_CHUNK_SIZE / COMMON_CHUNK_SIZE) - 1;
pub(crate) const MAX_COMMON_CHUNKS_IN_REGION: usize = (LARGE_CHUNKS_IN_REGION * COMMON_CHUNKS_IN_LARGE_CHUNK) + REST_COMMON_CHUNKS_IN_REGION;

#[inline(always)]
fn alloc_region_() -> *mut RawRegion {
    let (ptr, _) = unsafe {
        crate::os::alloc_aligned_from_os(
            null_mut(),
            REGION_SIZE,
            true,        // commit
            false,       // huge_only
            REGION_SIZE, // alignment
        )
    };

    if unlikely(ptr.is_null()) {
        mark_deallocated(REGION_SIZE);

        return null_mut();
    }

    if cfg!(test) {
        unsafe { ptr.write_bytes(0, REGION_SIZE) };
    }

    ptr.cast()
}

pub(crate) unsafe fn alloc_region() -> *mut RawRegion {
    if unlikely(!may_allocate(REGION_SIZE)) {
        return null_mut();
    }

    alloc_region_()
}

pub(crate) unsafe fn alloc_region_without_counting_limit() -> *mut RawRegion {
    allocate_any_way(REGION_SIZE);

    alloc_region_()
}

#[cfg(test)]
pub(crate) unsafe fn alloc_region_without_touching_limit() -> *mut RawRegion {
    alloc_region_()
}

pub(crate) unsafe fn dealloc_region(region: NonNull<RawRegion>) {
    unsafe { crate::os::free(region.cast().as_ptr(), REGION_SIZE) };

    mark_deallocated(REGION_SIZE);
}

#[cfg(test)]
pub(crate) unsafe fn dealloc_region_without_touching_limit(region: NonNull<RawRegion>) {
    unsafe { crate::os::free(region.cast().as_ptr(), REGION_SIZE) };
}

pub(crate) struct ChunkMetadata<NLPM = ()> {
    non_local_ptr_manager: NonNull<NLPM>,
    offset_to_slot_in_table: OffsetToSlotInTable,
}

impl ChunkMetadata {
    pub(crate) fn offset_to_slot_in_table(&self) -> OffsetToSlotInTable {
        self.offset_to_slot_in_table
    }

    pub(crate) fn non_local_ptr_manager<NLPM>(&self) -> &NLPM {
        unsafe { self.non_local_ptr_manager.cast().as_ref() }
    }

    pub(crate) fn non_local_ptr_manager_exist(&self) -> bool {
        self.non_local_ptr_manager.as_ptr() != null_mut()
    }
}

impl<NLPM> Clone for ChunkMetadata<NLPM> {
    fn clone(&self) -> Self {
        Self {
            non_local_ptr_manager: self.non_local_ptr_manager,
            offset_to_slot_in_table: self.offset_to_slot_in_table,
        }
    }
}

impl<NLPM> Copy for ChunkMetadata<NLPM> {}

const CHUNK_METADATA_SIZE: usize = size_of::<ChunkMetadata>();

const _: () = {
    assert!(
        CHUNK_METADATA_SIZE * MAX_COMMON_CHUNKS_IN_REGION <= COMMON_CHUNK_SIZE,
        "Region metadata size is too big."
    );
};

#[cfg(test)]
struct RegionMetadata {
    common_chunks_metadata: [ChunkMetadata; MAX_COMMON_CHUNKS_IN_REGION],
}

pub(crate) struct ParsedRegion {
    #[cfg(test)]
    pub(crate) metadata: NonNull<CommonRawChunk>,
    pub(crate) common_chunks: [NonNull<CommonRawChunk>; REST_COMMON_CHUNKS_IN_REGION],
    pub(crate) large_chunks: [NonNull<LargeRawChunk>; LARGE_CHUNKS_IN_REGION],
}

pub(crate) fn parse_region(region: NonNull<RawRegion>) -> ParsedRegion {
    let metadata: NonNull<()> = region.cast();
    let mut curr = unsafe { metadata.cast::<u8>().byte_add(COMMON_CHUNK_SIZE) };
    let common_chunks = array::from_fn::<_, REST_COMMON_CHUNKS_IN_REGION, _>(|_| unsafe {
        let new = curr.cast();

        curr = curr.byte_add(COMMON_CHUNK_SIZE);

        new
    });
    let large_chunks = array::from_fn::<_, LARGE_CHUNKS_IN_REGION, _>(|_| unsafe {
        let new = curr.cast();

        curr = curr.byte_add(LARGE_CHUNK_SIZE);

        new
    });

    // Metadata is initialized lazy when it is necessary.

    ParsedRegion {
        #[cfg(test)]
        metadata: metadata.cast(),
        common_chunks,
        large_chunks,
    }
}

pub(crate) fn region_ptr_from_chunk_ptr(chunk_ptr: NonNull<u8>) -> NonNull<RawRegion> {
    unwrap_or_bug_hint(NonNull::new(
        ((chunk_ptr.as_ptr() as usize) & !(REGION_SIZE - 1)) as *mut RawRegion,
    ))
}

pub(crate) fn chunk_ptr_to_chunk_metadata_ptr(
    chunk_ptr: NonNull<u8>,
) -> NonNull<ChunkMetadata> {
    let region_start = (chunk_ptr.as_ptr() as usize) & !(REGION_SIZE - 1);
    let shift_from_start = (chunk_ptr.as_ptr() as usize) - region_start;
    let idx = shift_from_start / COMMON_CHUNK_SIZE;

    unsafe {
        NonNull::new_unchecked(
            (region_start as *mut u8)
                .byte_add(idx * CHUNK_METADATA_SIZE)
                .cast::<ChunkMetadata>(),
        )
    }
}

pub(crate) fn init_metadata_for_common_chunk<NLPM: NonLocalPtrManager>(
    non_local_ptr_manager: NonNull<NLPM>,
    offset_to_slot_in_table: OffsetToSlotInTable,
    chunk_ptr: NonNull<u8>,
) {
    if cfg!(test) {
        let is_common = offset_to_slot_in_table.is_common();

        assert!(is_common);
    }

    let metadata_ptr = chunk_ptr_to_chunk_metadata_ptr(chunk_ptr);

    unsafe {
        metadata_ptr.cast().write(ChunkMetadata {
            non_local_ptr_manager,
            offset_to_slot_in_table,
        });
    };
}

pub(crate) fn init_metadata_for_large_chunk<NLPM: NonLocalPtrManager>(
    non_local_ptr_manager: NonNull<NLPM>,
    offset_to_slot_in_table: OffsetToSlotInTable,
    chunk_ptr: NonNull<u8>,
) {
    if cfg!(test) {
        let is_common = offset_to_slot_in_table.is_common();

        assert!(!is_common);
    }

    let mut metadata_ptr = chunk_ptr_to_chunk_metadata_ptr(chunk_ptr);

    for _ in 0..COMMON_CHUNKS_IN_LARGE_CHUNK {
        unsafe {
            metadata_ptr.cast().write(ChunkMetadata {
                non_local_ptr_manager,
                offset_to_slot_in_table,
            });
        };

        unsafe { metadata_ptr = metadata_ptr.byte_add(CHUNK_METADATA_SIZE); }
    }
}

#[cfg(test)]
pub(crate) fn is_region_empty(region: NonNull<RawRegion>) -> bool {
    static ZERO: [u8; 4096] = [0; 4096];

    let mut p = region.as_ptr().cast::<libc::c_void>();
    let end = unsafe { p.add(REGION_SIZE) };

    while unsafe { p.byte_add(4096) } <= end {
        if unsafe { libc::memcmp(p as *const _, ZERO.as_ptr() as *const _, 4096) } != 0 {
            return false;
        }

        p = unsafe { p.byte_add(4096) };
    }

    let tail = end as usize - p as usize;
    if tail > 0 {
        unsafe { libc::memcmp(p as *const _, ZERO.as_ptr() as *const _, tail) == 0 }
    } else {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utilities::acquire_test_lock;
    use std::collections::HashSet;
    use std::mem;
    use crate::non_local_ptr_manager::LockFreeNonLocalPtrManager;

    #[test]
    fn test_parse_region() {
        let test_lock = acquire_test_lock();

        let region = unsafe { alloc_region() };
        let parsed_region = parse_region(NonNull::new(region).expect("Failed to allocate region"));

        assert_eq!(parsed_region.metadata.as_ptr() as usize, region as usize);
        assert_eq!(
            parsed_region.common_chunks.len(),
            REST_COMMON_CHUNKS_IN_REGION
        );
        assert_eq!(parsed_region.large_chunks.len(), LARGE_CHUNKS_IN_REGION);

        let mut ptrs: HashSet<*mut u8> = HashSet::new();

        ptrs.insert(parsed_region.metadata.as_ptr().cast());

        for ptr in parsed_region.common_chunks {
            assert!(ptrs.insert(ptr.as_ptr().cast()), "Duplicate common chunk");
        }

        for ptr in parsed_region.large_chunks {
            assert!(ptrs.insert(ptr.as_ptr().cast()), "Duplicate large chunk");
        }

        unsafe {
            dealloc_region(NonNull::new(region).expect("Failed to allocate region"));
        };

        drop(test_lock);
    }

    #[test]
    fn test_region_is_empty() {
        let test_lock = acquire_test_lock();
        let region = NonNull::new(unsafe { alloc_region() }).expect("Failed to allocate region");

        assert!(
            is_region_empty(region),
            "The region should be empty after init in tests"
        );

        unsafe { region.write_bytes(1, 1); }

        assert!(
            !is_region_empty(region),
            "The region should not be empty after a write operation"
        );

        unsafe {
            dealloc_region(region);
        };

        drop(test_lock);
    }

    #[test]
    fn init_common_chunks() {
        type RawMetadata = [u8; size_of::<ChunkMetadata>()];

        const ZEROED_METADATA: RawMetadata = unsafe { mem::zeroed() };

        let test_lock = acquire_test_lock();
        let region = NonNull::new(unsafe { alloc_region() }).expect("Failed to allocate region");
        let parsed_region = parse_region(region);
        let nlpm = LockFreeNonLocalPtrManager::new();
        let nlpm_ptr = NonNull::from(&nlpm);

        // unsafe { region.write_bytes(0, REGION_SIZE); } is already done in `alloc_region`.

        for idx in 0..REST_COMMON_CHUNKS_IN_REGION {
            let chunk_ptr = parsed_region.common_chunks[idx].cast();
            let metadata_ptr = chunk_ptr_to_chunk_metadata_ptr(chunk_ptr);

            assert_eq!(unsafe { metadata_ptr.cast::<RawMetadata>().read() }, ZEROED_METADATA);

            init_metadata_for_common_chunk(
                nlpm_ptr,
                OffsetToSlotInTable::new_for_common_chunk(idx),
                chunk_ptr
            );
        }

        for large_idx in 0..LARGE_CHUNKS_IN_REGION {
            for common_idx in 0..COMMON_CHUNKS_IN_LARGE_CHUNK {
                let chunk_ptr = unsafe {
                    parsed_region.large_chunks[large_idx].cast::<CommonRawChunk>().add(common_idx)
                }.cast();
                let metadata_ptr = chunk_ptr_to_chunk_metadata_ptr(chunk_ptr);

                assert_eq!(unsafe { metadata_ptr.cast::<RawMetadata>().read() }, ZEROED_METADATA);

                init_metadata_for_common_chunk(
                    nlpm_ptr,
                    OffsetToSlotInTable::new_for_common_chunk(large_idx * COMMON_CHUNKS_IN_LARGE_CHUNK + common_idx),
                    chunk_ptr
                );
            }
        }

        for idx in 0..REST_COMMON_CHUNKS_IN_REGION {
            let chunk_ptr = parsed_region.common_chunks[idx].cast();
            let metadata_ptr = chunk_ptr_to_chunk_metadata_ptr(chunk_ptr)
                .cast::<ChunkMetadata<LockFreeNonLocalPtrManager>>();

            let metadata = unsafe { metadata_ptr.read() };

            assert_ne!(
                unsafe { metadata_ptr.cast::<RawMetadata>().read() },
                ZEROED_METADATA
            );

            assert_eq!(metadata.non_local_ptr_manager.as_ptr(), nlpm_ptr.as_ptr());

            let expected_offset = OffsetToSlotInTable::new_for_common_chunk(idx);
            assert_eq!(
                format!("{:?}", metadata.offset_to_slot_in_table),
                format!("{:?}", expected_offset)
            );
        }

        for large_idx in 0..LARGE_CHUNKS_IN_REGION {
            for common_idx in 0..COMMON_CHUNKS_IN_LARGE_CHUNK {
                let chunk_ptr = unsafe {
                    parsed_region.large_chunks[large_idx]
                        .cast::<CommonRawChunk>()
                        .add(common_idx)
                }
                    .cast();

                let metadata_ptr = chunk_ptr_to_chunk_metadata_ptr(chunk_ptr)
                    .cast::<ChunkMetadata<LockFreeNonLocalPtrManager>>();

                let metadata = unsafe { metadata_ptr.read() };

                assert_ne!(
                    unsafe { metadata_ptr.cast::<RawMetadata>().read() },
                    ZEROED_METADATA
                );

                assert_eq!(metadata.non_local_ptr_manager.as_ptr(), nlpm_ptr.as_ptr());

                let expected_offset = OffsetToSlotInTable::new_for_common_chunk(
                    large_idx * COMMON_CHUNKS_IN_LARGE_CHUNK + common_idx,
                );
                assert_eq!(
                    format!("{:?}", metadata.offset_to_slot_in_table),
                    format!("{:?}", expected_offset)
                );
            }
        }

        unsafe { dealloc_region(region) };
        drop(test_lock);
    }
}
