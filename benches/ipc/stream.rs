use std::{
    future::Future,
    io::{self, ErrorKind, Read, Write},
    net::TcpStream,
};

use super::drive::{self, Deadline, Interest, Path};
use super::environment;
use super::model::Status;
#[cfg(any(feature = "process", test))]
use std::time::Instant;
use tokio::io::{Interest as ReadyInterest, Ready};
#[cfg(feature = "process")]
use {
    super::model::{Cell, Observed, window},
    evering::process::Supervisor,
    std::{
        net::{Shutdown, TcpListener},
        process::Command,
    },
};

#[derive(Clone, Default)]
pub struct Counts {
    pub accepted: u64,
    pub completed: u64,
    pub validated: u64,
    pub elapsed_ns: u64,
    pub phase_ns: [u64; 3],
    pub path: Path,
    #[cfg(feature = "process")]
    pub observed: Option<Observed>,
}

pub struct RunError {
    pub status: Status,
    pub message: String,
    pub counts: Box<Counts>,
}

pub(super) fn fail(status: Status, message: impl ToString, counts: Option<&Counts>) -> RunError {
    RunError {
        status,
        message: message.to_string(),
        counts: Box::new(counts.cloned().unwrap_or_default()),
    }
}

pub(super) fn setup<T, E: ToString>(result: Result<T, E>) -> Result<T, RunError> {
    result.map_err(|error| fail(Status::SetupError, error, None))
}

pub(super) fn measured<E: std::fmt::Debug>(
    counts: &mut Counts,
    result: Result<drive::Measured, drive::MeasureError<E>>,
) -> Result<(), RunError> {
    let (done, path, setup_ns, elapsed_ns) = match result {
        Ok(done) => (done.counts, done.path, done.setup_ns, done.elapsed_ns),
        Err(error) => {
            counts.path = error.path;
            counts.accepted = error.error.counts.accepted;
            counts.completed = error.error.counts.completed;
            counts.validated = error.error.counts.validated;
            return Err(fail(
                if error.timed {
                    Status::TimedError
                } else {
                    Status::SetupError
                },
                format!("{:?}", error.error.kind),
                Some(counts),
            ));
        }
    };
    counts.accepted = done.accepted;
    counts.completed = done.completed;
    counts.validated = done.validated;
    counts.path = path;
    counts.phase_ns[0] = setup_ns;
    counts.elapsed_ns = elapsed_ns;
    counts.phase_ns[1] = counts.elapsed_ns;
    Ok(())
}

pub(super) fn wait_child(child: &mut Supervisor, deadline: Deadline) -> Result<bool, String> {
    loop {
        if let Some(exit) = child.try_wait().map_err(|error| error.to_string())? {
            return Ok(exit.success());
        }
        if deadline.remaining(Instant::now()).is_err() {
            child.kill_wait().map_err(|error| error.to_string())?;
            return Err("timeout".into());
        }
        std::thread::yield_now();
    }
}

fn would_block(error: &io::Error) -> bool {
    error.kind() == ErrorKind::WouldBlock || error.raw_os_error() == Some(10035)
}

#[cfg(feature = "process")]
pub(super) fn accept<T>(
    child: &mut Supervisor,
    deadline: Deadline,
    mut next: impl FnMut() -> io::Result<T>,
) -> Result<T, RunError> {
    loop {
        match next() {
            Ok(stream) => return Ok(stream),
            Err(error) if would_block(&error) && deadline.remaining(Instant::now()).is_ok() => {
                if setup(child.try_wait())?.is_some() {
                    return Err(fail(Status::SetupError, "worker exited during setup", None));
                }
                std::thread::yield_now();
            }
            Err(error) => return Err(fail(Status::SetupError, error, None)),
        }
    }
}

pub(super) trait Socket: Sized {
    type Native;
    fn from_native(stream: Self::Native) -> io::Result<Self>;
    fn try_read(&self, bytes: &mut [u8]) -> io::Result<usize>;
    fn try_write(&self, bytes: &[u8]) -> io::Result<usize>;
    fn ready(&self, interest: ReadyInterest) -> impl Future<Output = io::Result<Ready>>;
    fn shutdown(&self, how: std::net::Shutdown) -> io::Result<()>;
}

macro_rules! socket {
    ($async:ty, $native:ty) => {
        impl Socket for $async {
            type Native = $native;
            fn from_native(stream: Self::Native) -> io::Result<Self> {
                stream.set_nonblocking(true)?;
                Self::from_std(stream)
            }
            fn try_read(&self, bytes: &mut [u8]) -> io::Result<usize> {
                Self::try_read(self, bytes)
            }
            fn try_write(&self, bytes: &[u8]) -> io::Result<usize> {
                Self::try_write(self, bytes)
            }
            fn ready(&self, interest: ReadyInterest) -> impl Future<Output = io::Result<Ready>> {
                Self::ready(self, interest)
            }
            fn shutdown(&self, how: std::net::Shutdown) -> io::Result<()> {
                socket2::SockRef::from(self).shutdown(how)
            }
        }
    };
}

