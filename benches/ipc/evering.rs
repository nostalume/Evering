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
    model::{Cell, Observed, Policy, Status, payload, valid_response, window},
    stream::{Counts, RunError},
};

pub(super) const REGION: RegionId = RegionId::new(0x4556_4552_494e_4742, 1);
const MAGIC: u64 = 0x4556_4552_4245_4e31;
const DATA: u64 = 0;
const BARRIER: u64 = 1;
const BOOTSTRAP_LEN: usize = 56;
pub(super) const ADAPTIVE_SPINS: usize = 64;
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

fn text(error: impl ToString) -> String {
    error.to_string()
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

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(text)
}

#[derive(Clone, Copy)]
struct Local<'a> {
    ring: &'a evering::os::Ring,
    wait: &'a evering::runtime::Wait,
    runtime: &'a tokio::runtime::Runtime,
}

fn signal<const WAIT: bool>(ring: &evering::os::Ring) -> bool {
    !WAIT || evering::Notify::notify(ring).is_ok()
}

fn retry_send<const SPINS: usize, const WAIT: bool, S, V>(
    endpoint: &S,
    local: Local<'_>,
    deadline: Instant,
    value: V,
) -> Result<bool, (V, String)>
where
    S: evering::Sender<Item = V, TryError = TrySendError<V>> + Clone,
{
    let mut pending = evering::Pending::new(value);
    let mut spins = SPINS;
    loop {
        let value = pending.into_inner().unwrap();
        match endpoint.try_send(value) {
            Ok(()) => return Ok(signal::<WAIT>(local.ring)),
            Err(TrySendError::Disconnected(value)) => return Err((value, "disconnected".into())),
            Err(TrySendError::Full(value)) if Instant::now() >= deadline => {
                return Err((value, "timeout".into()));
            }
            Err(TrySendError::Full(value)) if spins > 0 => {
                pending = evering::Pending::new(value);
                if WAIT {
                    spins -= 1;
                    std::hint::spin_loop();
                } else {
                    std::thread::yield_now();
                }
            }
            Err(TrySendError::Full(value)) => {
                pending = evering::Pending::new(value);
                let async_endpoint = evering::Async::new(endpoint.clone(), local.ring, local.wait);
                let send = async_endpoint.send(&mut pending);
                let timeout = {
                    let _entered = local.runtime.enter();
                    tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), send)
                };
                return match local.runtime.block_on(timeout) {
                    Ok(Ok(done)) => Ok(done.notified.is_ok()),
                    Ok(Err(_)) => Err((pending.into_inner().unwrap(), "wait failed".into())),
                    Err(_) => Err((pending.into_inner().unwrap(), "timeout".into())),
                };
            }
        }
    }
}

fn retry_recv<const SPINS: usize, const WAIT: bool, R>(
    endpoint: &R,
    local: Local<'_>,
    deadline: Instant,
) -> Result<(R::Item, bool), String>
where
    R: evering::Receiver<TryError = TryRecvError> + Clone,
{
    let mut spins = SPINS;
    loop {
        match endpoint.try_recv() {
            Ok(value) => return Ok((value, signal::<WAIT>(local.ring))),
            Err(TryRecvError::Disconnected) => return Err("sender disconnected".into()),
            Err(TryRecvError::Empty) if Instant::now() >= deadline => return Err("timeout".into()),
            Err(TryRecvError::Empty) if spins > 0 => {
                if WAIT {
                    spins -= 1;
                    std::hint::spin_loop();
                } else {
                    std::thread::yield_now();
                }
            }
            Err(TryRecvError::Empty) => {
                let async_endpoint = evering::Async::new(endpoint.clone(), local.ring, local.wait);
                let recv = async_endpoint.recv();
                let timeout = {
                    let _entered = local.runtime.enter();
                    tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), recv)
                };
                return match local.runtime.block_on(timeout) {
                    Ok(Ok(done)) => Ok((done.value, done.notified.is_ok())),
                    Ok(Err(evering::RecvError::Disconnected)) => Err("sender disconnected".into()),
                    Ok(Err(error)) => Err(format!("receive wait failed: {error:?}")),
                    Err(_) => Err("timeout".into()),
                };
            }
        }
    }
}

