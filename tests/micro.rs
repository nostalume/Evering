#![cfg(feature = "benchmark")]

#[allow(dead_code)]
#[path = "../benches/ipc/micro.rs"]
mod micro;

use std::time::Instant;

use ::evering::{
    Session,
    layout::{RegionId, Repr, SchemaId, SchemaKey},
    mapping::{Access, Request},
    notify::{Notify, Wait as _},
};
use micro::{Mechanism, Sample, measure};

const REGION: RegionId = RegionId::new(0x6d69_6372_6f2d_6970, 1);

#[repr(C)]
#[derive(Clone, Copy)]
struct Record(u64);

unsafe impl Repr for Record {
    const SCHEMA: SchemaKey = SchemaKey::new(SchemaId(0x6d69_6372_6f2d_7265), 1);
}

#[cfg(unix)]
fn session() -> Session {
    let source = ::evering::os::unix::UnixFd::memfd("evering-micro", 1 << 20, false).unwrap();
    Session::create(
        source.borrow(),
        Request::new(1 << 20, Access::READ | Access::WRITE),
        REGION,
    )
    .unwrap()
}

#[cfg(unix)]
fn session_pair() -> (Session, Session) {
    let source = ::evering::os::unix::UnixFd::memfd("evering-micro-pair", 1 << 20, false).unwrap();
    let request = Request::new(1 << 20, Access::READ | Access::WRITE);
    (
        Session::create(source.borrow(), request, REGION).unwrap(),
        Session::open(source.borrow(), request, REGION).unwrap(),
    )
}

#[cfg(windows)]
fn session() -> Session {
    let source =
        ::evering::os::windows::Section::anonymous(1 << 20, Access::READ | Access::WRITE).unwrap();
    Session::create(
        source.borrow(),
        Request::new(1 << 20, Access::READ | Access::WRITE),
        REGION,
    )
    .unwrap()
}

#[cfg(windows)]
fn session_pair() -> (Session, Session) {
    let source =
        ::evering::os::windows::Section::anonymous(1 << 20, Access::READ | Access::WRITE).unwrap();
    let request = Request::new(1 << 20, Access::READ | Access::WRITE);
    (
        Session::create(source.borrow(), request, REGION).unwrap(),
        Session::open(source.borrow(), request, REGION).unwrap(),
    )
}

fn queue_rows(iterations: u64) -> Vec<micro::Row> {
    let (session, peer) = session_pair();
    let (left, port) = session.create_channel::<Record>(8).unwrap();
    let right = peer.adopt(port).unwrap();
    let (tx, _) = left.split();
    let (_, rx) = right.split();
    let pool = session.create_pool(64 * 1024, None).unwrap();
    let peer_pool = peer.open_pool(pool.id()).unwrap();
    let pool = pool.as_ref();
    let peer_pool = peer_pool.as_ref();
    let reserve = measure(
        Mechanism::ReservePublish,
        iterations,
        "capacity=8",
        |operation| {
            let record = pool.copy(&[0_u8]).unwrap().transfer(Record(operation));
            let started = Instant::now();
            tx.try_send(record).map_err(|_| "send".to_owned())?;
            let elapsed = started.elapsed();
            rx.claim()
                .map_err(|_| "reset receive".to_owned())?
                .discard(peer_pool)
                .map_err(|error| format!("reset discard: {error:?}"))?;
            Ok(Sample {
                elapsed,
                operations: 1,
                reset: true,
            })
        },
    )
    .unwrap();
    let claim = measure(
        Mechanism::ClaimRecycle,
        iterations,
        "capacity=8",
        |operation| {
            tx.try_send(pool.copy(&[0_u8]).unwrap().transfer(Record(operation)))
                .map_err(|_| "setup send".to_owned())?;
            let started = Instant::now();
            let claim = rx.claim().map_err(|_| "receive".to_owned())?;
            let (_, block) = claim
                .adopt::<[u8]>(peer_pool)
                .map_err(|error| format!("adopt: {error:?}"))?;
            let elapsed = started.elapsed();
            drop(block);
            Ok(Sample {
                elapsed,
                operations: 1,
                reset: true,
            })
        },
    )
    .unwrap();
    vec![reserve, claim]
}

