#![cfg(test)]

use crate::mem::{AddrSpec, Mmap, RawMemBlk};

mod mock;
mod unix;

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

impl<S: AddrSpec, M: Mmap<S>> MemBlkTestIO for RawMemBlk<S, M> {
    #[inline]
    unsafe fn write_bytes(&self, data: &[u8], len: usize, offset: usize) {
        debug_assert!(self.size() >= data.len() + offset);
        debug_assert!(data.len() >= len);
        unsafe {
            use crate::mem::MemBlkOps;
            core::ptr::copy_nonoverlapping(data.as_ptr(), self.start_mut_ptr().add(offset), len)
        };
    }

    #[inline]
    unsafe fn read_bytes(&self, buf: &mut [u8], len: usize, offset: usize) {
        debug_assert!(self.size() >= buf.len() + offset);
        debug_assert!(buf.len() >= len);
        unsafe {
            use crate::mem::MemBlkOps;
            core::ptr::copy_nonoverlapping(self.start_ptr().add(offset), buf.as_mut_ptr(), len)
        };
    }
}
