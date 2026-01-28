macro_rules! test_assert {
    ($($args:tt)*) => {
        if cfg!(test) {
            assert!($($args)*);
        }
    };
}

pub(crate) use test_assert;
