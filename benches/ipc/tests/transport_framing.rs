//! The evering arm read as a transport: bootstrap binds region, channel and extent; a
//! framed stream carries the delivery digest and nothing else; readiness is a validated
//! round trip whose buffers stay inside their declared capacity.

use crate::{boundary, drive, evering_arm, model, stream};

#[cfg(feature = "process")]
#[test]
fn evering_bootstrap_binds_region_channel_and_extent() {
    use ::evering::{ChannelId as Id, Port};

    let id = Id::new(evering_arm::REGION, 2, 3, 4, 5);
    let port = Port::from_parts(id, 1, 8).unwrap();
    let pool = ::evering::PoolId::new(evering_arm::REGION, 5, 6, 7);
    let encoded = evering_arm::bootstrap(&port, pool, 4096).unwrap();
    let (decoded, decoded_pool, decoded_extent) = evering_arm::parse(encoded.as_ref()).unwrap();
    assert_eq!(decoded.id(), id);
    assert_eq!((decoded.role(), decoded.generation()), (1, 8));
    assert_eq!((decoded_pool, decoded_extent), (pool, 4096));
    let mut invalid = encoded.into_boxed();
    invalid[0] ^= 1;
    assert!(evering_arm::parse(&invalid).is_err());
    assert!(
        evering_arm::parse(
            evering_arm::bootstrap(
                &Port::from_parts(Id::new(evering_arm::REGION, 2, 3, 4, 0), 1, 8).unwrap(),
                pool,
                4096,
            )
            .unwrap()
            .as_ref()
        )
        .is_err()
    );
    assert!(evering_arm::parse(evering_arm::bootstrap(&port, pool, 0).unwrap().as_ref()).is_err());
}

#[test]
fn delivery_digest_reads_the_complete_payload() {
    let payload = model::payload(7, 11, 4096);
    let digest = model::digest(&payload);
    let mut changed = payload.clone();
    changed[2047] ^= 1;
    assert_ne!(digest, model::digest(&changed));
}

#[test]
fn framed_stream_returns_only_the_delivery_digest() {
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        stream::serve(listener.accept().unwrap().0, boundary::ABSENT).unwrap()
    });
    let mut client = TcpStream::connect(address).unwrap();
    let payload = model::payload(7, 11, 4096);
    client.write_all(&11_u64.to_le_bytes()).unwrap();
    client
        .write_all(&(payload.len() as u32).to_le_bytes())
        .unwrap();
    client.write_all(&payload).unwrap();
    let mut header = [0; 12];
    client.read_exact(&mut header).unwrap();
    let mut response = vec![0; u32::from_le_bytes(header[8..].try_into().unwrap()) as usize];
    client.read_exact(&mut response).unwrap();
    assert_eq!(u64::from_le_bytes(header[..8].try_into().unwrap()), 11);
    assert_eq!(response, model::digest(&payload).to_le_bytes());
    client.shutdown(Shutdown::Write).unwrap();
    worker.join().unwrap();
}

#[cfg(unix)]
#[test]
fn framing_is_transport_independent() {
    use std::{
        io::{Read, Write},
        net::Shutdown,
        os::unix::net::UnixStream,
    };

    let (mut client, server) = UnixStream::pair().unwrap();
    let worker = std::thread::spawn(move || stream::serve(server, boundary::ABSENT).unwrap());
    client.write_all(&7_u64.to_le_bytes()).unwrap();
    client.write_all(&3_u32.to_le_bytes()).unwrap();
    client.write_all(&[1, 2, 3]).unwrap();
    let mut response = [0; 20];
    client.read_exact(&mut response).unwrap();
    assert_eq!(&response[..12], &[7, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0, 0]);
    assert_eq!(&response[12..], &model::digest(&[1, 2, 3]).to_le_bytes());
    client.shutdown(Shutdown::Write).unwrap();
    worker.join().unwrap();
}

#[test]
fn framed_stream_ready_is_a_validated_round_trip() {
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::time::Duration;

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        stream::serve(listener.accept().unwrap().0, boundary::ABSENT).unwrap()
    });
    let mut client = TcpStream::connect(address).unwrap();
    client
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    client.write_all(&u64::MAX.to_le_bytes()).unwrap();
    client.write_all(&0_u32.to_le_bytes()).unwrap();
    let mut response = [0; 12];
    client.read_exact(&mut response).unwrap();
    assert_eq!(
        u64::from_le_bytes(response[..8].try_into().unwrap()),
        u64::MAX
    );
    assert_eq!(u32::from_le_bytes(response[8..].try_into().unwrap()), 0);
    client.shutdown(Shutdown::Write).unwrap();
    worker.join().unwrap();
}

#[test]
fn readiness_stream_progresses_with_constrained_duplex_buffers() {
    use drive::Endpoint as _;
    use std::net::{TcpListener, TcpStream};
    use std::time::{Duration, Instant};

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    // Nagle is off on every other TCP leg this study runs (`stream::worker`, `stream::run`). It matters
    // most here: a 64 KiB request across a 4 KiB duplex is tens of segments, and each one whose peer's
    // buffer just drained waits on the delayed-ACK timer instead of on the driver being tested.
    let worker = std::thread::spawn(move || {
        let stream = listener.accept().unwrap().0;
        stream.set_nodelay(true).unwrap();
        let socket = socket2::SockRef::from(&stream);
        socket.set_send_buffer_size(4096).unwrap();
        socket.set_recv_buffer_size(4096).unwrap();
        stream::serve(stream, boundary::ABSENT).unwrap();
    });
    let client = TcpStream::connect(address).unwrap();
    client.set_nodelay(true).unwrap();
    let socket = socket2::SockRef::from(&client);
    socket.set_send_buffer_size(4096).unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    let mut client = stream::Client::<tokio::net::TcpStream>::new(client).unwrap();
    let deadline = drive::Deadline::after(Instant::now(), Duration::from_secs(5)).unwrap();
    drive::transfer(
        &mut client,
        drive::Work {
            start: u64::MAX,
            count: 1,
            window: 1,
            payload: 0,
            seed: 7,
        },
        deadline,
        &mut drive::PathCounts::default(),
    )
    .unwrap();
    let mut path = drive::PathCounts::default();
    let counts = drive::transfer(
        &mut client,
        drive::Work {
            start: 0,
            count: 32,
            window: 8,
            payload: 64 * 1024,
            seed: 7,
        },
        deadline,
        &mut path,
    )
    .unwrap();
    assert_eq!(
        (counts.accepted, counts.completed, counts.validated),
        (32, 32, 32)
    );
    client.abort().unwrap();
    worker.join().unwrap();
}
