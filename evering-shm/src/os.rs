#![cfg(feature ="std")]

use std::os::fd::AsFd;

#[cfg(feature = "unix")]
pub mod unix;

pub struct FdBackend<F: AsFd>(core::marker::PhantomData<F>);

impl<F: AsFd> FdBackend<F> {
    #[inline]
    pub const fn new() -> Self {
        Self(core::marker::PhantomData)
    }
}