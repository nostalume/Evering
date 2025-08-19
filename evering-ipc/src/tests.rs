#![cfg(test)]

use core::{hint::black_box, time::Duration};

use bytes::Bytes;

mod char_ipc;
mod box_ipc;

const CONCURRENCY: usize = 200;

// Fixed constants
const PING: i32 = 1;
const PONG: i32 = 2;

static PONGDATA: &[u8] = PONG.to_be_bytes().as_slice();
static PINGDATA: &[u8] = PING.to_be_bytes().as_slice();

type BenchFn = fn(&str, usize, usize) -> Duration;

const fn shmsize(bufsize: usize) -> usize {
    if bufsize < (1 << 20) {
        (1 << 8) << 20
    } else if bufsize < (4 << 20) {
        1 << 30
    } else {
        2 << 30
    }
}

fn shmid(pref: &str) -> String {
    pref.chars()
        .chain(std::iter::repeat_with(fastrand::alphanumeric).take(6))
        .collect()
}

const REQ: u8 = b'S';
const RESP: u8 = b'R';

fn check_buf(bufsize: usize, resp: &[u8], expected: u8) {
    assert_eq!(resp.len(), bufsize);
    // Pick a few bytes to check. Checking all bytes is meaningless and will
    // significantly slow down the benchmark.
    for _ in 0..(32.min(bufsize)) {
        let b = *fastrand::choice(resp).unwrap();
        assert_eq!(black_box(b), black_box(expected));
    }
}

/// Returns arbitrary response data.
fn make_buf(bufsize: usize, expected: u8) -> Bytes {
    // Black boxed to mock runtime values
    black_box(Bytes::from(vec![black_box(expected); bufsize]))
}

fn check_req(bufsize: usize, req: &[u8]) {
    check_buf(bufsize, req, REQ);
}

fn req(bufsize: usize) -> Bytes {
    make_buf(bufsize, REQ)
}

fn resp(bufsize: usize) -> Bytes {
    make_buf(bufsize, RESP)
}

fn check_resp(bufsize: usize, resp: &[u8]) {
    check_buf(bufsize, resp, RESP);
}

fn cur_block_on<T>(fut: impl Future<Output = T>) -> T {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&rt, fut)
}