socket!(tokio::net::TcpStream, TcpStream);
#[cfg(all(unix, feature = "local-socket"))]
socket!(tokio::net::UnixStream, std::os::unix::net::UnixStream);

fn try_read<S: Socket>(stream: &S, bytes: &mut [u8], path: &mut Path) -> io::Result<Option<usize>> {
    match stream.try_read(bytes) {
        Ok(0) => Err(ErrorKind::UnexpectedEof.into()),
        Ok(read) => Ok(Some(read)),
        Err(error) if would_block(&error) => {
            path.recv_stalled = true;
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn write_frame(stream: &mut impl Write, operation: u64, bytes: &[u8]) -> io::Result<()> {
    stream.write_all(&operation.to_le_bytes())?;
    stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
    stream.write_all(bytes)
}

fn read_frame(stream: &mut impl Read) -> io::Result<Option<(u64, Vec<u8>)>> {
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

struct Pending {
    bytes: Vec<u8>,
    offset: usize,
}

pub struct Client<S: Socket = tokio::net::TcpStream> {
    stream: S,
    runtime: tokio::runtime::Runtime,
    send: Option<Pending>,
    header: [u8; 12],
    header_offset: usize,
    operation: u64,
    body: Vec<u8>,
    body_offset: usize,
    woke: bool,
    progressed: bool,
}

impl<S: Socket> Client<S> {
    pub fn new(stream: S::Native) -> io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let entered = runtime.enter();
        let stream = S::from_native(stream)?;
        drop(entered);
        Ok(Self {
            stream,
            runtime,
            send: None,
            header: [0; 12],
            header_offset: 0,
            operation: 0,
            body: Vec::new(),
            body_offset: 0,
            woke: false,
            progressed: false,
        })
    }

    fn shutdown(&self, how: std::net::Shutdown) -> io::Result<()> {
        self.stream.shutdown(how)
    }
}

impl<S: Socket> drive::Endpoint for Client<S> {
    type Error = io::Error;

    fn stage(&mut self, operation: u64, payload: Vec<u8>) -> io::Result<()> {
        if self.send.is_some() {
            return Err(io::Error::new(ErrorKind::InvalidInput, "already staged"));
        }
        let mut bytes = Vec::with_capacity(12 + payload.len());
        bytes.extend_from_slice(&operation.to_le_bytes());
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&payload);
        self.send = Some(Pending { bytes, offset: 0 });
        Ok(())
    }

    fn try_send(&mut self, path: &mut Path) -> io::Result<drive::Step<(), io::Error>> {
        let pending = self
            .send
            .as_mut()
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "nothing staged"))?;
        match self.stream.try_write(&pending.bytes[pending.offset..]) {
            Ok(0) => Err(ErrorKind::WriteZero.into()),
            Ok(written) => {
                self.progressed = true;
                pending.offset += written;
                if pending.offset == pending.bytes.len() {
                    self.send = None;
                    Ok(drive::Step::Committed(Ok(())))
                } else {
                    path.send_stalled = true;
                    path.partial_io = true;
                    Ok(drive::Step::Pending)
                }
            }
            Err(error) if would_block(&error) => {
                path.send_stalled = true;
                Ok(drive::Step::Pending)
            }
            Err(error) => Err(error),
        }
    }

    fn try_recv(
        &mut self,
        path: &mut Path,
        expected: drive::Expected,
    ) -> io::Result<drive::Step<bool, io::Error>> {
        while self.header_offset < self.header.len() {
            let read = try_read(&self.stream, &mut self.header[self.header_offset..], path)?;
            let Some(read) = read else {
                return Ok(drive::Step::Pending);
            };
            self.progressed = true;
            self.header_offset += read;
            if self.header_offset < self.header.len() {
                path.partial_io = true;
                return Ok(drive::Step::Pending);
            }
            let len = u32::from_le_bytes(self.header[8..].try_into().unwrap()) as usize;
            if len > 1024 * 1024 {
                return Err(io::Error::new(ErrorKind::InvalidData, "oversize frame"));
            }
            self.operation = u64::from_le_bytes(self.header[..8].try_into().unwrap());
            self.body.resize(len, 0);
        }
        while self.body_offset < self.body.len() {
            let read = try_read(&self.stream, &mut self.body[self.body_offset..], path)?;
            let Some(read) = read else {
                return Ok(drive::Step::Pending);
            };
            self.progressed = true;
            self.body_offset += read;
            if self.body_offset < self.body.len() {
                path.partial_io = true;
                return Ok(drive::Step::Pending);
            }
        }
        let result = expected.matches(self.operation, &self.body);
        self.header = [0; 12];
        self.header_offset = 0;
        self.body.clear();
        self.body_offset = 0;
        Ok(drive::Step::Committed(Ok(result)))
    }

    fn wait(&mut self, interest: Interest, deadline: Deadline, path: &mut Path) -> io::Result<()> {
        if self.woke && !self.progressed {
            path.stale_wake = true;
        }
        self.woke = false;
        self.progressed = false;
        let ready = match (interest.read, interest.write) {
            (true, true) => ReadyInterest::READABLE | ReadyInterest::WRITABLE,
            (true, false) => ReadyInterest::READABLE,
            (false, true) => ReadyInterest::WRITABLE,
            (false, false) => {
                return Err(io::Error::new(ErrorKind::InvalidInput, "no wait interest"));
            }
        };
        path.wait_entered = true;
        let timeout = deadline
            .remaining(Instant::now())
            .map_err(|_| ErrorKind::TimedOut)?;
        let entered = self.runtime.enter();
        let future = tokio::time::timeout(timeout, self.stream.ready(ready));
        drop(entered);
        let result = self.runtime.block_on(future);
        match result {
            Ok(Ok(_)) => {
                path.wait_returned = true;
                self.woke = true;
                Ok(())
            }
            Ok(Err(error)) => Err(error),
            Err(_) => Err(ErrorKind::TimedOut.into()),
        }
    }

    fn abort(&mut self) -> io::Result<()> {
        self.send = None;
        self.shutdown(std::net::Shutdown::Both)
    }
}

