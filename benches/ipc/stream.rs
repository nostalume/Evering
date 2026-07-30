use std::{
    io::{self, ErrorKind, Read, Write},
    net::TcpStream,
};

#[cfg(not(feature = "process"))]
use super::model::Status;
#[cfg(any(feature = "process", test))]
use std::time::Instant;
#[cfg(feature = "process")]
use {
    super::model::{Cell, Observed, Status, payload, valid_response, window},
    evering::process::Supervisor,
    std::{
        net::{Shutdown, TcpListener},
        process::Command,
        time::Duration,
    },
};

#[derive(Clone, Default)]
pub struct Counts {
    pub accepted: u64,
    pub completed: u64,
    pub validated: u64,
    pub elapsed_ns: u64,
    pub phase_ns: [u64; 3],
    #[cfg(feature = "process")]
    pub observed: Option<Observed>,
}

pub struct RunError {
    pub status: Status,
    pub message: String,
    pub counts: Counts,
}

fn fail(status: Status, message: impl ToString, counts: Option<&Counts>) -> RunError {
    RunError {
        status,
        message: message.to_string(),
        counts: counts.cloned().unwrap_or_default(),
    }
}

fn would_block(error: &io::Error) -> bool {
    error.kind() == ErrorKind::WouldBlock || error.raw_os_error() == Some(10035)
}

fn write_frame(stream: &mut TcpStream, operation: u64, bytes: &[u8]) -> io::Result<()> {
    stream.write_all(&operation.to_le_bytes())?;
    stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
    stream.write_all(bytes)
}

fn read_frame(stream: &mut TcpStream) -> io::Result<Option<(u64, Vec<u8>)>> {
    let mut header = [0; 12];
    match stream.read(&mut header[..1]) {
        Ok(0) => return Ok(None),
        Ok(1) => {}
        Ok(_) => unreachable!(),
        Err(error) => return Err(error),
    }
    stream.read_exact(&mut header[1..])?;
    let len = u32::from_le_bytes(header[8..].try_into().unwrap()) as usize;
    if len > 1024 * 1024 {
        return Err(io::Error::new(ErrorKind::InvalidData, "oversize frame"));
    }
    let mut bytes = vec![0; len];
    stream.read_exact(&mut bytes)?;
    Ok(Some((
        u64::from_le_bytes(header[..8].try_into().unwrap()),
        bytes,
    )))
}

#[cfg(feature = "process")]
fn write_until(mut stream: &TcpStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        match stream.write(bytes) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(written) => bytes = &bytes[written..],
            Err(error) if would_block(&error) && Instant::now() < deadline => {
                std::thread::yield_now()
            }
            Err(error) if would_block(&error) => return Err(ErrorKind::TimedOut.into()),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(any(feature = "process", test))]
