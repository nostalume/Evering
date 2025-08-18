#![cfg_attr(not(any(test, feature = "unix")), no_std)]
#![feature(allocator_api)]

extern crate alloc;

use alloc::sync::Arc;
use core::marker::PhantomData;

pub mod driver {
    pub use evering::driver::*;
}

pub mod uring {
    pub use evering::uring::bare::*;
    pub use evering::uring::{IReceiver, ISender, UringSpec};
}

pub mod shm {
    pub use evering_shm::shm_alloc::*;
    pub use evering_shm::shm_area::{ShmBackend, ShmSpec};
    pub mod boxed {
        pub use evering_shm::shm_box::*;
    }
    pub mod os {
        pub use evering_shm::os::FdBackend;

        #[cfg(feature = "unix")]
        pub use evering_shm::os::unix::*;
    }
}

use evering::driver::Driver;
use evering::driver::bare::{Completer, ReceiveBridge, SubmitBridge, box_client, box_server};
use evering::uring::bare::{BoxQueue, Boxed};
use evering_shm::shm_alloc::{ShmAlloc, ShmAllocError, ShmHeader, ShmInit};
use evering_shm::shm_area::{ShmBackend, ShmSpec};

pub trait IpcSpec {
    type A: ShmInit;
    type S: ShmSpec;
    type M: ShmBackend<Self::S>;
}

pub type IpcAlloc<I: IpcSpec> = Arc<ShmAlloc<I::A, I::S, I::M>>;
pub type IpcError<I: IpcSpec> = ShmAllocError<I::S, I::M>;
pub type IpcQueue<T, I, const N: usize> = BoxQueue<T, IpcAlloc<I>, N>;
pub type IpcSubmitter<D, I, const N: usize> = SubmitBridge<D, Boxed<IpcAlloc<I>>, N>;
pub type IpcReceiver<D, I, const N: usize> = ReceiveBridge<D, Boxed<IpcAlloc<I>>, N>;
pub type IpcCompleter<D, I, const N: usize> = Completer<D, Boxed<IpcAlloc<I>>, N>;

pub struct IpcHandle<I: IpcSpec, D: Driver, const N: usize>(IpcAlloc<I>, PhantomData<D>);

impl<I: IpcSpec, D: Driver, const N: usize> IpcHandle<I, D, N> {
    pub fn init_or_load(
        state: I::M,
        start: Option<<I::S as ShmSpec>::Addr>,
        size: usize,
        flags: <I::S as ShmSpec>::Flags,
        cfg: <I::M as ShmBackend<I::S>>::Config,
    ) -> Result<Self, IpcError<I>> {
        let area = ShmAlloc::init_or_load(state, start, size, flags, cfg)?;
        let area = Arc::new(area);
        Ok(IpcHandle(area, PhantomData))
    }

