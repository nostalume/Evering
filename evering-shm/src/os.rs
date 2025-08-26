#![cfg(feature = "std")]

#[cfg(feature = "unix")]
pub mod unix;

use std::os::fd::AsFd;

pub struct FdBackend<F: AsFd>(core::marker::PhantomData<F>);

impl<F: AsFd> Default for FdBackend<F> {
    fn default() -> Self {
        Self::new()
    }
}

impl<F: AsFd> FdBackend<F> {
    #[inline]
    pub const fn new() -> Self {
        Self(core::marker::PhantomData)
    }
}
