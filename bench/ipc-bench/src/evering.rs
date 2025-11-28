extern crate evering_ipc;

use core::marker::PhantomData;
use core::time::Duration;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::time::Instant;

use evering_ipc::driver::cell::IdCell;
use evering_ipc::driver::unlocked::PoolDriver;
use evering_ipc::shm::boxed::{ShmBox, ShmSlice, ShmToken};
use evering_ipc::shm::os::{
    FdBackend,
    unix::{MFdFlags, ProtFlags, AddrSpec, UnixFdConf},
};
use evering_ipc::shm::tlsf::SpinTlsf;
use evering_ipc::uring::{IReceiver, ISender, UringSpec};
use evering_ipc::{IpcAlloc, IpcHandle, IpcSpec};
use tokio::runtime::Builder;

use super::*;

use tokio::task::{spawn_local, yield_now};

type ShmReq<I> = ShmBox<[u8], IpcAlloc<I>>;
type ShmResp<I> = ShmBox<[u8], IpcAlloc<I>>;
type ShmReqT<I> = ShmToken<u8, IpcAlloc<I>, ShmSlice>;
type ShmRespT<I> = ShmToken<u8, IpcAlloc<I>, ShmSlice>;

enum Sqe<I: IpcSpec> {
    Exit,
    Ping {
        ping: i32,
        req: ShmReqT<I>,
        resp: ShmRespT<I>,
    },
}

impl<I: IpcSpec> core::fmt::Debug for Sqe<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exit => write!(f, "Exit"),
            Self::Ping { ping, req, resp } => f.debug_struct("Ping").field("ping", ping).finish(),
        }
    }
}

enum Rqe<I: IpcSpec> {
    Exited,
    Pong { pong: i32, resp: ShmRespT<I> },
}

impl<I: IpcSpec> core::fmt::Debug for Rqe<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exited => write!(f, "Exit"),
            Self::Pong { pong, resp } => f.debug_struct("Pong").field("pong", pong).finish(),
        }
    }
}

struct IpcInfo<I: IpcSpec>(PhantomData<I>);

impl<I: IpcSpec> UringSpec for IpcInfo<I> {
    type SQE = Sqe<I>;
    type CQE = Rqe<I>;
}

type MyPoolDriver<I> = PoolDriver<IpcInfo<I>>;

struct MyIpcSpec<F>(PhantomData<F>);
impl<F: AsFd> IpcSpec for MyIpcSpec<F> {
    type A = SpinTlsf;
    type S = AddrSpec;
    type M = FdBackend<F>;
}

const CAP: usize = CONCURRENCY.next_power_of_two();

type MyIpc<F> = IpcHandle<MyIpcSpec<F>, MyPoolDriver<MyIpcSpec<F>>, CAP>;

fn default_cfg(id: &str, size: usize) -> UnixFdConf<OwnedFd> {
    UnixFdConf::default_mem(id, size, MFdFlags::empty()).unwrap()
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

pub fn bench(id: &str, iters: usize, bufsize: usize) -> Duration {
    let shmid = shmid(id);
    let shmsize = shmsize(bufsize);
    let s_cfg = default_cfg(shmid.as_str(), shmsize);
    let handle = init_or_load(shmsize, s_cfg);

    let runtime = Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();

    let elapsed = std::thread::scope(|s| {
        let handle_c = handle.clone();
        let server_handle = s.spawn(|| {
            runtime.block_on(async move {
                let handle = handle_c.clone();
                let cq = handle.server();
                let respdata = resp(bufsize);

                'outer: loop {
                    let msg = match cq.recv().await {
                        Ok(m) => m,
                        Err(_) => {
                            break 'outer;
                        }
                    };

                    let (id, data) = msg.into_inner();
                    match data {
                        Sqe::Exit => {
                            cq.send(IdCell::new(id, Rqe::Exited)).await.unwrap();
                            break 'outer;
                        }
                        Sqe::Ping { ping, req, resp } => {
                            assert_eq!(ping, PING);
                            let buf = req.into_box();
                            check_req(bufsize, buf.as_ref());

                            let mut resp_box = resp.into_box();
                            resp_box.as_mut().copy_from_slice(&respdata);

                            cq.send(IdCell::new(
                                id,
                                Rqe::Pong {
                                    pong: PONG,
                                    resp: resp_box.into(),
                                },
                            ))
                            .await
                            .unwrap();
                        }
                    }
                }
            })
        });

        let client_handle = s.spawn(|| {
            runtime.block_on(async move {
                let req_data = req(bufsize);
                let handle = handle.clone();
                let (sb, rb) = handle.client();

                tokio::spawn(async move {
                    loop {
                        while !rb.try_complete() {
                            yield_now().await;
                        }
                    }
                });

                let tasks = (0..CONCURRENCY)
                    .map(|_| {
                        let req_c = req_data.clone();
                        let handle_c = handle.clone();
                        let sb_c = sb.clone();

                        tokio::spawn(async move {
                            for _ in 0..(iters / CONCURRENCY) {
                                let req_box = ShmBox::copy_from_slice(&req_c, handle_c.alloc());
                                let resp_box = unsafe {
                                    ShmBox::new_zeroed_slice_in(bufsize, handle_c.alloc())
                                        .assume_init()
                                };

                                let op = Sqe::Ping {
                                    ping: PING,
                                    req: req_box.into(),
                                    resp: resp_box.into(),
                                };

                                match sb_c.try_submit(op) {
                                    Ok(op) => match op.await {
                                        Rqe::Exited => {}
                                        Rqe::Pong {
                                            pong,
                                            resp: resp_ret_token,
                                        } => {
                                            let resp_ret = resp_ret_token.into_box();
                                            assert_eq!(pong, PONG);
                                            check_resp(bufsize, resp_ret.as_ref());
                                        }
                                    },
                                    _ => {}
                                }
                            }
                        })
                    })
                    .collect::<Vec<_>>();

                let now = Instant::now();
                for task in tasks.into_iter() {
                    task.await.unwrap();
                }
                let elapsed = now.elapsed();
                sb.try_submit(Sqe::Exit).unwrap();
                elapsed
            })
        });

        let elapsed = client_handle.join().unwrap();
        server_handle.join().unwrap();
        elapsed
    });

    elapsed
}
#[test]
fn ipc_test() {
    let elapsed = bench("test", 1000, 1024);
    println!("elapsed: {elapsed:?}");
}
