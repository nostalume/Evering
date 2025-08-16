#![cfg_attr(not(test), no_std)]
#![feature(allocator_api)]

extern crate alloc;

use alloc::alloc::Allocator;
use evering_shm::shm_box::{ShmBox, ShmToken};

use core::marker::PhantomData;
use core::ptr::NonNull;
use evering::uring::UringSpec;
use evering::uring::asynch::{Uring, default_channel_in};
use evering_shm::shm_alloc::{ShmAlloc, ShmHeader, ShmInit};
use evering_shm::shm_area::{ShmBackend, ShmSpec};

struct IpcHandle<A: ShmInit, S: ShmSpec, M: ShmBackend<S>, U: UringSpec>(
    ShmAlloc<A, S, M>,
    PhantomData<U>,
);

impl<U: UringSpec, S: ShmSpec, M: ShmBackend<S>, A: ShmInit> IpcHandle<A, S, M, U> {
    pub fn init_or_load(&self) -> NonNull<[u8; 100]> {
        loop {
            match self.0.spec_raw::<_>() {
                Some(u) => break u,
                None => {
                    let u = ShmBox::new_in([1u8; 100], &self.0);
                    unsafe {
                        self.0.init_spec_raw(u.as_ref());
                    }
                }
            }
        }
    }

    pub fn u(&self) -> NonNull<lfqueue::ConstBoundedQueue<char,16>> {
        loop {
            match self.0.spec_raw::<_>() {
                Some(u) => break u,
                None => {
                    let u = ShmBox::new_in(lfqueue::ConstBoundedQueue::<char,16>::new_const(), &self.0);
                    self.0.init_spec(u);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::marker::PhantomData;
    use std::os::fd::OwnedFd;
    use std::sync::Arc;

    use evering::uring::UringSpec;
    use evering_shm::os::unix::{FdBackend, FdConfig, FdShmSpec, MFdFlags, ProtFlags};
    use evering_shm::shm_alloc::ShmSpinGma;

    use crate::IpcHandle;

    type MyShm = ShmSpinGma<FdShmSpec<OwnedFd>, FdBackend>;

    struct CharUring;

    impl UringSpec for CharUring {
        type SQE = char;
        type CQE = char;
    }

    fn create(size: usize) -> MyShm {
        let cfg = FdConfig::default_from_mem_fd("test", MFdFlags::empty()).unwrap();
        let m = MyShm::init_or_load(
            FdBackend,
            None,
            size,
            ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
            cfg,
        )
        .unwrap();
        m
    }

    #[test]
    fn c() {
        let m = create(0x2000);
        let h = IpcHandle(m, PhantomData::<CharUring>);
        let mut u = h.init_or_load();
        unsafe {
            let l = u.as_mut();
            for i in l.iter_mut() {
                dbg!("{}", *i);
            }
        }
    }
    
    #[test]
    fn u() {
        let m = create(0x2000);
        let h = IpcHandle(m, PhantomData::<CharUring>);
        let mut u = h.u();
        unsafe {
            let l = u.as_mut();
            
            let ar = Arc::new(l);
            let w = ar.capacity();
            dbg!("{}", w);
            let _ = ar.enqueue('a');
        }
    }
}
