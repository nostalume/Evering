#![cfg(feature = "std")]

#[cfg(feature = "unix")]
pub mod unix;

use core::marker::PhantomData;
use std::os::fd::AsFd;

pub struct FdBackend<F: AsFd>(PhantomData<F>);
