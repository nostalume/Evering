// use core::marker::PhantomData;
// use core::time::Duration;
// use std::os::fd::{AsFd, OwnedFd};
// use std::sync::Arc;
// use std::time::Instant;

use core::time::Duration;
use std::sync::Arc;
use std::time::Instant;

use evering::{
    RegionId, Repr, Request,
    os::unix::UnixFd,
    perlude::talc::{
        Access, Session, SessionBy,
        channel::{
            CachePool, Completer, QueueChannel, ReqId, SubmitCause, Submitter, TryRecvError,
            TrySendError, TrySubmitError,
        },
    },
};
use tokio::runtime::Builder;

use crate::{CONCURRENCY, check_req, check_resp, req, resp, shmid, shmsize};

const CAP: usize = CONCURRENCY.next_power_of_two();

type UnixSession<H> = Session<H>;

fn mock_session<H: Repr>(name: &str, size: usize) -> UnixSession<H> {
    let fd = UnixFd::memfd(name, size, false).expect("should create");
    SessionBy::<H>::create(
        fd,
        Request::new(size, Access::WRITE | Access::READ),
        RegionId::new(0x4556_4552_4245_4e43, size as u64),
    )
    .unwrap()
}

pub fn bench(id: &str, iters: usize, bufsize: usize) -> Duration {
    let shmid = shmid(id);
    let shmsize = shmsize(bufsize);
    let handle = Arc::new(mock_session::<ReqId<()>>(&shmid, shmsize));

    let runtime = Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();

    #[cfg(feature = "tracing")]
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .try_init();

    let id = handle.prepare(CAP).expect("alloc ok");
    let view = handle.acquire(id).expect("view ok");

    let (ls, lr) = view.clone().lsplit();
    let (rs, rr) = view.clone().rsplit();

    let (ls, lr) = CachePool::<(), CAP>::new().bind(ls, lr);

    std::thread::scope(|s| {
        let shandle = handle.clone();
        let runtime = &runtime;
        let server = s.spawn(move || {
            runtime.block_on(async move {
                let respdata = resp(bufsize);
                let heap = shandle.heap();
                loop {
                    let packet = match rr.try_recv() {
                        Ok(p) => p,
                        Err(TryRecvError::Empty) => {
                            tokio::task::yield_now().await;
                            continue;
                        }
                        Err(TryRecvError::Disconnected) => {
                            rs.close();
                            break;
                        }
                    };

                    #[cfg(feature = "tracing")]
                    tracing::debug!("[Server]: received");

                    let (header, req) = heap.open::<ReqId<()>, [u8]>(packet).expect("should open");
                    check_req(bufsize, &req);
                    drop(req);

                    let mut resp = heap
                        .copy(&respdata)
                        .expect("allocate response")
                        .pack(header);
                    loop {
                        match rs.try_send(resp) {
                            Ok(_) => break,
                            Err(TrySendError::Full(returned)) => {
                                resp = returned;
                                tokio::task::yield_now().await;
                            }
                            Err(TrySendError::Disconnected(returned)) => {
                                drop(
                                    heap.open::<ReqId<()>, [u8]>(returned)
                                        .expect("reclaim rejected response")
                                        .1,
                                );
                                return;
                            }
                        }
                    }
                }
            })
        });

        let runtime = &runtime;
        let client = s.spawn(|| {
            let lsfinal = ls.clone();
            runtime.block_on(async move {
                tokio::spawn(async move {
                    loop {
                        match lr.complete() {
                            Ok(_) => continue,
                            Err(TryRecvError::Empty) => {
                                tokio::task::yield_now().await;
                                continue;
                            }
                            Err(TryRecvError::Disconnected) => break,
                        }
                    }
                });

                let tasks = (0..CONCURRENCY)
                    .map(|_| {
                        let req_data = req(bufsize);
                        let handle = handle.clone();
                        let ls = ls.clone();

                        tokio::spawn(async move {
                            let heap = handle.heap();
                            for _ in 0..(iters / CONCURRENCY) {
                                let mut req =
                                    heap.copy(&req_data).expect("allocate request").pack(());

                                let op = loop {
                                    match ls.try_submit(req) {
                                        Ok(op) => break Some(op),
                                        Err(TrySubmitError::SendRejected {
                                            cause: SubmitCause::Full,
                                            item: returned,
                                        })
                                        | Err(TrySubmitError::CacheFull(returned)) => {
                                            req = returned;
                                            tokio::task::yield_now().await;
                                        }
                                        Err(TrySubmitError::SendRejected {
                                            cause: SubmitCause::Disconnected,
                                            item: returned,
                                        }) => {
                                            drop(
                                                heap.open::<(), [u8]>(returned)
                                                    .expect("reclaim rejected request")
                                                    .1,
                                            );
                                            break None;
                                        }
                                    }
                                };
                                let Some(op) = op else { break };
                                let resp = heap.open::<(), [u8]>(op.await).expect("should open").1;

                                check_resp(bufsize, &resp);
                                drop(resp);
                            }
                        })
                    })
                    .collect::<Vec<_>>();

                let now = Instant::now();
                for task in tasks.into_iter() {
                    task.await.unwrap();
                }
                let elapsed = now.elapsed();
                lsfinal.close();
                elapsed
            })
        });

        let elapsed = client.join().unwrap();
        server.join().unwrap();
        elapsed
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn ipc_test() {
        let elapsed = super::bench("test", 30000, 4096);
        println!("elapsed: {elapsed:?}");
    }
}
