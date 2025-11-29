#![cfg(feature = "std")]

#[cfg(feature = "unix")]
pub mod unix;

pub struct FdBackend;
