#![cfg(feature = "unix")]
#![cfg(test)]

use crate::mem::{Access, MapBuilder, MapView};
use crate::msg::Envelope;
use crate::os::FdBackend;
use crate::os::unix::{AddrSpec, UnixFd};
use crate::perlude::Session;
use crate::perlude::allocator::{MapAlloc, Optimistic};
use crate::tests::{prob, tracing_init};

type UnixMemHandle = MapView<AddrSpec, FdBackend>;
type UnixAlloc = MapAlloc<Optimistic, AddrSpec, FdBackend>;
type UnixSession<H, const N: usize> = Session<Optimistic, H, N, AddrSpec, FdBackend>;

fn mock_handle(name: &str, size: usize) -> UnixMemHandle {
    let fd = UnixFd::memfd(name, size, false).expect("should create");
    let builder = MapBuilder::fd();
    builder
        .shared(size, Access::WRITE | Access::READ, fd)
        .unwrap()
}

fn mock_alloc(name: &str, size: usize) -> UnixAlloc {
    let fd = UnixFd::memfd(name, size, false).expect("should create");
    let builder = MapBuilder::fd();
    builder
        .shared(size, Access::WRITE | Access::READ, fd)
        .unwrap()
}

fn mock_session<H: Envelope, const N: usize>(name: &str, size: usize) -> UnixSession<H, N> {
    let fd = UnixFd::memfd(name, size, false).expect("should create");
    let builder = MapBuilder::fd();
    builder
        .shared(size, Access::WRITE | Access::READ, fd)
        .unwrap()
}

#[test]
fn area_init() {
    const SIZE: usize = 2000;
    const NAME: &str = "area";

    tracing_init();

    let area = mock_handle(NAME, SIZE);
    tracing::debug!(
        "area header: {:?}, {:?}",
        area.header(),
        area.header().status()
    );
    tracing::debug!("area: {:?}", area);
    let area2 = area.clone();
    tracing::debug!("area header: {:?}", area2);
}

#[test]
fn arena_alloc() {
    use std::sync::Barrier;
    use std::thread;

    use crate::mem::MemAlloc;

    const BYTES_SIZE: usize = 4 << 20;
    const ALLOC_NUM: usize = 200;
    const NUM: usize = 5;
    const NAME: &str = "alloc";
    const SIZE: usize = {
        (BYTES_SIZE * ALLOC_NUM).next_power_of_two()
    };

    tracing_init();
    let a = mock_alloc(NAME, SIZE);

    let bar = Barrier::new(NUM);
    let mut metas: Vec<_> = (0..ALLOC_NUM)
        .map(|_| a.malloc_bytes(BYTES_SIZE).unwrap())
        .collect();
    thread::scope(|s| {
        for i in 0..NUM {
            let a_ref = &a;
            let b_ref = &bar;
            let start = 0;
            let end = if i == NUM - 1 {
                metas.len()
            } else {
                ALLOC_NUM / NUM
            };

            let chunk: Vec<_> = metas.drain(start..end).collect();
            s.spawn(move || {
                b_ref.wait();
                for meta in chunk {
                    tracing::debug!("{:?}", meta);
                    a_ref.dealloc(meta);
                }
            });
        }
    });
}

