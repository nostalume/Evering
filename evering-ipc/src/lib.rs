#![cfg_attr(not(any(test, feature = "unix")), no_std)]
#![feature(allocator_api)]

extern crate alloc;

use alloc::sync::Arc;
use core::marker::PhantomData;
use core::mem::ManuallyDrop;

mod tests;

pub mod driver {
    pub use evering::driver::*;
}

pub mod uring {
    pub use evering::uring::bare::*;
    pub use evering::uring::{IReceiver, ISender, UringSpec};
}

pub mod shm {
    pub use evering_shm::perlude::{AddrSpec, Mmap};
    pub mod boxed {
        pub use evering_shm::boxed::*;
    }
    pub mod os {
        pub use evering_shm::os::FdBackend;

        #[cfg(feature = "unix")]
        pub mod unix {
            pub use evering_shm::os::unix::*;
        }
    }
}

use evering::driver::Driver;
use evering::driver::bare::{Completer, ReceiveBridge, SubmitBridge, box_client, box_server};
use evering::uring::bare::{BoxQueue, Boxed};
use evering_shm::perlude::{AddrSpec, Mmap, ShmAlloc, ShmAllocError, ShmHeader, ShmInit};

pub trait IpcSpec {
    type A: ShmInit;
    type S: AddrSpec;
    type M: Mmap<Self::S>;
}

pub type IpcAlloc<I: IpcSpec> = Arc<ShmAlloc<I::A, I::S, I::M>>;
pub type IpcAllocRef<'a, I: IpcSpec> = &'a ShmAlloc<I::A, I::S, I::M>;
pub type IpcError<I: IpcSpec> = ShmAllocError<I::S, I::M>;
pub type IpcQueue<T, I, const N: usize> = BoxQueue<T, IpcAlloc<I>, N>;
pub type IpcSubmitter<D, I, const N: usize> = ManuallyDrop<SubmitBridge<D, Boxed<IpcAlloc<I>>, N>>;
pub type IpcReceiver<D, I, const N: usize> = ManuallyDrop<ReceiveBridge<D, Boxed<IpcAlloc<I>>, N>>;
pub type IpcCompleter<D, I, const N: usize> = ManuallyDrop<Completer<D, Boxed<IpcAlloc<I>>, N>>;

pub struct IpcHandle<I: IpcSpec, D: Driver, const N: usize>(IpcAlloc<I>, PhantomData<D>);

impl<I: IpcSpec, D: Driver, const N: usize> IpcHandle<I, D, N> {
    pub fn alloc(&self) -> IpcAlloc<I> {
        self.0.clone()
    }

    pub fn init_or_load(
        state: I::M,
        start: Option<<I::S as AddrSpec>::Addr>,
        size: usize,
        flags: <I::S as AddrSpec>::Flags,
        cfg: <I::M as Mmap<I::S>>::Config,
    ) -> Result<Self, IpcError<I>> {
        let area = ShmAlloc::init_or_load(state, start, size, flags, cfg)?;
        let area = Arc::new(area);
        Ok(IpcHandle(area, PhantomData))
    }

    unsafe fn queue_ref<T>(&self, idx: usize) -> BoxQueue<T, IpcAllocRef<'_, I>, N> {
        assert!(idx <= 2);
        loop {
            match unsafe { self.0.as_ref().spec_ref(idx) } {
                Some(u) => break u,
                None => {
                    let q = Boxed::new::<T, N>(self.0.clone());
                    // let q2 = Boxed::new(&self.0);
                    self.0.init_spec(q, idx);
                }
            }
        }
    }

    unsafe fn queue<T>(&self, idx: usize) -> IpcQueue<T, I, N> {
        assert!(idx <= 2);
        loop {
            match unsafe { self.0.spec(idx) } {
                Some(u) => break u,
                None => {
                    let q = Boxed::new::<T, N>(self.0.clone());
                    // let q2 = Boxed::new(&self.0);
                    self.0.init_spec(q, idx);
                }
            }
        }
    }

    pub unsafe fn queue_pair<T, U>(&self) -> (IpcQueue<T, I, N>, IpcQueue<U, I, N>) {
        let q1 = unsafe { self.queue::<T>(0) };
        let q2 = unsafe { self.queue::<U>(1) };

        (q1, q2)
    }

    pub fn client(&self) -> (IpcSubmitter<D, I, N>, IpcReceiver<D, I, N>) {
        let q_pair = unsafe { self.queue_pair() };
        let (sb, rb) = box_client(q_pair);
        (ManuallyDrop::new(sb), ManuallyDrop::new(rb))
    }

    pub fn server(&self) -> IpcCompleter<D, I, N> {
        let q_pair = unsafe { self.queue_pair() };
        let cp = box_server::<D, _, N>(q_pair);
        ManuallyDrop::new(cp)
    }
}
