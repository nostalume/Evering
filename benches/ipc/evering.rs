#[cfg(unix)]
use std::sync::atomic::AtomicU64;
use std::{
    process::Command,
    time::{Duration, Instant},
};

use evering::{
    QueueChannel, RegionId, Repr, SchemaId, SchemaKey, TryRecvError, TrySendError,
    perlude::talc::{Access, Id, Session, SessionBy},
    process::{Bootstrap, Supervisor},
};

use super::{
    model::{Cell, Observed, Status, payload, valid_response, window},
    stream::{Counts, RunError},
};

pub(super) const REGION: RegionId = RegionId::new(0x4556_4552_494e_4742, 1);
const MAGIC: u64 = 0x4556_4552_4245_4e31;
const DATA: u64 = 0;
const BARRIER: u64 = 1;
const BOOTSTRAP_LEN: usize = 56;
#[cfg(unix)]
static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Envelope {
    kind: u64,
    operation: u64,
}

unsafe impl Repr for Envelope {
    const SCHEMA: SchemaKey = SchemaKey::new(SchemaId(0x6970_632e_656e_7631), 1);
}

fn fail(status: Status, message: impl ToString, counts: Option<&Counts>) -> RunError {
    RunError {
        status,
        message: message.to_string(),
        counts: Box::new(counts.cloned().unwrap_or_default()),
    }
}

fn put_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

pub(super) fn bootstrap(id: Id<Envelope>, extent: usize) -> Result<Bootstrap, String> {
    let mut bytes = Vec::with_capacity(BOOTSTRAP_LEN);
    put_u64(&mut bytes, MAGIC);
    put_u64(&mut bytes, id.region().high);
    put_u64(&mut bytes, id.region().low);
    bytes.extend_from_slice(&id.slab().to_le_bytes());
    bytes.extend_from_slice(&id.entry().to_le_bytes());
    put_u64(&mut bytes, id.generation() as u64);
    put_u64(&mut bytes, id.capacity() as u64);
    put_u64(&mut bytes, extent as u64);
    Bootstrap::new(bytes).map_err(|error| format!("bootstrap: {error:?}"))
}

fn take_u64(bytes: &[u8], offset: usize) -> Result<u64, String> {
    bytes
        .get(offset..offset + 8)
        .and_then(|value| value.try_into().ok())
        .map(u64::from_le_bytes)
        .ok_or_else(|| "truncated Evering bootstrap".into())
}

pub(super) fn parse(bytes: &[u8]) -> Result<(Id<Envelope>, usize), String> {
    if bytes.len() != BOOTSTRAP_LEN || take_u64(bytes, 0)? != MAGIC {
        return Err("invalid Evering bootstrap".into());
    }
    let region = RegionId::new(take_u64(bytes, 8)?, take_u64(bytes, 16)?);
    if region != REGION {
        return Err("wrong Evering region".into());
    }
    let slab = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
    let entry = u32::from_le_bytes(bytes[28..32].try_into().unwrap());
    let generation =
        usize::try_from(take_u64(bytes, 32)?).map_err(|_| "generation exceeds pointer width")?;
    let capacity =
        usize::try_from(take_u64(bytes, 40)?).map_err(|_| "capacity exceeds pointer width")?;
    let extent =
        usize::try_from(take_u64(bytes, 48)?).map_err(|_| "extent exceeds pointer width")?;
    if capacity == 0 || extent == 0 {
        return Err("zero Evering bootstrap bound".into());
    }
    Ok((Id::new(region, slab, entry, generation, capacity), extent))
}

fn open_session<S: evering::Source>(source: S, extent: usize) -> Result<Session<Envelope>, String>
where
    S::Error: core::fmt::Debug,
{
    SessionBy::open(
        source,
        evering::Request::new(extent, Access::READ | Access::WRITE),
        REGION,
    )
    .map_err(|error| format!("{error:?}"))
}

fn create_session<S: evering::Source>(source: S, extent: usize) -> Result<Session<Envelope>, String>
where
    S::Error: core::fmt::Debug,
{
    SessionBy::create(
        source,
        evering::Request::new(extent, Access::READ | Access::WRITE),
        REGION,
    )
    .map_err(|error| format!("{error:?}"))
}

