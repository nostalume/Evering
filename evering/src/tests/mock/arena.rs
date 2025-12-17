use crate::msg::Envelope;
use crate::perlude::allocator::{MapAlloc, Optimistic};
use crate::perlude::{Session, SessionBy};

use crate::tests::mock::{MAX_ADDR, MockAddr, MockBackend};
use crate::tests::{self, prob, tracing_init};

type MockAlloc<'a> = MapAlloc<Optimistic, MockAddr, MockBackend<'a>>;
type MockSession<'a, H, const N: usize> = Session<Optimistic, H, N, MockAddr, MockBackend<'a>>;

fn mock_alloc(bk: &mut [u8], start: usize, size: usize) -> MockAlloc<'_> {
    let bk = MockBackend(bk);
    bk.shared(start, size).try_into().unwrap()
}

fn mock_session<H: Envelope, const N: usize>(
    bk: &mut [u8],
    start: usize,
    size: usize,
) -> MockSession<'_, H, N> {
    let bk = MockBackend(bk);
    SessionBy::from(bk.shared(start, size)).unwrap()
}

macro_rules! mock_alloc_test {
    ($body:expr) => {{
        let mut pt = [0; MAX_ADDR];
        let a = mock_alloc(&mut pt, 0, MAX_ADDR);
        $body(a)
    }};
}

#[test]
fn alloc_tests() {
    mock_alloc_test! {|a| {
        tests::alloc_exceed::<50,20>(a);
    }}
    mock_alloc_test! {|a| {
        tests::alloc_frag::<4, 1000, 10>(a);
    }}
    mock_alloc_test! {|a| {
        tests::alloc_dealloc::<8, 1000, 5>(a);
    }}
    mock_alloc_test! {|a| {
        tests::pbox_droppy::<5, 1000>(a);
    }}
    mock_alloc_test! {|a| {
        tests::pbox_rand::<50, 10>(a);
    }}
    mock_alloc_test! {|a| {
        tests::parc_stress::<50, 10>(a);
    }}
    mock_alloc_test! {|a| {
        tests::pbox_token::<500, 10>(a);
    }}
}

#[test]
fn conn_sync() {
    use std::thread;

    use crate::msg::MoveMessage;
    use crate::perlude::channel::{MsgReceiver, MsgSender, Token, TryRecvError};
    use crate::tests::Info;

    const N: usize = 1;
    const SIZE: usize = 256;
    const FUZZ_PROB: f32 = 0.01;

    fn stress<S: MsgSender<()>, R: MsgReceiver<()>, F: FnMut(Token) -> Option<Token>>(
        s: S,
        r: R,
        mut handler: F,
    ) {
        let mut alive = true;

        while alive {
            if !prob(FUZZ_PROB) {
                thread::yield_now();
            }

            let p = match r.try_recv() {
                Ok(p) => p,
                Err(e) => match e {
                    TryRecvError::Empty => continue,
                    TryRecvError::Disconnected => break,
                },
            };

            let (t, _) = p.into_parts();
            // assert!(h == Exit::None, "header corrupted");

            match handler(t) {
                None => {
                    // let exit = Token::empty().pack(Exit::Exit);
                    // let _ = s.try_send(exit);
                    s.close();
                    alive = false;
                }
                Some(reply) => {
                    let pack = reply.pack_default();
                    let _ = s.try_send(pack);
                }
            }
        }
    }

    tracing_init();

    let mut pt = [0; MAX_ADDR];
    let conn = mock_session::<(), N>(&mut pt, 0, MAX_ADDR);
    let alloc = conn.alloc.clone();

    let h = conn.prepare(SIZE).expect("alloc ok");
    let q = conn.acquire(h).expect("view ok");

    let (ls, lr) = q.clone().lsplit();
    let (rs, rr) = q.clone().rsplit();

    let (msg, alloc) = Info::mock().token(alloc);
    let _ = ls.try_send(msg.pack_default());

    let handler = |token: Token, label: &'static str| {
        if prob(FUZZ_PROB) {
            None
        } else {
            let info = Info::detoken(token, &alloc).expect("should work");
            tracing::debug!("[{}] receive: {:?}", label, info);
            let (new, _) = Info::mock().token(&alloc);
            Some(new)
        }
    };
    thread::scope(|s| {
        s.spawn(|| stress(ls, lr, |token| handler(token, "Left")));

        s.spawn(|| {
            stress(rs, rr, |token| handler(token, "right"));
        });
    });
}

#[tokio::test]
async fn conn_async() {
    use crate::msg::MoveMessage;
    use crate::perlude::allocator::MemAllocator;
    use crate::perlude::channel::{
        CachePool, MsgCompleter, MsgReceiver, MsgSender, MsgSubmitter, ReqNull, Token,
        TryRecvError, TrySendError, TrySubmitError,
    };
    use crate::tests::Info;

    const N: usize = 1;
    const SIZE: usize = 256;
    const FUZZ_PROB: f32 = 0.01;

    async fn client_task<const N: usize, S: MsgSubmitter<(), N>, A: MemAllocator>(
        submitter: S,
        alloc: A,
    ) {
        loop {
            if prob(FUZZ_PROB) {
                submitter.close();
                break;
            }

            let (msg, _) = Info::mock().token(&alloc);
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
            let info = Info::detoken(res, &alloc).unwrap();
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

    let mut pt = [0; MAX_ADDR];
    let conn = mock_session::<ReqNull, N>(&mut pt, 0, MAX_ADDR);
    let handle = conn.prepare(SIZE).expect("alloc ok");
    let view = conn.acquire(handle).expect("view ok");

    let (ls, lr) = view.clone().lsplit();
    let (rs, rr) = view.clone().rsplit();

    let (ls, lr) = CachePool::<(), SIZE>::new().bind(ls, lr);

    let alloc = conn.alloc.clone();
    let handler = move |token: Token| {
        let info = Info::detoken(token, &alloc).expect("should work");
        tracing::debug!("[Server] receive: {:?}", info);
        let (new, _) = Info::mock().token(&alloc);
        Some(new)
    };

    let server = server_task(rs, rr, move |token| handler(token));

    let alloc = conn.alloc.clone();
    let client = client_task::<SIZE, _, _>(ls, alloc);
    let completer = completer_task::<SIZE, _>(lr);

    let _ = tokio::join!(server, client, completer);
}
