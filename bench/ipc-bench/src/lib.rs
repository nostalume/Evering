mod evering;
mod monoio;
mod shmipc;
mod utils;
use std::hint::black_box;
use std::path::Path;
use std::time::{Duration, Instant};

use bytes::Bytes;
use bytesize::ByteSize;
use criterion::{Criterion, criterion_group, criterion_main};

const BUFSIZES: &[usize] = &[
    4,
    64,
    256,
    1024,
    4096,
    16 << 10,
    32 << 10,
    64 << 10,
    // 256 << 10,
    // 512 << 10,
    // 1 << 20,
    // 4 << 20,
];

const CONCURRENCY: usize = 200;
const METADATA: usize = 4096;

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
        2 << 31
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
#[inline]
fn make_buf(bufsize: usize, expected: u8) -> Bytes {
    // Black boxed to mock runtime values
    black_box(Bytes::from(vec![black_box(expected); bufsize]))
}

#[inline]
fn check_req(bufsize: usize, req: &[u8]) {
    check_buf(bufsize, req, REQ);
}

#[inline]
fn req(bufsize: usize) -> Bytes {
    make_buf(bufsize, REQ)
}

#[inline]
fn resp(bufsize: usize) -> Bytes {
    make_buf(bufsize, RESP)
}

#[inline]
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

fn groups(c: &mut Criterion) {
    macro_rules! benches {
        ($($name:ident),* $(,)?) => ([$((stringify!($name), self::$name::bench as BenchFn),)*]);
    }

    let mut g = c.benchmark_group("ipc_benchmark");

    for (i, bufsize) in BUFSIZES.iter().copied().enumerate() {
        let bsize = ByteSize::b(bufsize as u64).display().iec_short();

        for (name, f) in benches![monoio, shmipc, evering] {
            let id = format!("ipc_benchmark_{i:02}_{bsize:.0}_{name}");
            g.bench_function(&id, |b| {
                b.iter_custom(|iters| f(&id, iters as usize, bufsize))
            });
        }
    }
}

criterion_group!(
    name = ipc_benchmark;
    config = Criterion::default().sample_size(50).measurement_time(Duration::from_secs(30));
    targets = groups
);

criterion_main!(ipc_benchmark);