fn serve(session: Session<Envelope>, id: Id<Envelope>) -> Result<(), String> {
    let view = session
        .acquire(id)
        .ok_or("worker could not acquire channel")?;
    let (tx, rx) = view.rsplit();
    loop {
        let record = match rx.try_recv() {
            Ok(record) => record,
            Err(TryRecvError::Empty) => {
                std::thread::yield_now();
                continue;
            }
            Err(TryRecvError::Disconnected) => {
                tx.close();
                return Ok(());
            }
        };
        let heap = session.heap();
        let (header, mut value) = heap
            .open::<Envelope, [u8]>(record)
            .map_err(|_| "worker rejected message identity")?;
        match header.kind {
            DATA => value.iter_mut().for_each(|byte| *byte ^= 0xa5),
            BARRIER if value.is_empty() => {}
            _ => return Err("worker rejected message envelope".into()),
        }
        let mut record = value.token_of().pack(header);
        loop {
            match tx.try_send(record) {
                Ok(()) => break,
                Err(TrySendError::Full(returned)) => {
                    record = returned;
                    std::thread::yield_now();
                }
                Err(TrySendError::Disconnected(_)) => {
                    return Err("parent closed response receiver".into());
                }
            }
        }
    }
}

#[cfg(unix)]
fn open_worker(address: &str) -> Result<(Session<Envelope>, Id<Envelope>), String> {
    use evering::os::unix::{UnixFd, process::Socket};

    let socket = Socket::bind(address).map_err(|error| error.to_string())?;
    let (bootstrap, resources) = socket
        .recv(3)
        .map_err(|error| error.to_string())?
        .into_parts();
    let (id, extent) = parse(bootstrap.as_ref())?;
    let mut resources = resources.into_vec();
    let source = UnixFd::from_fd(resources.remove(0)).map_err(|error| error.to_string())?;
    let _parent_event = unsafe { evering::os::Event::from_owned_fd(resources.remove(0)) };
    let _child_ring = unsafe { evering::os::Ring::from_owned_fd(resources.remove(0)) };
    let session = open_session(source, extent)?;
    Ok((session, id))
}

#[cfg(windows)]
fn open_worker(address: &str) -> Result<(Session<Envelope>, Id<Envelope>), String> {
    use evering::os::windows::{Section, process::Socket};
    use std::os::windows::io::AsRawHandle;

    let socket = Socket::connect(address).map_err(|error| error.to_string())?;
    let (bootstrap, resources) = socket
        .recv(3)
        .map_err(|error| error.to_string())?
        .into_parts();
    let (id, extent) = parse(bootstrap.as_ref())?;
    let mut resources = resources.into_vec();
    let source = Section::from_owned_handle(resources.remove(0));
    let _parent_event =
        unsafe { evering::os::Event::from_owned_handle(resources.remove(0).as_raw_handle()) };
    let _child_ring =
        unsafe { evering::os::Ring::from_owned_handle(resources.remove(0).as_raw_handle()) };
    let session = open_session(source, extent)?;
    Ok((session, id))
}

pub fn worker(address: &str) -> Result<(), String> {
    let (session, id) = open_worker(address)?;
    serve(session, id)
}

struct Setup {
    session: Session<Envelope>,
    id: Id<Envelope>,
    child: Supervisor,
    _notify: (
        evering::os::Ring,
        evering::os::Event,
        evering::os::Ring,
        evering::os::Event,
    ),
    #[cfg(unix)]
    socket_path: std::path::PathBuf,
}