    unsafe fn queue<T>(&self, idx: usize) -> IpcQueue<T, I, N> {
        assert!(idx <= 2);
        loop {
            let arc = self.0.clone();
            match unsafe { arc.spec_in(idx) } {
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
        box_client(q_pair)
    }

    pub fn server(&self) -> IpcCompleter<D, I, N> {
        let q_pair = unsafe { self.queue_pair() };
        box_server::<D, _, N>(q_pair)
    }
}

#[cfg(test)]
mod tests {
    use core::marker::PhantomData;
    use core::time::Duration;
    use std::os::fd::{AsFd, OwnedFd};
    use std::sync::Arc;

    use evering::driver::unlocked::PoolDriver;
    use evering::uring::{IReceiver, ISender, UringSpec};
    use evering_shm::os::FdBackend;
    use evering_shm::os::unix::{MFdFlags, ProtFlags, UnixFdConf, UnixShm};
    use evering_shm::shm_alloc::tlsf::SpinTlsf;
    use tokio::task::yield_now;
    use tokio::time;

    use crate::{IpcCompleter, IpcHandle, IpcSpec};

    const SIZE: usize = 0x50000;
    const CAP: usize = 1 << 5;

    struct CharUring;
    impl UringSpec for CharUring {
        type SQE = char;
        type CQE = char;
    }
    type MyPoolDriver = PoolDriver<CharUring>;

    struct MyIpcSpec<F>(PhantomData<F>);
    impl<F: AsFd> IpcSpec for MyIpcSpec<F> {
        type A = SpinTlsf;
        type S = UnixShm;
        type M = FdBackend<F>;
    }

    type MyIpc<F> = IpcHandle<MyIpcSpec<F>, MyPoolDriver, CAP>;

    struct MyHandle;

    impl MyHandle {
        async fn try_handle_ref<F: AsFd, const N: usize>(
            cq: &IpcCompleter<MyPoolDriver, MyIpcSpec<F>, N>,
        ) {
            // use tokio::time::{self, Duration};
            loop {
                let mut f = false;
                while let Ok(ch) = cq.try_recv() {
                    f = true;
                    println!("[handle]: recv: {}", ch);
                    // time::sleep(Duration::from_millis(50)).await;
                    let res = fastrand::alphabetic();
                    if let Err(e) = cq.try_send(ch.replace(res)) {
                        println!("[handle]: send err: {}", e);
                    }
                    println!("[handle]: send: {}", res);
                }
                if !f {
                    time::sleep(Duration::from_micros(10)).await;
                }
            }
        }
    }

    fn init_or_load<F: AsFd>(size: usize, cfg: UnixFdConf<F>) -> Arc<MyIpc<F>> {
        let h = IpcHandle::init_or_load(
            FdBackend::new(),
            None,
            size,
            ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
            cfg,
        )
        .unwrap();
        Arc::new(h)
    }

    #[test]
    fn queue_test() {
        let cfg = UnixFdConf::default_from_mem_fd("test", SIZE, MFdFlags::empty()).unwrap();
        let handle = init_or_load(SIZE, cfg);
        let (pa, pb) = unsafe { handle.queue_pair::<char, char>() };
        let mut len_a = 0;
        let mut len_b = 0;
        for _ in 0..32 {
            let ch = fastrand::alphabetic();
            match fastrand::u8(0..4) {
                0 => len_a += pa.enqueue(ch).map_or(0, |_| 1),
                1 => len_b += pb.enqueue(ch).map_or(0, |_| 1),
                2 => {
                    if let Some(ch) = pa.dequeue() {
                        dbg!(format!("A recv: {}", ch));
                        len_b -= 1;
                    }
                }
                3 => {
                    if let Some(ch) = pb.dequeue() {
                        dbg!(format!("B recv: {}", ch));
                        len_a -= 1;
                    }
                }
                _ => unreachable!(),
            }
        }
        dbg!(format!("{}, {}", len_a, len_b));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 5)]
    async fn ipc_test() {
        let s_cfg = UnixFdConf::default_from_mem_fd("test", SIZE, MFdFlags::empty()).unwrap();
        let c_cfg = s_cfg.clone();

        tokio::spawn(async move {
            let handle = init_or_load(SIZE, s_cfg);
            let cq = handle.server();
            for _ in 0..5 {
                let cq = cq.clone();
                tokio::spawn(async move {
                    MyHandle::try_handle_ref(&cq).await;
                });
            }
        });
        tokio::spawn(async move {
            let handle = init_or_load(SIZE, c_cfg);
            let (sb, rb) = handle.client();
            let mut clients = Vec::new();
            for th in 0..5 {
                let sb = sb.clone();
                let rb = rb.clone();
                tokio::spawn(async move {
                    loop {
                        rb.try_complete();
                        yield_now().await;
                    }
                });

                let t = tokio::spawn(async move {
                    for i in 0..1000 {
                        let ch = fastrand::alphabetic();
                        println!("[submit {}]: send {}", th, ch);
                        let res = sb.try_submit(ch).unwrap().await;
                        println!("[submit {}]: recv {}: {}", th, i, res);
                        yield_now().await
                    }
                });
                clients.push(t);
            }
            for t in clients {
                t.await.unwrap();
            }
        });

        use tokio::time::{self, Duration};
        time::sleep(Duration::from_secs(5)).await;
    }
}
