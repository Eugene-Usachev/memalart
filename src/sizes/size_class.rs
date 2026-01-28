use core::fmt;
use std::fmt::Display;
use crate::sizes::{large_size_to_id_and_real_size, COMMON_SIZE_MAX, LARGE_SIZE_MAX};

#[derive(Copy, Clone)]
pub(crate) enum SizeClass {
    Common,
    Large,
    ExtraLarge,
}

impl SizeClass {
    pub(crate) const fn define(size: usize) -> Self {
        if size <= COMMON_SIZE_MAX as usize {
            SizeClass::Common
        } else if size <= LARGE_SIZE_MAX {
            SizeClass::Large
        } else {
            SizeClass::ExtraLarge
        }
    }
}

pub(crate) const fn recommended_size(min_size: usize) -> usize {
    if min_size <= COMMON_SIZE_MAX as usize {
        return (min_size + 7) & !7; // round it to a multiple of 8;
    }

    if min_size <= LARGE_SIZE_MAX {
        return large_size_to_id_and_real_size(min_size).1;
    }

    // TODO Huge

    min_size
}

impl Display for SizeClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            SizeClass::Common => "Common",
            SizeClass::Large => "Large",
            SizeClass::ExtraLarge => "ExtraLarge",
        };
        write!(f, "{name}")
    }
}