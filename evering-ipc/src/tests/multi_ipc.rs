#![cfg(test)]

use core::marker::PhantomData;
use core::time::Duration;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;

use evering::driver::unlocked::PoolDriver;
use evering::uring::{IReceiver, ISender, UringSpec};
use evering_shm::os::FdBackend;
use evering_shm::os::unix::{MFdFlags, ProtFlags, UnixFdConf, UnixAddrSpec};
use evering_shm::alloc::tlsf::SpinTlsf;
use evering_shm::boxed::{ShmBox, ShmSized, ShmSlice, ShmToken};
use tokio::task::yield_now;
use tokio::time;

use crate::{IpcAlloc, IpcCompleter, IpcHandle, IpcSpec};

const SIZE: usize = 0x50000;
const SLICE: &[u8] = &[1u8; 100];
const SLICE2: &[u8] = &[2u8; 100];
const CAP: usize = 1 << 5;

struct Bsl<I: IpcSpec>(PhantomData<I>);
impl<I: IpcSpec> UringSpec for Bsl<I> {
    type SQE = ShmToken<u8, IpcAlloc<I>, ShmSlice>;
    type CQE = ShmToken<u8, IpcAlloc<I>, ShmSlice>;
}
type MyPoolDriver<I> = PoolDriver<Bsl<I>>;

struct MyIpcSpec<F>(PhantomData<F>);
impl<F: AsFd> IpcSpec for MyIpcSpec<F> {
    type A = SpinTlsf;
    type S = UnixAddrSpec;
    type M = FdBackend<F>;
}

type MyIpc<F> = IpcHandle<MyIpcSpec<F>, MyPoolDriver<MyIpcSpec<F>>, CAP>;

struct MyHandle;

impl MyHandle {
    async fn try_handle_ref<F: AsFd + 'static, const N: usize>(
        cq: &IpcCompleter<MyPoolDriver<MyIpcSpec<F>>, MyIpcSpec<F>, N>,
    ) {
        // use tokio::time::{self, Duration};
        loop {
            let mut f = false;
            while let Ok(msg) = cq.try_recv() {
                f = true;
                println!("[handle]: recv correctly");
                let ans = msg.map(|r| {
                    let mut s = r.into_box();
                    s.copy_from_slice(SLICE);
                    s.into()
                });
                if let Err(_) = cq.try_send(ans) {
                    println!("[handle]: send err");
                }
            }
            if !f {
                time::sleep(Duration::from_micros(10)).await;
            }
        }
    }
}

fn init_or_load<F: AsFd + 'static>(size: usize, cfg: UnixFdConf<F>) -> Arc<MyIpc<F>> {
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
    let cfg = UnixFdConf::default_mem("test", SIZE, MFdFlags::empty()).unwrap();
    let handle = init_or_load(SIZE, cfg);

    type BooT = ShmToken<u8, IpcAlloc<MyIpcSpec<OwnedFd>>, ShmSlice>;
    let (pa, pb) = unsafe { handle.queue_pair::<BooT, BooT>() };
    let mut len_a = 0;
    let mut len_b = 0;
    for _ in 0..32 {
        let ba = ShmBox::copy_from_slice(SLICE, handle.alloc());
        let bb = ShmBox::copy_from_slice(SLICE2, handle.alloc());
        match fastrand::u8(0..4) {
            0 => len_a += pa.enqueue(ba.into()).map_or(0, |_| 1),
            1 => len_b += pb.enqueue(bb.into()).map_or(0, |_| 1),
            2 => {
                if let Some(ba) = pa.dequeue() {
                    dbg!(format!("A recv: {:?}", ba.into_box().as_ref()));
                    len_b -= 1;
                }
            }
            3 => {
                if let Some(bb) = pb.dequeue() {
                    dbg!(format!("B recv: {:?}", bb.into_box().as_ref()));
                    len_a -= 1;
                }
            }
            _ => unreachable!(),
        }
    }
    dbg!(format!("{}, {}", len_a, len_b));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ipc_test() {
    let s_cfg = UnixFdConf::default_mem("something", SIZE, MFdFlags::empty()).unwrap();
    let handle = init_or_load(SIZE, s_cfg);
    let handle_c = handle.clone();

    tokio::spawn(async move {
        let handle = handle_c;
        let cq = handle.server();
        for _ in 0..5 {
            let cq = cq.clone();
            tokio::spawn(async move {
                MyHandle::try_handle_ref(&cq).await;
            });
        }
    });
    tokio::spawn(async move {
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

            let handle = handle.clone();
            let t = tokio::spawn(async move {
                for i in 0..1000 {
                    let req = ShmBox::copy_from_slice(SLICE, handle.alloc());
                    match sb.try_submit(req.into()) {
                        Ok(d) => {
                            let res = d.await;
                            let buf = res.into_box();
                            for i in buf.iter() {
                                assert_eq!(*i, SLICE[0]);
                            }
                        }
                        Err(res) => {
                            println!("[submit {}]: send err: {:?}", th, res);
                            break;
                        }
                    }

                    println!("[submit {}]: recv {}", th, i);
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
    time::sleep(Duration::from_secs(2)).await;
}
