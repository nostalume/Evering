use std::{
    io::{self, ErrorKind, Read, Write},
    net::TcpStream,
};

#[cfg(feature = "process")]
use evering::process::Supervisor;
#[cfg(feature = "process")]
use std::{
    net::{Shutdown, TcpListener},
    process::Command,
    time::{Duration, Instant},
};

use super::model::{Cell, Status, payload, response, valid_response};

pub struct Counts {
    pub accepted: u64,
    pub completed: u64,
    pub validated: u64,
    pub elapsed_ns: u64,
}

pub struct RunError {
    pub status: Status,
    pub message: String,
    pub accepted: u64,
    pub completed: u64,
    pub validated: u64,
}

fn fail(status: Status, message: impl ToString, counts: Option<&Counts>) -> RunError {
    RunError {
        status,
        message: message.to_string(),
        accepted: counts.map_or(0, |value| value.accepted),
        completed: counts.map_or(0, |value| value.completed),
        validated: counts.map_or(0, |value| value.validated),
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
    let operation = u64::from_le_bytes(header[..8].try_into().unwrap());
    let len = u32::from_le_bytes(header[8..].try_into().unwrap()) as usize;
    if len > 1024 * 1024 {
        return Err(io::Error::new(ErrorKind::InvalidData, "oversize frame"));
    }
    let mut bytes = vec![0; len];
    stream.read_exact(&mut bytes)?;
    Ok(Some((operation, bytes)))
}

pub fn round_trip(
    stream: &mut TcpStream,
    seed: u64,
    operation: u64,
    len: usize,
) -> io::Result<bool> {
    write_frame(stream, operation, &payload(seed, operation, len))?;
    let Some((returned, bytes)) = read_frame(stream)? else {
        return Err(ErrorKind::UnexpectedEof.into());
    };
    Ok(returned == operation && valid_response(seed, operation, len, &bytes))
}

pub fn serve(mut stream: TcpStream) -> io::Result<()> {
    while let Some((operation, request)) = read_frame(&mut stream)? {
        write_frame(&mut stream, operation, &response(request))?;
    }
    Ok(())
}

pub fn worker(address: &str) -> Result<(), String> {
    let stream = TcpStream::connect(address).map_err(|error| error.to_string())?;
    serve(stream).map_err(|error| error.to_string())
}

#[cfg(feature = "process")]
pub fn run(
    cell: &Cell,
    requested: u64,
    warmup: u64,
    seed: u64,
    timeout: Duration,
) -> Result<Counts, RunError> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| fail(Status::SetupError, error, None))?;
    let address = listener
        .local_addr()
        .map_err(|error| fail(Status::SetupError, error, None))?;
    let executable =
        std::env::current_exe().map_err(|error| fail(Status::SetupError, error, None))?;
    let mut command = Command::new(executable);
    command.args(["worker-stream", &address.to_string()]);
    let mut child = Supervisor::spawn(&mut command)
        .map_err(|error| fail(Status::SetupError, format!("spawn worker: {error}"), None))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| fail(Status::SetupError, error, None))?;
    let deadline = Instant::now() + timeout;
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if would_block(&error) && Instant::now() < deadline => {
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
        .set_nonblocking(false)
        .map_err(|error| fail(Status::SetupError, error, None))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|error| fail(Status::SetupError, error, None))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|error| fail(Status::SetupError, error, None))?;

    for operation in requested..requested.saturating_add(warmup) {
        if !round_trip(&mut stream, seed, operation, cell.payload as usize)
            .map_err(|error| fail(Status::SetupError, error, None))?
        {
            return Err(fail(Status::SetupError, "warmup validation failed", None));
        }
    }

    let mut counts = Counts {
        accepted: 0,
        completed: 0,
        validated: 0,
        elapsed_ns: 0,
    };
    let started = Instant::now();
    while counts.accepted < requested {
        let batch = (requested - counts.accepted).min(cell.in_flight);
        let first = counts.accepted;
        for operation in first..first + batch {
            write_frame(
                &mut stream,
                operation,
                &payload(seed, operation, cell.payload as usize),
            )
            .map_err(|error| fail(Status::TimedError, error, Some(&counts)))?;
            counts.accepted += 1;
        }
        for expected in first..first + batch {
            let Some((operation, bytes)) = read_frame(&mut stream)
                .map_err(|error| fail(Status::TimedError, error, Some(&counts)))?
            else {
                return Err(fail(
                    Status::TimedError,
                    "worker closed during timed work",
                    Some(&counts),
                ));
            };
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
    stream
        .shutdown(Shutdown::Write)
        .map_err(|error| fail(Status::DrainError, error, Some(&counts)))?;
    if !child
        .wait()
        .map_err(|error| fail(Status::DrainError, error, Some(&counts)))?
        .success()
    {
        return Err(fail(
            Status::DrainError,
            "worker failed during drain",
            Some(&counts),
        ));
    }
    Ok(counts)
}
