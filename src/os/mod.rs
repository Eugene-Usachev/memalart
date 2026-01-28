mod sys;
#[cfg(test)]
mod test;
mod unix;
mod vec;

#[cfg(unix)]
pub use unix::*;
pub(crate) use vec::*;
