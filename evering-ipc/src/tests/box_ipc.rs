#![cfg(test)]
#![cfg(feature = "unix")]

use core::marker::PhantomData;
use core::time::Duration;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::time::Instant;

use crate::driver::cell::IdCell;
use crate::driver::unlocked::PoolDriver;
use crate::shm::boxed::{ShmBox, ShmSlice, ShmToken};
use crate::shm::os::{
    FdBackend,
    unix::{MFdFlags, ProtFlags, UnixFdConf, UnixShm},
};
use crate::shm::tlsf::SpinTlsf;
use crate::tests::*;
use crate::uring::{IReceiver, ISender, UringSpec};
use crate::{IpcAlloc, IpcHandle, IpcSpec};

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
    type S = UnixShm;
    type M = FdBackend<F>;
}

const CONCURRENCY: usize = 200;

const CAP: usize = CONCURRENCY.next_power_of_two();

type MyIpc<F> = IpcHandle<MyIpcSpec<F>, MyPoolDriver<MyIpcSpec<F>>, CAP>;

fn default_cfg(id: &str, size: usize) -> UnixFdConf<OwnedFd> {
    UnixFdConf::default_from_mem_fd(id, size, MFdFlags::empty()).unwrap()
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
    let c_cfg = s_cfg.clone();

    let elapsed = std::thread::scope(|s| {
        let server_handle = s.spawn(|| {
            let handle = init_or_load(shmsize, s_cfg);
            let cq = handle.server();
            cur_block_on(async move {
                let respdata = resp(bufsize);
                'outer: loop {
                    // It's good practice to yield if try_recv repeatedly fails to avoid busy-waiting
                    if cq.try_recv().is_err() {
                        tokio::task::yield_now().await;
                    }

                    while let Ok(msg) = cq.try_recv() {
                        let (id, data) = msg.into_inner();
                        match data {
                            Sqe::Exit => {
                                cq.send(IdCell::new(id, Rqe::Exited)).await.unwrap();
                                break 'outer; // Break from the 'outer loop as intended
                            }
                            Sqe::Ping { ping, req, resp } => {
                                assert_eq!(ping, PING);
                                println!("server: ping: {}", ping); // Added dbg for server side
                                let buf: ShmBox<_, _> = req.into();
                                check_req(bufsize, buf.as_ref());
                                let mut resp = resp.into_box();
                                resp.as_mut().copy_from_slice(&respdata);
                                println!("server resp");
                                cq.send(IdCell::new(
                                    id,
                                    Rqe::Pong {
                                        pong: PONG,
                                        resp: resp.into(),
                                    },
                                ))
                                .await
                                .unwrap();
                            }
                        }
                    }
                }
            })
        });

        let client_handle = s.spawn(|| {
            let req_data = req(bufsize);
            let handle_arc = init_or_load(shmsize, c_cfg);
            let (sb, rb) = handle_arc.client();

            std::thread::spawn(move || {
                loop {
                    rb.try_complete();
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            });

            let client_elapsed = cur_block_on(async move {
                // let rb = rb.clone();
                // tokio::task::spawn_local(async move {
                //     loop {
                //         rb.try_complete();
                //         tokio::time::sleep(Duration::from_millis(100)).await;
                //     }
                // });

                let tasks = (0..CONCURRENCY)
                    .map(|_| {
                        let req_c = req_data.clone();
                        let handle_c = handle_arc.clone();
                        let sb_c = sb.clone();

                        async move {
                            for _ in 0..(iters / CONCURRENCY) {
                                let req_box = ShmBox::copy_from_slice(&req_c, handle_c.alloc());
                                let resp_box: ShmBox<[u8], IpcAlloc<MyIpcSpec<OwnedFd>>> = unsafe {
                                    ShmBox::new_zeroed_slice_in(bufsize, handle_c.alloc())
                                        .assume_init()
                                };

                                let op = Sqe::Ping {
                                    ping: PING,
                                    req: req_box.into(),
                                    resp: resp_box.into(),
                                };

                                let Rqe::Pong {
                                    pong,
                                    resp: resp_ret_token,
                                } = sb_c.submit(op).await.unwrap().await
                                else {
                                    unreachable!()
                                };
                                println!("pong: {}", pong);

                                let resp_ret = resp_ret_token.into_box();
                                assert_eq!(pong, PONG);
                                check_resp(bufsize, resp_ret.as_ref());
                            }
                        }
                    })
                    .map(spawn_local); // Assuming `spawn_local` is correctly defined for local tasks.
                println!("finish task collect: Starting ping-pong tasks...");
                let now = Instant::now();
                for task in tasks {
                    task.await.unwrap();
                }
                let elapsed = now.elapsed();
                // Send the exit message AFTER all ping-pong tasks are complete
                sb.try_submit(Sqe::Exit).unwrap();
                elapsed
            });
            client_elapsed // Return the calculated elapsed duration
        });

        let elapsed = client_handle.join().unwrap();
        server_handle.join().unwrap();
        elapsed
    });

    elapsed
}

#[test]
fn ipc_test() {
    let elapsed = bench("test", 200, 1024);
    println!("elapsed: {elapsed:?}");
}
