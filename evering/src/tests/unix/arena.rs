use crate::msg::{Envelope, MoveMsg};
use crate::os::FdBackend;
use crate::os::unix::{AddrSpec, UnixFd};

use crate::perlude::arena::{Access, MapAlloc, MapBuilder, Optimistic, Session};
use crate::tests::{self, Info, tracing_init};

type UnixAlloc = MapAlloc<Optimistic, AddrSpec, FdBackend>;
type UnixSession<H, const N: usize> = Session<Optimistic, H, N, AddrSpec, FdBackend>;

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
fn arena_lines() {
    // 2 kb
    const BYTES_SIZE: usize = 2 << 10;
    const ALLOC_NUM: usize = 200;
    const NUM: usize = 5;

    const NAME: &str = "alloc";
    const SIZE: usize = (BYTES_SIZE * ALLOC_NUM).next_power_of_two();

    let a = mock_alloc(NAME, SIZE);

    tests::alloc_lines::<BYTES_SIZE, ALLOC_NUM, NUM>(a);
}

#[test]
fn pbox_token() {
    const ALLOC_NUM: usize = 100;
    const NUM: usize = 5;

    const NAME: &str = "alloc";
    const SIZE: usize = 20000;

    let a = mock_alloc(NAME, SIZE);
    tests::pbox_token::<ALLOC_NUM, NUM>(a);
}

#[test]
fn pbox_rand() {
    const ALLOC_NUM: usize = 100;
    const NUM: usize = 5;

    const NAME: &str = "alloc";
    const SIZE: usize = 80000;

    let a = mock_alloc(NAME, SIZE);
    tests::pbox_rand::<ALLOC_NUM, NUM>(a)
}

#[tokio::test]
async fn conn_async() {
    use crate::perlude::arena::channel::{CachePool, ReqNull, Token};

    const N: usize = 1;
    const QSIZE: usize = 256;
    const FUZZ_PROB: f32 = 0.001;

    const NAME: &str = "alloc";
    const SIZE: usize = 60000;

    tracing_init();

    let conn = mock_session::<ReqNull, N>(NAME, SIZE);
    let handle = conn.prepare(QSIZE).expect("alloc ok");
    let view = conn.acquire(handle).expect("view ok");

    let (ls, lr) = view.clone().lsplit();
    let (rs, rr) = view.clone().rsplit();

    let (ls, lr) = CachePool::<(), QSIZE>::new().bind(ls, lr);

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