#[test]
fn token_of_pbox() {
    use std::sync::Barrier;
    use std::thread;

    use crate::boxed::PBox;

    #[derive(Debug)]
    struct Recover {
        f1: u64,
        f2: char,
    }

    impl Recover {
        fn rand() -> Self {
            Self {
                f1: fastrand::u64(0..100),
                f2: fastrand::char('a'..'z'),
            }
        }
    }

    const ALLOC_NUM: usize = 100;
    const NUM: usize = 5;

    const NAME: &str = "alloc";
    const SIZE: usize = 20000;

    tracing_init();

    let a = mock_alloc(NAME, SIZE);

    let bar = Barrier::new(NUM);
    thread::scope(|s| {
        let handles = (0..NUM)
            .map(|_| {
                let a_ref = &a;
                let b_ref = &bar;

                s.spawn(move || {
                    b_ref.wait();
                    (0..ALLOC_NUM)
                        .map(move |_| {
                            let recover = PBox::new_in(Recover::rand(), &a_ref);
                            recover.token_of()
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();

        let tokens: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        let _: Vec<_> = tokens
            .into_iter()
            .map(|chunk| {
                let a_ref = &a;
                let b_ref = &bar;

                s.spawn(move || {
                    b_ref.wait();
                    chunk.into_iter().for_each(|token| {
                        let recover = token.detoken(&a_ref);
                        tracing::debug!("{:?}", recover)
                    })
                })
            })
            .collect();
    });
}

#[test]
fn token_of_slice() {
    use std::sync::Barrier;
    use std::thread;

    use crate::boxed::PBox;
    use crate::mem::MemAllocator;

    const ALLOC_NUM: usize = 100;
    const NUM: usize = 5;

    const NAME: &str = "alloc";
    const SIZE: usize = 50000;

    tracing_init();

    fn rand_slice<A: MemAllocator>(a: A) -> PBox<[u8], A> {
        let len = fastrand::usize(1..128);
        PBox::new_slice_in(len, |_| fastrand::u8(0..128), a)
    }

    let a = mock_alloc(NAME, SIZE);

    let bar = Barrier::new(NUM);
    thread::scope(|s| {
        let handles = (0..NUM)
            .map(|_| {
                let a_ref = &a;
                let b_ref = &bar;

                s.spawn(move || {
                    b_ref.wait();
                    (0..ALLOC_NUM)
                        .map(move |_| {
                            let slice = rand_slice(&a_ref);
                            slice.token_of()
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();

        let tokens: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        let _: Vec<_> = tokens
            .into_iter()
            .map(|chunk| {
                let a_ref = &a;
                let b_ref = &bar;

                s.spawn(move || {
                    b_ref.wait();
                    chunk.into_iter().for_each(|token| {
                        let slice = token.detoken(&a_ref);
                        tracing::debug!("{:?}", slice)
                    })
                })
            })
            .collect();
    });
}

#[test]
fn token_slice() {
    use std::sync::Barrier;
    use std::thread;

    use super::Byte;
    use crate::mem::MemAllocator;
    use crate::msg::MoveMessage;
    use crate::token::AllocToken;

    const ALLOC_NUM: usize = 100;
    const NUM: usize = 5;

    const NAME: &str = "alloc";
    const SIZE: usize = 50000;

    tracing_init();

    fn rand_slice_token<A: MemAllocator>(a: A) -> (AllocToken<A>, A) {
        Byte::slice_token(fastrand::usize(1..128), |_| Byte::mock(), a)
    }

    let a = mock_alloc(NAME, SIZE);

    let bar = Barrier::new(NUM);
    thread::scope(|s| {
        let handles = (0..NUM)
            .map(|_| {
                let a_ref = &a;
                let b_ref = &bar;

                s.spawn(move || {
                    b_ref.wait();
                    (0..ALLOC_NUM)
                        .map(move |_| {
                            let (token, _) = rand_slice_token(&a_ref);
                            token
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();

        let tokens: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        let _: Vec<_> = tokens
            .into_iter()
            .map(|chunk| {
                let a_ref = &a;
                let b_ref = &bar;

                s.spawn(move || {
                    b_ref.wait();
                    chunk.into_iter().for_each(|token| {
                        let slice = Byte::slice_detoken(token, &a_ref).expect("should detoken");
                        tracing::debug!("{:?}", slice)
                    })
                })
            })
            .collect();
    });
}

#[tokio::test]
async fn conn_async() {
    use super::Byte;
    use crate::msg::MoveMessage;
    use crate::perlude::allocator::MemAllocator;
    use crate::perlude::channel::{
        CachePool, MsgCompleter, MsgReceiver, MsgSender, MsgSubmitter, MsgToken, ReqNull, Token,
        TryRecvError, TrySendError, TrySubmitError,
    };

    const N: usize = 1;
    const QSIZE: usize = 256;
    const FUZZ_PROB: f32 = 0.01;

    const NAME: &str = "alloc";
    const SIZE: usize = 60000;

    fn rand_slice_token<A: MemAllocator>(a: A) -> (Token, A) {
        Byte::slice_token(fastrand::usize(1..128), |_| Byte::mock(), a)
    }

    async fn client_task<const N: usize, S: MsgSubmitter<(), N>, A: MemAllocator>(
        submitter: S,
        alloc: A,
    ) {
        loop {
            if prob(FUZZ_PROB) {
                submitter.close();
                break;
            }

            let (msg, _) = rand_slice_token(&alloc);
            let token = msg.pack_default();

            let op = match submitter.try_submit(token) {
                Ok(op) => op,
                Err(TrySubmitError::SendError(TrySendError::Full(_))) => {
                    tokio::task::yield_now().await;
                    continue;
                }
                Err(TrySubmitError::SendError(TrySendError::Disconnected)) => break,
                Err(TrySubmitError::CacheFull) => {
                    tokio::task::yield_now().await;
                    continue;
                }
            };

            let (res, _) = op.await.into_parts();
            let info = Byte::slice_detoken(res, &alloc).unwrap();
            tracing::debug!("[Client] receive: {:?}", info);
        }
    }

    async fn completer_task<const N: usize, C: MsgCompleter<(), N>>(completer: C) {
        loop {
            match completer.complete() {
                Ok(_) => continue,
                Err(TryRecvError::Empty) => {
                    tokio::task::yield_now().await;
                    continue;
                }
                Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    async fn server_task<
        S: MsgSender<ReqNull>,
        R: MsgReceiver<ReqNull>,
        F: FnMut(Token) -> Option<Token>,
    >(
        sender: S,
        receiver: R,
        mut handler: F,
    ) {
        loop {
            if prob(FUZZ_PROB) {
                tokio::task::yield_now().await;
            }

            let packet = match receiver.try_recv() {
                Ok(p) => p,
                Err(TryRecvError::Empty) => {
                    tokio::task::yield_now().await;
                    continue;
                }
                Err(TryRecvError::Disconnected) => {
                    sender.close();
                    break;
                }
            };

            let (token, header) = packet.into_parts();
            match handler(token) {
                None => {
                    sender.close();
                    break;
                }
                Some(reply) => {
                    let packed = reply.pack(header);
                    match sender.try_send(packed) {
                        Ok(_) => continue,
                        Err(TrySendError::Full(_)) => {
                            tokio::task::yield_now().await;
                            continue;
                        }
                        Err(TrySendError::Disconnected) => break,
                    }
                }
            }
        }
    }

    tracing_init();

    let conn = mock_session::<ReqNull, N>(NAME, SIZE);
    let handle = conn.prepare(QSIZE).expect("alloc ok");
    let view = conn.acquire(handle).expect("view ok");

    let (ls, lr) = view.clone().lsplit();
    let (rs, rr) = view.clone().rsplit();

    let (ls, lr) = CachePool::<(), QSIZE>::new().bind(ls, lr);

    let alloc = conn.alloc.clone();
    let handler = move |token: Token| {
        let info = Byte::slice_detoken(token, &alloc).expect("should work");
        tracing::debug!("[Server] receive: {:?}", info);
        let (new, _) = rand_slice_token(&alloc);
        Some(new)
    };

    let server = tokio::spawn(server_task(rs, rr, move |token| handler(token)));

    let alloc = conn.alloc.clone();
    let client = tokio::spawn(client_task::<QSIZE, _, _>(ls, alloc));
    let completer = tokio::spawn(completer_task::<QSIZE, _>(lr));

    let _ = tokio::join!(server, client, completer);
}