#[cfg(unix)]
impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(unix)]
fn setup(extent: usize, capacity: usize, deadline: Instant) -> Result<Setup, String> {
    use evering::os::unix::{UnixFd, process::Socket};
    use std::os::fd::AsFd;

    let source =
        UnixFd::memfd("evering-bench", extent, false).map_err(|error| error.to_string())?;
    let session = create_session(source.borrow(), extent)?;
    let id = session
        .prepare(capacity)
        .ok_or("could not create channel")?;
    let path = std::env::temp_dir().join(format!(
        "evering-bench-{}-{}.sock",
        std::process::id(),
        NEXT_SOCKET.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut command = Command::new(std::env::current_exe().map_err(|error| error.to_string())?);
    command.args(["worker-evering", path.to_string_lossy().as_ref()]);
    let mut child = Supervisor::spawn(&mut command).map_err(|error| error.to_string())?;
    let socket = loop {
        match Socket::connect(&path) {
            Ok(socket) => break socket,
            Err(error) if Instant::now() < deadline => {
                if child
                    .try_wait()
                    .map_err(|error| error.to_string())?
                    .is_some()
                {
                    return Err("worker exited during setup".into());
                }
                let _ = error;
                std::thread::yield_now();
            }
            Err(error) => return Err(error.to_string()),
        }
    };
    let (parent_ring, parent_event) = evering::os::event().map_err(|error| error.to_string())?;
    let (child_ring, child_event) = evering::os::event().map_err(|error| error.to_string())?;
    socket
        .send(
            &bootstrap(id, extent)?,
            &[source.as_fd(), parent_event.as_fd(), child_ring.as_fd()],
        )
        .map_err(|error| error.to_string())?;
    Ok(Setup {
        session,
        id,
        child,
        _notify: (parent_ring, parent_event, child_ring, child_event),
        socket_path: path,
    })
}

#[cfg(windows)]
fn setup(extent: usize, capacity: usize, _deadline: Instant) -> Result<Setup, String> {
    use std::os::windows::io::AsHandle;

    use evering::os::windows::{Section, process::Listener};

    let listener = Listener::bind().map_err(|error| error.to_string())?;
    let mut command = Command::new(std::env::current_exe().map_err(|error| error.to_string())?);
    command.args(["worker-evering", listener.name()]);
    let child = Supervisor::spawn(&mut command).map_err(|error| error.to_string())?;
    let socket = listener.accept(&child).map_err(|error| error.to_string())?;
    let source = Section::anonymous(extent, Access::READ | Access::WRITE)
        .map_err(|error| error.to_string())?;
    let session = create_session(source.borrow(), extent)?;
    let id = session
        .prepare(capacity)
        .ok_or("could not create channel")?;
    let (parent_ring, parent_event) = evering::os::event().map_err(|error| error.to_string())?;
    let (child_ring, child_event) = evering::os::event().map_err(|error| error.to_string())?;
    socket
        .send(
            &child,
            &bootstrap(id, extent)?,
            &[
                source.as_handle(),
                parent_event.as_handle(),
                child_ring.as_handle(),
            ],
        )
        .map_err(|error| error.to_string())?;
    Ok(Setup {
        session,
        id,
        child,
        _notify: (parent_ring, parent_event, child_ring, child_event),
    })
}

pub fn run(
    cell: &Cell,
    requested: u64,
    warmup: u64,
    seed: u64,
    timeout: Duration,
) -> Result<Counts, RunError> {
    let began = Instant::now();
    let extent = usize::try_from(cell.memory)
        .map_err(|_| fail(Status::SetupError, "extent exceeds pointer width", None))?;
    let capacity = usize::try_from(cell.capacity)
        .map_err(|_| fail(Status::SetupError, "capacity exceeds pointer width", None))?;
    let payload_len = usize::try_from(cell.payload)
        .map_err(|_| fail(Status::SetupError, "payload exceeds pointer width", None))?;
    if cell.in_flight == 0 {
        return Err(fail(Status::SetupError, "in-flight must be nonzero", None));
    }
    let setup_deadline = Instant::now() + timeout;
    let mut setup = setup(extent, capacity, setup_deadline)
        .map_err(|error| fail(Status::SetupError, error, None))?;
    let view = setup
        .session
        .acquire(setup.id)
        .ok_or_else(|| fail(Status::SetupError, "parent could not acquire channel", None))?;
    let (tx, rx) = view.lsplit();
    let send_one = |operation: u64, len: usize, kind: u64, deadline: Instant| {
        let mut record = setup
            .session
            .heap()
            .copy(&payload(seed, operation, len))
            .map_err(|error| format!("allocate request: {error:?}"))?
            .pack(Envelope { kind, operation });
        loop {
            match tx.try_send(record) {
                Ok(()) => break,
                Err(TrySendError::Full(returned)) if Instant::now() < deadline => {
                    record = returned;
                    std::thread::yield_now();
                }
                Err(TrySendError::Full(returned)) => {
                    let _ = setup.session.heap().discard(returned);
                    return Err("request send timed out".to_owned());
                }
                Err(TrySendError::Disconnected(returned)) => {
                    let _ = setup.session.heap().discard(returned);
                    return Err("worker closed request receiver".to_owned());
                }
            }
        }
        Ok(())
    };
    let recv_one = |operation: u64, len: usize, kind: u64, deadline: Instant| {
        let record = loop {
            match rx.try_recv() {
                Ok(record) => break record,
                Err(TryRecvError::Empty) if Instant::now() < deadline => std::thread::yield_now(),
                Err(TryRecvError::Empty) => return Err("response receive timed out".to_owned()),
                Err(TryRecvError::Disconnected) => {
                    return Err("worker closed response sender".to_owned());
                }
            }
        };
        let heap = setup.session.heap();
        let (header, value) = heap
            .open::<Envelope, [u8]>(record)
            .map_err(|_| "parent rejected message identity".to_owned())?;
        Ok(header.kind == kind
            && header.operation == operation
            && (kind == BARRIER || valid_response(seed, operation, len, &value)))
    };
    let geometry = evering::perlude::talc::Geometry::auto(extent).ok();
    let mut counts = Counts {
        observed: Some(Observed {
            payload: cell.payload,
            capacity: cell.capacity,
            in_flight: cell.in_flight,
            batch: window(requested, cell.capacity, cell.in_flight),
            topology: "1c1w".into(),
            transport: "shared-memory".into(),
            extent: Some(cell.memory),
            allocator: geometry.map(|value| format!("{value:?}")),
            socket_send: None,
            socket_recv: None,
        }),
        ..Counts::default()
    };
    for operation in requested..requested.saturating_add(warmup) {
        send_one(operation, payload_len, DATA, setup_deadline)
            .map_err(|error| fail(Status::SetupError, error, Some(&counts)))?;
        if !recv_one(operation, payload_len, DATA, setup_deadline)
            .map_err(|error| fail(Status::SetupError, error, Some(&counts)))?
        {
            return Err(fail(
                Status::SetupError,
                "warmup validation failed",
                Some(&counts),
            ));
        }
    }
    send_one(u64::MAX, 0, BARRIER, setup_deadline)
        .map_err(|error| fail(Status::SetupError, error, Some(&counts)))?;
    if !recv_one(u64::MAX, 0, BARRIER, setup_deadline)
        .map_err(|error| fail(Status::SetupError, error, Some(&counts)))?
    {
        return Err(fail(
            Status::SetupError,
            "barrier validation failed",
            Some(&counts),
        ));
    }
    counts.phase_ns[0] = began.elapsed().as_nanos().max(1) as u64;
    let started = Instant::now();
    let deadline = started + timeout;
    while counts.accepted < requested {
        let batch = window(requested - counts.accepted, cell.capacity, cell.in_flight);
        let first = counts.accepted;
        for operation in first..first + batch {
            send_one(operation, payload_len, DATA, deadline)
                .map_err(|error| fail(Status::TimedError, error, Some(&counts)))?;
            counts.accepted += 1;
        }
        for operation in first..first + batch {
            let valid = recv_one(operation, payload_len, DATA, deadline)
                .map_err(|error| fail(Status::TimedError, error, Some(&counts)))?;
            counts.completed += 1;
            if !valid {
                return Err(fail(
                    Status::TimedError,
                    format!("invalid response for operation {operation}"),
                    Some(&counts),
                ));
            }
            counts.validated += 1;
        }
    }
    counts.elapsed_ns = started.elapsed().as_nanos().max(1) as u64;
    counts.phase_ns[1] = counts.elapsed_ns;
    tx.close();
    drop((tx, rx));
    let drain = Instant::now();
    let drain_deadline = drain + timeout;
    let exit = loop {
        if setup
            .child
            .try_wait()
            .map_err(|error| fail(Status::DrainError, error, Some(&counts)))?
            .is_some()
        {
            break setup
                .child
                .wait()
                .map_err(|error| fail(Status::DrainError, error, Some(&counts)))?;
        }
        if Instant::now() >= drain_deadline {
            setup
                .child
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
    if !exit.success() {
        return Err(fail(
            Status::DrainError,
            "worker failed during drain",
            Some(&counts),
        ));
    }
    let view = setup.session.acquire(setup.id).ok_or_else(|| {
        fail(
            Status::DrainError,
            "channel disappeared during drain",
            Some(&counts),
        )
    })?;
    setup.session.remove(setup.id, view).map_err(|_| {
        fail(
            Status::DrainError,
            "channel remained busy during drain",
            Some(&counts),
        )
    })?;
    counts.phase_ns[2] = drain.elapsed().as_nanos().max(1) as u64;
    Ok(counts)
}
