use std::os::unix::{
    ffi::OsStrExt,
    net::{UnixListener, UnixStream},
};
use std::{process::Command, time::Instant};

use evering::process::Supervisor;

use super::{
    drive::Deadline,
    environment,
    model::{Cell, Status},
    stream::{self, Counts, RunError},
};

pub fn worker(path: &str, expected_environment: &str) -> Result<(), String> {
    environment::admit(expected_environment)?;
    stream::serve(UnixStream::connect(path).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}

pub fn run(
    cell: &Cell,
    requested: u64,
    warmup: u64,
    seed: u64,
    deadline: Deadline,
    environment: &str,
) -> Result<Counts, RunError> {
    let began = Instant::now();
    let directory = stream::setup(tempfile::Builder::new().prefix("evering-").tempdir())?;
    let endpoint = directory.path().join("ipc");
    if endpoint.as_os_str().as_bytes().len() > 100 {
        return Err(stream::fail(
            Status::SetupError,
            "Unix socket path exceeds portable limit",
            None,
        ));
    }
    let listener = stream::setup(UnixListener::bind(&endpoint))?;
    let executable = stream::setup(std::env::current_exe())?;
    let path = endpoint
        .to_str()
        .ok_or_else(|| stream::fail(Status::SetupError, "non-UTF-8 endpoint", None))?;
    let mut command = Command::new(executable);
    command.args(["worker-local", path, environment]);
    let mut child = stream::setup(Supervisor::spawn(&mut command))?;
    stream::setup(listener.set_nonblocking(true))?;
    let socket = stream::accept(&mut child, deadline, || {
        listener.accept().map(|value| value.0)
    })?;
    let (observed, work) = stream::observed(
        socket2::SockRef::from(&socket),
        cell,
        requested,
        seed,
        "unix-domain-stream",
    )?;
    stream::exchange::<tokio::net::UnixStream>(
        socket, child, observed, work, warmup, began, deadline,
    )
}
