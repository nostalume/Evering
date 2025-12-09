#![cfg(feature = "std")]

#[cfg(feature = "unix")]
pub mod unix;

use self::unix::AddrSpec;
use crate::mem::MapBuilder;

pub struct FdBackend;

impl MapBuilder<AddrSpec, FdBackend> {
    pub fn fd() -> Self {
        Self::from_backend(FdBackend)
    }
}
