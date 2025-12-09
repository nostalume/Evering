// use core::marker::PhantomData;
// use core::time::Duration;
// use std::os::fd::{AsFd, OwnedFd};
// use std::sync::Arc;
// use std::time::Instant;

use core::time::Duration;
use std::time::Instant;

use bytes::Bytes;
use evering::{
    msg::{self, Envelope, Message, Move, MoveMessage, TypeTag, type_id},
    os::{
        FdBackend,
        unix::{AddrSpec, UnixFd},
    },
    perlude::{
        Session,
        allocator::{Access, MapBuilder, Optimistic, Pessimistic},
        channel::{
            CachePool, Completer, MsgCompleter, MsgSubmitter, QueueChannel, ReqId, Submitter,
            TryRecvError, TrySendError, TrySubmitError,
        },
    },
};
use tokio::runtime::Builder;

use crate::{CONCURRENCY, check_req, check_resp, req, resp, shmid, shmsize};

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
    pub fn as_slice(s: &[Byte]) -> &[u8] {
        unsafe { core::slice::from_raw_parts(s.as_ptr() as *const u8, s.len()) }
    }
}

#[inline]
pub fn as_slice(bytes: &Bytes) -> &[Byte] {
    unsafe { core::slice::from_raw_parts(bytes.as_ptr() as *const Byte, bytes.len()) }
}

impl TypeTag for Byte {
    const TYPE_ID: msg::TypeId = type_id::type_id("Info");
}

impl Message for Byte {
    type Semantics = Move;
}

const CAP: usize = CONCURRENCY.next_power_of_two();

type UnixSession<H, const N: usize> = Session<Optimistic, H, N, AddrSpec, FdBackend>;

fn mock_session<H: Envelope, const N: usize>(name: &str, size: usize) -> UnixSession<H, N> {
    let fd = UnixFd::memfd(name, size, false).expect("should create");
    let builder = MapBuilder::fd();
    builder
        .shared(size, Access::WRITE | Access::READ, fd)
        .unwrap()
}

pub fn bench(id: &str, iters: usize, bufsize: usize) -> Duration {
    let shmid = shmid(id);
    let shmsize = shmsize(bufsize);
    let handle = mock_session::<ReqId<()>, 1>(&shmid, shmsize);

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

    let elapsed = std::thread::scope(|s| {
        let salloc = handle.alloc.clone();
        let server = s.spawn(|| {
            runtime.block_on(async move {
                // let handle = shandle;
                // let cq = handle.server();
                let respdata = resp(bufsize);
                let alloc = salloc;
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

                    let (req, header) = packet.into_parts();
                    let req = Byte::slice_detoken(req, &alloc).expect("should detoken");
                    check_req(bufsize, Byte::as_slice(&req));
                    drop(req);

                    let (resp, alloc) = Byte::copied_slice_token(as_slice(&respdata), &alloc);

                    let resp = resp.pack(header);
                    match rs.try_send(resp) {
                        Ok(_) => continue,
                        Err(TrySendError::Full(_)) => {
                            tokio::task::yield_now().await;
                            continue;
                        }
                        Err(TrySendError::Disconnected) => break,
                    }
                }
            })
        });

        let calloc = handle.alloc.clone();
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
                        let alloc = calloc.clone();
                        let ls = ls.clone();

                        tokio::spawn(async move {
                            for _ in 0..(iters / CONCURRENCY) {
                                let (req, alloc) =
                                    Byte::copied_slice_token(as_slice(&req_data), &alloc);
                                let req = req.pack_default();

                                let op = match ls.try_submit(req) {
                                    Ok(op) => op,
                                    Err(TrySubmitError::SendError(TrySendError::Full(_))) => {
                                        tokio::task::yield_now().await;
                                        continue;
                                    }
                                    Err(TrySubmitError::SendError(TrySendError::Disconnected)) => {
                                        break;
                                    }
                                    Err(TrySubmitError::CacheFull) => {
                                        tokio::task::yield_now().await;
                                        continue;
                                    }
                                };
                                let (resp, _) = op.await.into_parts();
                                let resp =
                                    Byte::slice_detoken(resp, alloc).expect("should detoken");

                                check_resp(bufsize, Byte::as_slice(&resp));
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
    });

    elapsed
}

#[cfg(test)]
mod tests {
    #[test]
    fn ipc_test() {
        let elapsed = super::bench("test", 30000, 4096);
        println!("elapsed: {elapsed:?}");
    }
}
