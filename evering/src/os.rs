#![cfg(feature = "std")]

#[cfg(feature = "unix")]
pub mod unix;

use self::unix::AddrSpec;
use crate::mem::MemBlkBuilder;

pub struct FdBackend;

impl MemBlkBuilder<AddrSpec, FdBackend> {
    fn fd() -> Self {
        Self::from_backend(FdBackend)
    }
}
