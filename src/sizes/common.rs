pub(crate) const COMMON_CHUNK_SIZE: usize = 64 * 1024;
pub(crate) type CommonRawChunk = [u8; COMMON_CHUNK_SIZE];

pub(crate) const COMMON_SIZE_MAX: u16 = 1024;
pub(crate) const COMMON_SIZE_TABLE_LEN: usize = (COMMON_SIZE_MAX / 8) as usize;
#[cfg(test)]
const POSSIBLE_COMMON_SIZES: [usize; COMMON_SIZE_TABLE_LEN] = {
    let mut sizes = [0; COMMON_SIZE_TABLE_LEN];
    let mut i = 0;

    while i < COMMON_SIZE_TABLE_LEN {
        sizes[i] = i * 8 + 8;

        i += 1;
    }

    sizes
};

pub(crate) const fn common_size_to_index(size: u16) -> usize {
    ((size - 1) >> 3) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_common_size_to_index() {
        let mut prev = 1;

        // These assertions make the debugging much easier.
        assert_eq!(common_size_to_index(1), 0);
        assert_eq!(common_size_to_index(8), 0);
        assert_eq!(common_size_to_index(9), 1);

        for idx in 0..COMMON_SIZE_TABLE_LEN {
            for i in 0..8 {
                assert_eq!(common_size_to_index(prev + i), idx);
            }

            prev += 8;
        }
    }
}