fn serve<const SPINS: usize, const WAIT: bool>(
    session: Session<Envelope>,
    id: Id<Envelope>,
    ring: evering::os::Ring,
    wait: evering::runtime::Wait,
    runtime: &tokio::runtime::Runtime,
    timeout: Duration,
) -> Result<(), String> {
    let view = session
        .acquire(id)
        .ok_or("worker could not acquire channel")?;
    let (tx, rx) = view.rsplit();
    let local = Local {
        ring: &ring,
        wait: &wait,
        runtime,
    };
    loop {
        let deadline = Instant::now() + timeout;
        let (record, received) = match retry_recv::<SPINS, WAIT, _>(&rx, local, deadline) {
            Ok(record) => record,
            Err(error) if error == "sender disconnected" => {
                tx.close();
                if !signal::<WAIT>(&ring) {
                    return Err("notify".into());
                }
                return Ok(());
            }
            Err(error) => return Err(error),
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
        let record = value.token_of().pack(header);
        let sent = match retry_send::<SPINS, WAIT, _, _>(&tx, local, deadline, record) {
            Ok(sent) => sent,
            Err((record, error)) => {
                let _ = session.heap().discard(record);
                return Err(error);
            }
        };
        if !received || !sent {
            return Err("notification failed after commit".into());
        }
    }
}

#[cfg(unix)]
fn open_worker(
    address: &str,
) -> Result<
    (
        Session<Envelope>,
        Id<Envelope>,
        evering::os::Ring,
        evering::runtime::Wait,
    ),
    String,
> {
    use evering::os::unix::{UnixFd, process::Socket};

    let socket = Socket::bind(address).map_err(text)?;
    let (bootstrap, resources) = socket.recv(3).map_err(text)?.into_parts();
    let (id, extent) = parse(bootstrap.as_ref())?;
    let mut resources = resources.into_vec();
    let source = UnixFd::from_fd(resources.remove(0)).map_err(text)?;
    let parent_event = unsafe { evering::os::Event::from_owned_fd(resources.remove(0)) };
    let child_ring = unsafe { evering::os::Ring::from_owned_fd(resources.remove(0)) };
    let session = open_session(source, extent)?;
    let wait = evering::runtime::Wait::new(parent_event).map_err(text)?;
    Ok((session, id, child_ring, wait))
}

#[cfg(windows)]
fn open_worker(
    address: &str,
) -> Result<
    (
        Session<Envelope>,
        Id<Envelope>,
        evering::os::Ring,
        evering::runtime::Wait,
    ),
    String,
> {
    use evering::os::windows::{Section, process::Socket};
    use std::os::windows::io::IntoRawHandle;

    let socket = Socket::connect(address).map_err(text)?;
    let (bootstrap, resources) = socket.recv(3).map_err(text)?.into_parts();
    let (id, extent) = parse(bootstrap.as_ref())?;
    let mut resources = resources.into_vec();
    let source = Section::from_owned_handle(resources.remove(0));
    let parent_event =
        unsafe { evering::os::Event::from_owned_handle(resources.remove(0).into_raw_handle()) };
    let child_ring =
        unsafe { evering::os::Ring::from_owned_handle(resources.remove(0).into_raw_handle()) };
    let session = open_session(source, extent)?;
    let wait = evering::runtime::Wait::new(parent_event).map_err(text)?;
    Ok((session, id, child_ring, wait))
}

fn worker_with<const SPINS: usize, const WAIT: bool>(
    address: &str,
    timeout: Duration,
) -> Result<(), String> {
    let runtime = runtime()?;
    let entered = runtime.enter();
    let (session, id, ring, wait) = open_worker(address)?;
    drop(entered);
    serve::<SPINS, WAIT>(session, id, ring, wait, &runtime, timeout)
}

pub fn worker(address: &str, policy: &str, timeout: Duration) -> Result<(), String> {
    match policy {
        "busy" => worker_with::<{ usize::MAX }, false>(address, timeout),
        "adaptive" => worker_with::<ADAPTIVE_SPINS, true>(address, timeout),
        "notified" => worker_with::<0, true>(address, timeout),
        _ => Err("unknown Evering retry policy".into()),
    }
}

struct Setup {
    session: Session<Envelope>,
    id: Id<Envelope>,
    child: Supervisor,
    runtime: tokio::runtime::Runtime,
    ring: evering::os::Ring,
    wait: evering::runtime::Wait,
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
fn setup(
    extent: usize,
    capacity: usize,
    deadline: Instant,
    policy: &str,
    timeout: Duration,
) -> Result<Setup, String> {
    use evering::os::unix::{UnixFd, process::Socket};
    use std::os::fd::AsFd;

    let runtime = runtime()?;
    let entered = runtime.enter();
    let source = UnixFd::memfd("evering-bench", extent, false).map_err(text)?;
    let session = create_session(source.borrow(), extent)?;
    let id = session
        .prepare(capacity)
        .ok_or("could not create channel")?;
    let path = std::env::temp_dir().join(format!(
        "evering-bench-{}-{}.sock",
        std::process::id(),
        NEXT_SOCKET.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut command = Command::new(std::env::current_exe().map_err(text)?);
    let timeout = timeout.as_millis().to_string();
    command.args([
        "worker-evering",
        path.to_string_lossy().as_ref(),
        policy,
        &timeout,
    ]);
    let mut child = Supervisor::spawn(&mut command).map_err(text)?;
    let socket = loop {
        match Socket::connect(&path) {
            Ok(socket) => break socket,
            Err(_) if Instant::now() < deadline => {
                if child.try_wait().map_err(text)?.is_some() {
                    return Err("worker exited during setup".into());
                }
                std::thread::yield_now();
            }
            Err(error) => return Err(error.to_string()),
        }
    };
    let (parent_ring, parent_event) = evering::os::event().map_err(text)?;
    let (child_ring, child_event) = evering::os::event().map_err(text)?;
    socket
        .send(
            &bootstrap(id, extent)?,
            &[source.as_fd(), parent_event.as_fd(), child_ring.as_fd()],
        )
        .map_err(text)?;
    let wait = evering::runtime::Wait::new(child_event).map_err(text)?;
    drop(entered);
    Ok(Setup {
        session,
        id,
        child,
        runtime,
        ring: parent_ring,
        wait,
        socket_path: path,
    })
}

#[cfg(windows)]
fn setup(
    extent: usize,
    capacity: usize,
    _deadline: Instant,
    policy: &str,
    timeout: Duration,
) -> Result<Setup, String> {
    use std::os::windows::io::AsHandle;

    use evering::os::windows::{Section, process::Listener};

    let runtime = runtime()?;
    let entered = runtime.enter();
    let listener = Listener::bind().map_err(text)?;
    let mut command = Command::new(std::env::current_exe().map_err(text)?);
    let timeout = timeout.as_millis().to_string();
    command.args(["worker-evering", listener.name(), policy, &timeout]);
    let child = Supervisor::spawn(&mut command).map_err(text)?;
    let socket = listener.accept(&child).map_err(text)?;
    let source = Section::anonymous(extent, Access::READ | Access::WRITE).map_err(text)?;
    let session = create_session(source.borrow(), extent)?;
    let id = session
        .prepare(capacity)
        .ok_or("could not create channel")?;
    let (parent_ring, parent_event) = evering::os::event().map_err(text)?;
    let (child_ring, child_event) = evering::os::event().map_err(text)?;
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
        .map_err(text)?;
    let wait = evering::runtime::Wait::new(child_event).map_err(text)?;
    drop(entered);
    Ok(Setup {
        session,
        id,
        child,
        runtime,
        ring: parent_ring,
        wait,
    })
}

fn run_with<const SPINS: usize, const WAIT: bool>(
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
    let setup = setup(extent, capacity, setup_deadline, &cell.policy, timeout);
    let mut setup = setup.map_err(|error| fail(Status::SetupError, error, None))?;
    let view = setup
        .session
        .acquire(setup.id)
        .ok_or_else(|| fail(Status::SetupError, "parent could not acquire channel", None))?;
    let (tx, rx) = view.lsplit();
    let local = Local {
        ring: &setup.ring,
        wait: &setup.wait,
        runtime: &setup.runtime,
    };
    let send_one = |operation: u64, len: usize, kind: u64, deadline: Instant| {
        let record = setup
            .session
            .heap()
            .copy(&payload(seed, operation, len))
            .map_err(|error| format!("allocate request: {error:?}"))?
            .pack(Envelope { kind, operation });
        match retry_send::<SPINS, WAIT, _, _>(&tx, local, deadline, record) {
            Ok(notified) => Ok(notified),
            Err((record, error)) => {
                let _ = setup.session.heap().discard(record);
                Err(error)
            }
        }
    };
    let recv_one =
        |operation: u64, len: usize, kind: u64, deadline: Instant| -> Result<_, String> {
            let (record, notified) = retry_recv::<SPINS, WAIT, _>(&rx, local, deadline)?;
            let heap = setup.session.heap();
            let (header, value) = heap
                .open::<Envelope, [u8]>(record)
                .map_err(|_| "parent rejected message identity".to_owned())?;
            Ok((
                header.kind == kind
                    && header.operation == operation
                    && (kind == BARRIER || valid_response(seed, operation, len, &value)),
                notified,
            ))
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
        let sent = send_one(operation, payload_len, DATA, setup_deadline)
            .map_err(|error| fail(Status::SetupError, error, Some(&counts)))?;
        if !sent {
            return Err(fail(Status::SetupError, "notify", Some(&counts)));
        }
        let (valid, notified) = recv_one(operation, payload_len, DATA, setup_deadline)
            .map_err(|error| fail(Status::SetupError, error, Some(&counts)))?;
        if !valid || !notified {
            return Err(fail(Status::SetupError, "warmup failed", Some(&counts)));
        }
    }
    let sent = send_one(u64::MAX, 0, BARRIER, setup_deadline)
        .map_err(|error| fail(Status::SetupError, error, Some(&counts)))?;
    let (valid, notified) = recv_one(u64::MAX, 0, BARRIER, setup_deadline)
        .map_err(|error| fail(Status::SetupError, error, Some(&counts)))?;
    if !sent || !valid || !notified {
        return Err(fail(Status::SetupError, "barrier failed", Some(&counts)));
    }
    counts.phase_ns[0] = began.elapsed().as_nanos().max(1) as u64;
    let started = Instant::now();
    let deadline = started + timeout;
    while counts.accepted < requested {
        let batch = window(requested - counts.accepted, cell.capacity, cell.in_flight);
        let first = counts.accepted;
        for operation in first..first + batch {
            let notified = send_one(operation, payload_len, DATA, deadline)
                .map_err(|error| fail(Status::TimedError, error, Some(&counts)))?;
            counts.accepted += 1;
            if !notified {
                return Err(fail(Status::TimedError, "notify", Some(&counts)));
            }
        }
        for operation in first..first + batch {
            let (valid, notified) = recv_one(operation, payload_len, DATA, deadline)
                .map_err(|error| fail(Status::TimedError, error, Some(&counts)))?;
            counts.completed += 1;
            counts.validated += u64::from(valid);
            if !notified {
                return Err(fail(Status::TimedError, "notify", Some(&counts)));
            }
            if !valid {
                let message = format!("invalid response {operation}");
                return Err(fail(Status::TimedError, message, Some(&counts)));
            }
        }
    }
    counts.elapsed_ns = started.elapsed().as_nanos().max(1) as u64;
    counts.phase_ns[1] = counts.elapsed_ns;
    tx.close();
    if !signal::<WAIT>(&setup.ring) {
        return Err(fail(Status::DrainError, "notify", Some(&counts)));
    }
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
            return Err(fail(Status::DrainError, "timeout", Some(&counts)));
        }
        std::thread::yield_now();
    };
    if !exit.success() {
        return Err(fail(Status::DrainError, "exit", Some(&counts)));
    }
    let view = setup
        .session
        .acquire(setup.id)
        .ok_or_else(|| fail(Status::DrainError, "channel disappeared", Some(&counts)))?;
    setup
        .session
        .remove(setup.id, view)
        .map_err(|_| fail(Status::DrainError, "channel remained busy", Some(&counts)))?;
    counts.phase_ns[2] = drain.elapsed().as_nanos().max(1) as u64;
    Ok(counts)
}

pub fn run(
    policy: Policy,
    cell: &Cell,
    requested: u64,
    warmup: u64,
    seed: u64,
    timeout: Duration,
) -> Result<Counts, RunError> {
    match policy {
        Policy::Busy => run_with::<{ usize::MAX }, false>(cell, requested, warmup, seed, timeout),
        Policy::Adaptive => {
            run_with::<ADAPTIVE_SPINS, true>(cell, requested, warmup, seed, timeout)
        }
        Policy::Notified => run_with::<0, true>(cell, requested, warmup, seed, timeout),
    }
}