pub(super) fn read_until(
    mut stream: &TcpStream,
    mut bytes: &mut [u8],
    deadline: Instant,
) -> io::Result<()> {
    while !bytes.is_empty() {
        match stream.read(bytes) {
            Ok(0) => return Err(ErrorKind::UnexpectedEof.into()),
            Ok(read) => bytes = &mut bytes[read..],
            Err(error) if would_block(&error) && Instant::now() < deadline => {
                std::thread::yield_now()
            }
            Err(error) if would_block(&error) => return Err(ErrorKind::TimedOut.into()),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(feature = "process")]
fn write_frame_until(
    stream: &TcpStream,
    operation: u64,
    bytes: &[u8],
    deadline: Instant,
) -> io::Result<()> {
    let mut frame = Vec::with_capacity(12 + bytes.len());
    frame.extend_from_slice(&operation.to_le_bytes());
    frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(bytes);
    write_until(stream, &frame, deadline)
}

#[cfg(feature = "process")]
fn read_frame_until(stream: &TcpStream, deadline: Instant) -> io::Result<(u64, Vec<u8>)> {
    let mut header = [0; 12];
    read_until(stream, &mut header, deadline)?;
    let len = u32::from_le_bytes(header[8..].try_into().unwrap()) as usize;
    if len > 1024 * 1024 {
        return Err(io::Error::new(ErrorKind::InvalidData, "oversize frame"));
    }
    let mut bytes = vec![0; len];
    read_until(stream, &mut bytes, deadline)?;
    Ok((u64::from_le_bytes(header[..8].try_into().unwrap()), bytes))
}

pub fn serve(mut stream: TcpStream) -> io::Result<()> {
    while let Some((operation, mut request)) = read_frame(&mut stream)? {
        if operation == u64::MAX && request.is_empty() {
            continue;
        }
        request.iter_mut().for_each(|byte| *byte ^= 0xa5);
        write_frame(&mut stream, operation, &request)?;
    }
    Ok(())
}

pub fn worker(address: &str) -> Result<(), String> {
    serve(TcpStream::connect(address).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}

#[cfg(feature = "process")]
fn exchange(
    stream: &TcpStream,
    seed: u64,
    operation: u64,
    len: usize,
    deadline: Instant,
) -> io::Result<bool> {
    write_frame_until(stream, operation, &payload(seed, operation, len), deadline)?;
    let (returned, bytes) = read_frame_until(stream, deadline)?;
    Ok(returned == operation && valid_response(seed, operation, len, &bytes))
}

#[cfg(feature = "process")]
pub fn run(
    cell: &Cell,
    requested: u64,
    warmup: u64,
    seed: u64,
    timeout: Duration,
) -> Result<Counts, RunError> {
    let began = Instant::now();
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| fail(Status::SetupError, error, None))?;
    let address = listener
        .local_addr()
        .map_err(|error| fail(Status::SetupError, error, None))?;
    let executable =
        std::env::current_exe().map_err(|error| fail(Status::SetupError, error, None))?;
    let mut command = Command::new(executable);
    command.args(["worker-stream", &address.to_string()]);
    let mut child =
        Supervisor::spawn(&mut command).map_err(|error| fail(Status::SetupError, error, None))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| fail(Status::SetupError, error, None))?;
    let setup_deadline = Instant::now() + timeout;
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if would_block(&error) && Instant::now() < setup_deadline => {
                if child
                    .try_wait()
                    .map_err(|error| fail(Status::SetupError, error, None))?
                    .is_some()
                {
                    return Err(fail(Status::SetupError, "worker exited during setup", None));
                }
                std::thread::yield_now();
            }
            Err(error) => return Err(fail(Status::SetupError, error, None)),
        }
    };
    stream
        .set_nonblocking(true)
        .map_err(|error| fail(Status::SetupError, error, None))?;
    let socket = socket2::SockRef::from(&stream);
    let socket_send = socket
        .send_buffer_size()
        .map_err(|error| fail(Status::SetupError, error, None))? as u64;
    let socket_recv = socket
        .recv_buffer_size()
        .map_err(|error| fail(Status::SetupError, error, None))? as u64;
    let observed = Observed {
        payload: cell.payload,
        capacity: cell.capacity,
        in_flight: cell.in_flight,
        batch: window(requested, cell.capacity, cell.in_flight),
        topology: "1c1w".into(),
        transport: "ipv4-loopback".into(),
        extent: None,
        allocator: None,
        socket_send: Some(socket_send),
        socket_recv: Some(socket_recv),
    };
    let mut counts = Counts {
        accepted: 0,
        completed: 0,
        validated: 0,
        elapsed_ns: 0,
        phase_ns: [0; 3],
        observed: Some(observed),
    };
    for operation in requested..requested.saturating_add(warmup) {
        if !exchange(
            &stream,
            seed,
            operation,
            cell.payload as usize,
            setup_deadline,
        )
        .map_err(|error| fail(Status::SetupError, error, Some(&counts)))?
        {
            return Err(fail(
                Status::SetupError,
                "warmup validation failed",
                Some(&counts),
            ));
        }
    }
    write_frame_until(&stream, u64::MAX, &[], setup_deadline)
        .map_err(|error| fail(Status::SetupError, error, Some(&counts)))?;
    counts.phase_ns[0] = began.elapsed().as_nanos().max(1) as u64;
    let started = Instant::now();
    let timed_deadline = started + timeout;
    while counts.accepted < requested {
        let batch = window(requested - counts.accepted, cell.capacity, cell.in_flight);
        let first = counts.accepted;
        for operation in first..first + batch {
            write_frame_until(
                &stream,
                operation,
                &payload(seed, operation, cell.payload as usize),
                timed_deadline,
            )
            .map_err(|error| fail(Status::TimedError, error, Some(&counts)))?;
            counts.accepted += 1;
        }
        for expected in first..first + batch {
            let (operation, bytes) = read_frame_until(&stream, timed_deadline)
                .map_err(|error| fail(Status::TimedError, error, Some(&counts)))?;
            counts.completed += 1;
            if operation != expected
                || !valid_response(seed, operation, cell.payload as usize, &bytes)
            {
                return Err(fail(
                    Status::TimedError,
                    format!("invalid response for operation {expected}"),
                    Some(&counts),
                ));
            }
            counts.validated += 1;
        }
    }
    counts.elapsed_ns = started.elapsed().as_nanos().max(1) as u64;
    counts.phase_ns[1] = counts.elapsed_ns;
    stream
        .shutdown(Shutdown::Write)
        .map_err(|error| fail(Status::DrainError, error, Some(&counts)))?;
    let drain_started = Instant::now();
    let drain_deadline = drain_started + timeout;
    let exit = loop {
        if let Some(exit) = child
            .try_wait()
            .map_err(|error| fail(Status::DrainError, error, Some(&counts)))?
        {
            break exit;
        }
        if Instant::now() >= drain_deadline {
            child
                .kill_wait()
                .map_err(|error| fail(Status::DrainError, error, Some(&counts)))?;
            return Err(fail(
                Status::DrainError,
                "worker exceeded drain deadline",
                Some(&counts),
            ));
        }
        std::thread::yield_now();
    };
    counts.phase_ns[2] = drain_started.elapsed().as_nanos().max(1) as u64;
    if !exit.success() {
        return Err(fail(
            Status::DrainError,
            "worker failed during drain",
            Some(&counts),
        ));
    }
    Ok(counts)
}
