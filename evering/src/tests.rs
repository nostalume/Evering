#![cfg(test)]

use crate::{
    boxed::PBox,
    mem::{Access, AddrSpec, MemAllocator, Mmap, RawMap},
    msg::{Envelope, Message, Move, Tag, TypeTag, type_id},
};

mod mock;
mod unix;

#[inline]
pub(crate) fn tracing_init() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .try_init();
}

#[inline]
pub(crate) fn prob(prob: f32) -> bool {
    fastrand::f32() < prob
}

pub(crate) trait MemBlkTestIO {
    unsafe fn write_bytes(&self, data: &[u8], len: usize, offset: usize);
    unsafe fn read_bytes(&self, buf: &mut [u8], len: usize, offset: usize);

    unsafe fn write_in(&self, data: &[u8], offset: usize) {
        unsafe { self.write_bytes(data, data.len(), offset) };
    }

    unsafe fn read_in(&self, len: usize, offset: usize) -> Vec<u8> {
        let mut buf = vec![0; len];
        unsafe { self.read_bytes(&mut buf, len, offset) };
        buf
    }

    unsafe fn write(&self, data: &[u8]) {
        unsafe { self.write_bytes(data, data.len(), 0) };
    }

    unsafe fn read(&self, len: usize) -> Vec<u8> {
        let mut buf = vec![0; len];
        unsafe { self.read_bytes(&mut buf, len, 0) };
        buf
    }
}

impl<S: AddrSpec, M: Mmap<S>> MemBlkTestIO for RawMap<S, M> {
    #[inline]
    unsafe fn write_bytes(&self, data: &[u8], len: usize, offset: usize) {
        use crate::mem::{Access, Accessible};

        debug_assert!(self.size() >= data.len() + offset);
        debug_assert!(data.len() >= len);

        if !self.spec.flags().permits(Access::WRITE) {
            panic!("[write]: permission denied")
        }
        unsafe {
            use crate::mem::MemOps;
            core::ptr::copy_nonoverlapping(data.as_ptr(), self.start_mut_ptr().add(offset), len)
        };
    }

    #[inline]
    unsafe fn read_bytes(&self, buf: &mut [u8], len: usize, offset: usize) {
        use crate::mem::{Access, Accessible};

        debug_assert!(self.size() >= buf.len() + offset);
        debug_assert!(buf.len() >= len);

        if !self.spec.flags().permits(Access::READ) {
            panic!("[read]: permission denied")
        }
        unsafe {
            use crate::mem::MemOps;
            core::ptr::copy_nonoverlapping(self.start_ptr().add(offset), buf.as_mut_ptr(), len)
        };
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Info {
    version: u32,
    data: u32,
}

impl Info {
    #[inline]
    pub fn mock() -> Self {
        Self {
            version: fastrand::u32(0..100),
            data: fastrand::u32(0..100),
        }
    }
}

impl TypeTag for Info {
    const TYPE_ID: crate::msg::TypeId = type_id::type_id("Info");
}

impl Message for Info {
    type Semantics = Move;
}

#[repr(transparent)]
#[derive(Clone, Copy)]
pub(crate) struct Byte {
    data: u8,
}

impl core::fmt::Debug for Byte {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        core::fmt::Debug::fmt(&self.data, f)
    }
}

impl Byte {
    #[inline]
    pub fn mock() -> Self {
        Self {
            data: fastrand::u8(0..128),
        }
    }
}

impl TypeTag for Byte {
    const TYPE_ID: crate::msg::TypeId = type_id::type_id("Info");
}

impl Message for Byte {
    type Semantics = Move;
}

#[derive(Debug)]
pub(crate) struct Infos<A: MemAllocator> {
    version: u32,
    data: PBox<[u8], A>,
}

impl<A: MemAllocator> Infos<A> {
    #[inline]
    pub fn mock(a: A) -> Self {
        Self {
            version: fastrand::u32(0..100),
            data: PBox::new_slice_in(fastrand::usize(0..128), |_| fastrand::u8(0..128), a),
        }
    }
}
