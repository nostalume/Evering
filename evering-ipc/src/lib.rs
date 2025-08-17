#![cfg_attr(not(test), no_std)]
#![feature(allocator_api)]

extern crate alloc;

use core::marker::PhantomData;

use evering::driver::Driver;
use evering::driver::bare::{Completer, ReceiveBridge, SubmitBridge, box_default};
use evering::uring::bare::{BoxQueue, Boxed};
use evering_shm::shm_alloc::{ShmAlloc, ShmAllocError, ShmHeader, ShmInit};
use evering_shm::shm_area::{ShmBackend, ShmSpec};

pub trait IpcSpec {
    type A: ShmInit;
    type S: ShmSpec;
    type M: ShmBackend<Self::S>;
}

type IpcAlloc<I: IpcSpec> = ShmAlloc<I::A, I::S, I::M>;
type IpcError<I: IpcSpec> = ShmAllocError<I::S, I::M>;
type IpcQueue<'a, T, I, const N: usize> = BoxQueue<T, &'a IpcAlloc<I>, N>;
type IpcSubmitter<'a, D, I, const N: usize> = SubmitBridge<D, Boxed<&'a IpcAlloc<I>>, N>;
type IpcReceiver<'a, D, I, const N: usize> = ReceiveBridge<D, Boxed<&'a IpcAlloc<I>>, N>;
type IpcCompleter<'a, D, I, const N: usize> = Completer<D, Boxed<&'a IpcAlloc<I>>, N>;

struct IpcHandle<I: IpcSpec, D: Driver, const N: usize>(ShmAlloc<I::A, I::S, I::M>, PhantomData<D>);

impl<I: IpcSpec, D: Driver, const N: usize> IpcHandle<I, D, N> {
    pub fn init_or_load(
        state: I::M,
        start: Option<<I::S as ShmSpec>::Addr>,
        size: usize,
        flags: <I::S as ShmSpec>::Flags,
        cfg: <I::M as ShmBackend<I::S>>::Config,
    ) -> Result<Self, IpcError<I>> {
        let area = ShmAlloc::init_or_load(state, start, size, flags, cfg)?;
        Ok(IpcHandle(area, PhantomData))
    }

    unsafe fn queue<T>(&self, idx: usize) -> BoxQueue<T, &IpcAlloc<I>, N> {
        assert!(idx <= 2);
        loop {
            match unsafe { self.0.spec::<_>(idx) } {
                Some(u) => break u,
                None => {
                    let q = Boxed::new::<T, N>(&self.0);
                    // let q2 = Boxed::new(&self.0);
                    self.0.init_spec(q, idx);
                }
            }
        }
    }

    pub unsafe fn queue_pair<T, U>(&self) -> (IpcQueue<'_, T, I, N>, IpcQueue<'_, U, I, N>) {
        let q1 = unsafe { self.queue::<T>(0) };
        let q2 = unsafe { self.queue::<U>(1) };

        (q1, q2)
    }

    pub fn client(&self) -> (IpcSubmitter<'_, D, I, N>, IpcReceiver<'_, D, I, N>) {
        let q_pair = unsafe { self.queue_pair() };
        let (sb, cb, _) = box_default(q_pair);
        (sb, cb)
    }

    pub fn server(&self) -> IpcCompleter<'_, D, I, N> {
        let q_pair = unsafe { self.queue_pair() };
        let (_, _, cq) = box_default(q_pair);
        cq
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;
    use std::io::{self, Write};
    use std::os::fd::OwnedFd;

    use evering::driver::unlocked::PoolDriver;
    use evering::uring::UringSpec;
    use evering_shm::os::unix::{FdBackend, FdConfig, MFdFlags, ProtFlags, UnixShm};
    use evering_shm::shm_alloc::tlsf::SpinTlsf;
    use tokio::task::yield_now;
    use tokio::time;

    use crate::{IpcCompleter, IpcHandle, IpcSpec};

    const SIZE: usize = 0x50000;

    struct CharUring;
    impl UringSpec for CharUring {
        type SQE = char;
        type CQE = char;
    }
    type MyPoolDriver = PoolDriver<CharUring>;

    struct MyIpcSpec;
    impl IpcSpec for MyIpcSpec {
        type A = SpinTlsf;
        type S = UnixShm;
        type M = FdBackend<OwnedFd>;
    }

    type MyIpc = IpcHandle<MyIpcSpec, MyPoolDriver, { 1 << 5 }>;

    struct MyHandle;

    impl MyHandle {
        async fn try_handle_ref<const N: usize>(cq: &IpcCompleter<'_, MyPoolDriver, MyIpcSpec, N>) {
            // use tokio::time::{self, Duration};
            loop {
                let mut f = false;
                while let Some(ch) = cq.try_recv() {
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

    fn create(size: usize) -> MyIpc {
        let cfg = FdConfig::default_from_mem_fd("test", MFdFlags::empty()).unwrap();
        IpcHandle::init_or_load(
            FdBackend::new(),
            None,
            size,
            ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
            cfg,
        )
        .unwrap()
    }

    #[test]
    fn queue_test() {
        let handle = create(SIZE);
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
        let handle = create(SIZE);
        // leak to acquire 'static lifetime.
        let leak = Box::leak(Box::new(handle));
        let (sb, rb) = leak.client();
        let cq = leak.server();
        for _ in 0..5 {
            let cq = cq.clone();
            tokio::spawn(async move {
                MyHandle::try_handle_ref(&cq).await;
            });
        }

        for th in 0..5 {
            let sb = sb.clone();
            let rb = rb.clone();
            tokio::spawn(async move {
                loop {
                    rb.try_complete();
                    yield_now().await;
                }
            });

            tokio::spawn(async move {
                for i in 0..1000 {
                    let ch = fastrand::alphabetic();
                    println!("[submit {}]: send {}", th, ch);
                    io::stdout().flush().unwrap();
                    let res = sb.try_submit(ch).unwrap().await;
                    time::sleep(Duration::from_micros(50)).await;
                    println!("[submit {}]: recv {}: {}", th, i, res);
                    yield_now().await
                }
            });
        }

        use tokio::time::{self, Duration};
        time::sleep(Duration::from_secs(15)).await;
    }
}
