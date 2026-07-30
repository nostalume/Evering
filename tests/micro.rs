#![cfg(feature = "benchmark")]

#[allow(dead_code)]
#[path = "../benches/ipc/micro.rs"]
mod micro;

use std::time::Instant;

use ::evering::{
    Listen, Notify, RegionId, Repr, SchemaId, SchemaKey,
    perlude::talc::{Access, Session, SessionBy},
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
fn session() -> Session<Record> {
    let source = ::evering::os::unix::UnixFd::memfd("evering-micro", 1 << 20, false).unwrap();
    SessionBy::create(
        source.borrow(),
        ::evering::Request::new(1 << 20, Access::READ | Access::WRITE),
        REGION,
    )
    .unwrap()
}

#[cfg(windows)]
fn session() -> Session<Record> {
    let source =
        ::evering::os::windows::Section::anonymous(1 << 20, Access::READ | Access::WRITE).unwrap();
    SessionBy::create(
        source.borrow(),
        ::evering::Request::new(1 << 20, Access::READ | Access::WRITE),
        REGION,
    )
    .unwrap()
}

fn queue_rows(iterations: u64) -> Vec<micro::Row> {
    let session = session();
    let id = session.prepare(8).unwrap();
    let view = session.acquire(id).unwrap();
    let (tx, _) = view.clone().lsplit();
    let (_, rx) = view.rsplit();
    let heap = session.heap();
    let reserve = measure(
        Mechanism::ReservePublish,
        iterations,
        "capacity=8",
        |operation| {
            let record = heap.copy::<u8>(&[]).unwrap().pack(Record(operation));
            let started = Instant::now();
            tx.try_send(record).map_err(|_| "send".to_owned())?;
            let elapsed = started.elapsed();
            let record = rx.try_recv().map_err(|_| "reset receive".to_owned())?;
            heap.discard(record)
                .map_err(|_| "reset discard".to_owned())?;
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
            tx.try_send(heap.copy::<u8>(&[]).unwrap().pack(Record(operation)))
                .map_err(|_| "setup send".to_owned())?;
            let started = Instant::now();
            let record = rx.try_recv().map_err(|_| "receive".to_owned())?;
            let elapsed = started.elapsed();
            heap.discard(record)
                .map_err(|_| "reset discard".to_owned())?;
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
    let session = session();
    let heap = session.heap();
    let allocation = measure(
        Mechanism::AllocateRelease,
        8,
        "surface=typed;bytes=64;allocator=adaptive",
        |_| {
            let started = Instant::now();
            let record = heap.copy(&[0_u8; 64]).unwrap().pack(Record(0));
            heap.discard(record).map_err(|_| "release".to_owned())?;
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
    let _entered = runtime.enter();
    let (ring, event) = ::evering::os::event().unwrap();
    let wait = ::evering::runtime::Wait::new(event).unwrap();
    let notification = measure(Mechanism::SignalConsume, 8, "sticky=native", |_| {
        let started = Instant::now();
        ring.notify().map_err(|error| error.to_string())?;
        wait.clear().map_err(|error| error.to_string())?;
        Ok(Sample {
            elapsed: started.elapsed(),
            operations: 1,
            reset: true,
        })
    })
    .unwrap();

    for row in queue_rows(8).into_iter().chain([allocation, notification]) {
        assert_eq!(row.iterations, 8);
        assert!(row.gross_ns > 0);
        print!("{}", row.encode());
    }
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