pub fn serve(mut stream: impl Read + Write) -> io::Result<()> {
    while let Some((operation, mut request)) = read_frame(&mut stream)? {
        request.iter_mut().for_each(|byte| *byte ^= 0xa5);
        write_frame(&mut stream, operation, &request)?;
    }
    Ok(())
}

pub fn worker(address: &str, expected_environment: &str) -> Result<(), String> {
    environment::admit(expected_environment)?;
    let stream = TcpStream::connect(address).map_err(|error| error.to_string())?;
    stream
        .set_nodelay(true)
        .map_err(|error| error.to_string())?;
    serve(stream).map_err(|error| error.to_string())
}

#[cfg(feature = "process")]
pub(super) fn exchange<S: Socket>(
    stream: S::Native,
    mut child: Supervisor,
    observed: Observed,
    work: drive::Work,
    warmup: u64,
    began: Instant,
    deadline: Deadline,
) -> Result<Counts, RunError> {
    let mut counts = Counts {
        observed: Some(observed),
        ..Counts::default()
    };
    let mut client =
        Client::<S>::new(stream).map_err(|error| fail(Status::SetupError, error, Some(&counts)))?;
    measured(
        &mut counts,
        drive::measure(&mut client, work, warmup, began, deadline),
    )?;
    client
        .shutdown(Shutdown::Write)
        .map_err(|error| fail(Status::DrainError, error, Some(&counts)))?;
    let drain_started = Instant::now();
    let exit = wait_child(&mut child, deadline)
        .map_err(|error| fail(Status::DrainError, error, Some(&counts)))?;
    counts.phase_ns[2] = drain_started.elapsed().as_nanos().max(1) as u64;
    if !exit {
        return Err(fail(Status::DrainError, "exit", Some(&counts)));
    }
    Ok(counts)
}

#[cfg(feature = "process")]
pub(super) fn observed(
    socket: socket2::SockRef<'_>,
    cell: &Cell,
    requested: u64,
    seed: u64,
    transport: &str,
) -> Result<(Observed, drive::Work), RunError> {
    let size = |result| setup(result).map(|value| value as u64);
    let observed = Observed {
        payload: cell.payload,
        capacity: cell.capacity,
        in_flight: cell.in_flight,
        window: window(requested, cell.capacity, cell.in_flight),
        topology: "1c1w".into(),
        transport: transport.into(),
        extent: None,
        allocator: None,
        socket_send: Some(size(socket.send_buffer_size())?),
        socket_recv: Some(size(socket.recv_buffer_size())?),
    };
    let work = drive::Work {
        start: 0,
        count: requested,
        window: cell.capacity.min(cell.in_flight),
        payload: usize::try_from(cell.payload)
            .map_err(|_| fail(Status::SetupError, "payload exceeds pointer width", None))?,
        seed,
    };
    Ok((observed, work))
}

#[cfg(feature = "process")]
pub fn run(
    cell: &Cell,
    requested: u64,
    warmup: u64,
    seed: u64,
    deadline: Deadline,
    environment: &str,
) -> Result<Counts, RunError> {
    let began = Instant::now();
    let listener = setup(TcpListener::bind(("127.0.0.1", 0)))?;
    let address = setup(listener.local_addr())?;
    let executable = setup(std::env::current_exe())?;
    let mut command = Command::new(executable);
    command.args(["worker-stream", &address.to_string(), environment]);
    let mut child = setup(Supervisor::spawn(&mut command))?;
    setup(listener.set_nonblocking(true))?;
    let stream = accept(&mut child, deadline, || {
        listener.accept().map(|value| value.0)
    })?;
    setup(stream.set_nodelay(true))?;
    let (observed, work) = observed(
        socket2::SockRef::from(&stream),
        cell,
        requested,
        seed,
        "ipv4-loopback",
    )?;
    exchange::<tokio::net::TcpStream>(stream, child, observed, work, warmup, began, deadline)
}
