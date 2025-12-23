use crate::msg::{Envelope, MoveMsg};
use crate::perlude::arena::{MapAlloc, Optimistic,Session,SessionBy};

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
        tests::alloc_lines::<8, 1000, 5>(a);
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

    use crate::perlude::arena::channel::Token;
    use crate::tests::Info;

    const N: usize = 1;
    const SIZE: usize = 256;
    const FUZZ_PROB: f32 = 0.01;

    tracing_init();

    let mut pt = [0; MAX_ADDR];
    let conn = mock_session::<(), N>(&mut pt, 0, MAX_ADDR);
    let alloc = conn.alloc.clone();

    let h = conn.prepare(SIZE).expect("alloc ok");
    let q = conn.acquire(h).expect("view ok");

    let (ls, lr) = q.clone().lsplit();
    let (rs, rr) = q.clone().rsplit();

    let (msg, alloc) = MoveMsg::new(Info::mock(), alloc);
    let _ = ls.try_send(msg.with_default());

    let handler = |token: Token, label: &'static str| {
        if prob(FUZZ_PROB) {
            None
        } else {
            let info = MoveMsg::<Info>::detoken(token, &alloc).expect("should work");
            tracing::debug!("[{}] receive: {:?}", label, info);
            let (new, _) = MoveMsg::new(Info::mock(), &alloc);
            Some(new)
        }
    };

    thread::scope(|s| {
        s.spawn(|| tests::stress_conn(ls, lr, FUZZ_PROB, |token| handler(token, "Left")));

        s.spawn(|| {
            tests::stress_conn(rs, rr, FUZZ_PROB, |token| handler(token, "right"));
        });
    });
}

#[tokio::test]
async fn conn_async() {
    use crate::perlude::arena::channel::{CachePool, ReqNull, Token};
    use crate::tests::Info;

    const N: usize = 1;
    const SIZE: usize = 256;
    const FUZZ_PROB: f32 = 0.0001;

    tracing_init();

    let mut pt = [0; MAX_ADDR];
    let conn = mock_session::<ReqNull, N>(&mut pt, 0, MAX_ADDR);
    let handle = conn.prepare(SIZE).expect("alloc ok");
    let view = conn.acquire(handle).expect("view ok");

    let (ls, lr) = view.clone().lsplit();
    let (rs, rr) = view.clone().rsplit();

    let (ls, lr) = CachePool::<(), SIZE>::new().bind(ls, lr);

    let alloc = conn.alloc.clone();
    let client_token = move || {
        let (msg, _) = MoveMsg::new(Info::mock(), &alloc);
        let token = msg.with_default();
        token
    };
    let alloc = conn.alloc.clone();
    let client_handler = move |token| {
        let info = MoveMsg::<Info>::detoken(token, &alloc).unwrap();
        tracing::debug!("[Client] receive: {:?}", info);
    };

    let alloc = conn.alloc.clone();
    let server_handler = move |token: Token| {
        let info = MoveMsg::<Info>::detoken(token, &alloc).expect("should work");
        tracing::debug!("[Server] receive: {:?}", info);
        let (new, _) = MoveMsg::new(Info::mock(), &alloc);
        Some(new)
    };

    let server = tests::server_conn(rs, rr, server_handler, FUZZ_PROB);
    let client = tests::client_conn(ls, client_token, client_handler, FUZZ_PROB);
    let completer = tests::complete_conn(lr);

    let _ = tokio::join!(server, client, completer);
}