#[test]
fn registered_local_mechanisms_reset_every_iteration() {
    let iterations = std::env::var("EVERING_MICRO_ITERATIONS")
        .map_or(Ok(8), |value| value.parse::<u64>())
        .unwrap();
    let session = session();
    let heap = session.heap();
    let talc = measure(
        Mechanism::AllocateRelease,
        iterations,
        "surface=typed;bytes=64;allocator=adaptive",
        |_| {
            let started = Instant::now();
            drop(heap.copy(&[0_u8; 64]).unwrap());
            Ok(Sample {
                elapsed: started.elapsed(),
                operations: 1,
                reset: true,
            })
        },
    )
    .unwrap();
    let pool = session.create_pool(64 * 1024, None).unwrap();
    let pool = pool.as_ref();
    let pool = measure(
        Mechanism::AllocateRelease,
        iterations,
        "surface=typed;bytes=64;allocator=pool",
        |iteration| {
            let started = Instant::now();
            let block = pool
                .copy(&[0_u8; 64])
                .map_err(|error| format!("reserve {iteration}: {error:?}"))?;
            drop(block);
            Ok(Sample {
                elapsed: started.elapsed(),
                operations: 1,
                reset: true,
            })
        },
    )
    .unwrap();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (ring, event) = ::evering::os::event().unwrap();
    let wait = {
        let _entered = runtime.enter();
        ::evering::runtime::Wait::new(event).unwrap()
    };
    let notification = measure(Mechanism::Notify, iterations, "sticky=native", |_| {
        let started = Instant::now();
        ring.notify().map_err(|error| error.to_string())?;
        let elapsed = started.elapsed();
        runtime
            .block_on(wait.wait())
            .map_err(|error| error.to_string())?;
        Ok(Sample {
            elapsed,
            operations: 1,
            reset: true,
        })
    })
    .unwrap();
    let signal_consume = measure(
        Mechanism::SignalConsume,
        iterations,
        "sticky=native",
        |_| {
            let started = Instant::now();
            ring.notify().map_err(|error| error.to_string())?;
            runtime
                .block_on(wait.wait())
                .map_err(|error| error.to_string())?;
            Ok(Sample {
                elapsed: started.elapsed(),
                operations: 1,
                reset: true,
            })
        },
    )
    .unwrap();

    for row in queue_rows(iterations)
        .into_iter()
        .chain([talc, pool, notification, signal_consume])
    {
        assert_eq!(row.iterations, iterations);
        assert!(row.gross_ns > 0);
        print!("{}", row.encode());
    }
}

#[test]
fn pool_contention_candidate() {
    let iterations = std::env::var("EVERING_MICRO_ITERATIONS")
        .map_or(Ok(1000), |value| value.parse::<u64>())
        .unwrap();
    let session = session();
    let pool = session.create_pool(64 * 1024, None).unwrap();
    let pool = pool.as_ref();
    let started = Instant::now();
    std::thread::scope(|scope| {
        for thread in 0..4 {
            scope.spawn(move || {
                for value in 0..iterations {
                    std::hint::black_box(pool.put(value ^ thread).unwrap());
                }
            });
        }
    });
    println!(
        "POOL-CONTENTION\tthreads=4\toperations={}\telapsed_ns={}",
        iterations * 4,
        started.elapsed().as_nanos()
    );
}

#[test]
fn process_exchange_child() {
    use std::io::{Read, Write};

    let Ok(address) = std::env::var("EVERING_MICRO_CHILD") else {
        return;
    };
    let mut stream = std::net::TcpStream::connect(address).unwrap();
    let mut value = [0; 8];
    stream.read_exact(&mut value).unwrap();
    value[0] ^= 0xa5;
    stream.write_all(&value).unwrap();
}

#[test]
fn registered_process_exchange_is_cross_process_and_reset() {
    use std::io::{Read, Write};

    let row = measure(
        Mechanism::ProcessExchange,
        2,
        "transport=ipv4-loopback;bytes=8",
        |iteration| {
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", "process_exchange_child", "--nocapture"])
                .env(
                    "EVERING_MICRO_CHILD",
                    listener.local_addr().unwrap().to_string(),
                );
            let mut child = ::evering::process::Supervisor::spawn(&mut command)
                .map_err(|error| error.to_string())?;
            let (mut stream, _) = listener.accept().map_err(|error| error.to_string())?;
            let mut value = iteration.to_le_bytes();
            let started = Instant::now();
            stream
                .write_all(&value)
                .and_then(|()| stream.read_exact(&mut value))
                .map_err(|error| error.to_string())?;
            let elapsed = started.elapsed();
            let status = child.wait().map_err(|error| error.to_string())?;
            Ok(Sample {
                elapsed,
                operations: 1,
                reset: status.success() && value[0] == (iteration as u8 ^ 0xa5),
            })
        },
    )
    .unwrap();
    assert_eq!(row.iterations, 2);
    assert!(row.encode().contains("process-exchange\tcross-process"));
    print!("{}", row.encode());
}
