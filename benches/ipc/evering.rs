#[cfg(unix)]
use std::sync::atomic::AtomicU64;
use std::{
    process::Command,
    time::{Duration, Instant},
};

use evering::{
    BlockRange, Channel, ChannelId as Id, Decline, DeclineCause, Pool, PoolId, Port, Rx, Session,
    Tx,
    layout::{RegionId, Repr, SchemaId, SchemaKey},
    mapping::{Access, Request, Source},
    notify::{Contention, Patience, ProgressError, Signals, Wait as _},
    process::{Bootstrap, Supervisor},
};

use super::{
    boundary::{self, Trailer},
    drive::{self, Deadline, Expected, Interest, PathCounts, Step},
    environment,
    model::{self, Cell, Observed, Status, window},
    stream::{Counts, RunError, fail, measured, wait_child},
};

pub(super) const REGION: RegionId = RegionId::new(0x4556_4552_494e_4742, 1);
pub(super) const POOL_EXTENT: usize = 16_515_072;
const MAGIC: u64 = 0x4556_4552_4245_4e31;
const DATA: u64 = 0;
const READY: u64 = 1;
const BOOTSTRAP_LEN: usize = 88;
pub(super) const ADAPTIVE_SPINS: usize = 64;
#[cfg(unix)]
static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);

pub(super) fn pool_range() -> BlockRange {
    BlockRange::new(64, 64 * 1024).unwrap()
}

fn pool_geometry(pool: &Pool) -> String {
    let classes: Vec<_> = (0..).map_while(|index| pool.class(index)).collect();
    format!(
        "extent={POOL_EXTENT};range={:?};classes={classes:?}",
        pool.range()
    )
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Envelope {
    kind: u64,
    operation: u64,
    digest: u64,
}

unsafe impl Repr for Envelope {
    const SCHEMA: SchemaKey = SchemaKey::new(SchemaId(0x6970_632e_656e_7631), 2);
}

fn text(error: impl ToString) -> String {
    error.to_string()
}

fn put_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

pub(super) fn bootstrap(
    port: &Port<Envelope>,
    pool: PoolId,
    extent: usize,
) -> Result<Bootstrap, String> {
    let id = port.id();
    let mut bytes = Vec::with_capacity(BOOTSTRAP_LEN);
    put_u64(&mut bytes, MAGIC);
    put_u64(&mut bytes, id.region().high);
    put_u64(&mut bytes, id.region().low);
    bytes.extend_from_slice(&id.slab().to_le_bytes());
    bytes.extend_from_slice(&id.entry().to_le_bytes());
    put_u64(&mut bytes, id.generation() as u64);
    put_u64(&mut bytes, id.capacity() as u64);
    put_u64(&mut bytes, port.role() as u64);
    put_u64(&mut bytes, port.generation() as u64);
    put_u64(&mut bytes, extent as u64);
    let (_, slab, entry, generation) = pool.parts();
    bytes.extend_from_slice(&slab.to_le_bytes());
    bytes.extend_from_slice(&entry.to_le_bytes());
    put_u64(&mut bytes, generation as u64);
    Bootstrap::new(bytes).map_err(|error| format!("bootstrap: {error:?}"))
}

fn take_u64(bytes: &[u8], offset: usize) -> Result<u64, String> {
    bytes
        .get(offset..offset + 8)
        .and_then(|value| value.try_into().ok())
        .map(u64::from_le_bytes)
        .ok_or_else(|| "truncated Evering bootstrap".into())
}

pub(super) fn parse(bytes: &[u8]) -> Result<(Port<Envelope>, PoolId, usize), String> {
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
    let role = u8::try_from(take_u64(bytes, 48)?).map_err(|_| "role exceeds u8")?;
    let port_generation =
        usize::try_from(take_u64(bytes, 56)?).map_err(|_| "generation exceeds pointer width")?;
    let extent =
        usize::try_from(take_u64(bytes, 64)?).map_err(|_| "extent exceeds pointer width")?;
    if capacity == 0 || extent == 0 {
        return Err("zero Evering bootstrap bound".into());
    }
    let pool = PoolId::new(
        region,
        u32::from_le_bytes(bytes[72..76].try_into().unwrap()),
        u32::from_le_bytes(bytes[76..80].try_into().unwrap()),
        usize::try_from(take_u64(bytes, 80)?).map_err(|_| "generation exceeds pointer width")?,
    );
    let id = Id::new(region, slab, entry, generation, capacity);
    Ok((
        Port::from_parts(id, role, port_generation).ok_or("invalid Evering Port")?,
        pool,
        extent,
    ))
}

fn open_session<S: Source>(source: S, extent: usize) -> Result<Session, String>
where
    S::Error: core::fmt::Debug,
{
    Session::open(
        source,
        Request::new(extent, Access::READ | Access::WRITE),
        REGION,
    )
    .map_err(|error| format!("{error:?}"))
}

fn create_session<S: Source>(source: S, extent: usize) -> Result<Session, String>
where
    S::Error: core::fmt::Debug,
{
    Session::create(
        source,
        Request::new(extent, Access::READ | Access::WRITE),
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

impl Local<'_> {
    #[expect(
        clippy::result_large_err,
        reason = "an uncommitted Transfer stays inline for retry"
    )]
    fn try_send<'p, const WAIT: bool, H: Repr>(
        self,
        endpoint: &Tx<H>,
        value: evering::Transfer<'p, H>,
    ) -> Result<Result<(), String>, Decline<evering::Transfer<'p, H>>> {
        if WAIT {
            Signals::new(self.ring, self.wait)
                .try_send(endpoint, value)
                .map(|committed| committed.into_parts().1.map_err(text))
        } else {
            endpoint.try_send(value).map(|()| Ok(()))
        }
    }

    #[expect(
        clippy::type_complexity,
        reason = "benchmark separates pre-commit admission failure from committed signal health"
    )]
    fn adopt<'q, 'p, const WAIT: bool, H, T>(
        self,
        received: evering::Received<'q, H>,
        pool: evering::PoolRef<'p>,
    ) -> Result<Result<(H, evering::Block<'p, T>), String>, evering::AdoptError<'q, H>>
    where
        H: Repr,
        T: Repr + evering::layout::Shape + ?Sized,
    {
        if WAIT {
            Signals::new(self.ring, self.wait)
                .adopt(received, pool)
                .map(|committed| {
                    let (value, notified) = committed.into_parts();
                    notified.map(|()| value).map_err(text)
                })
        } else {
            received.adopt(pool).map(Ok)
        }
    }

    fn close<const WAIT: bool, H: Repr>(self, tx: &Tx<H>) -> Result<(), String> {
        if WAIT {
            Signals::new(self.ring, self.wait)
                .close_tx(tx)
                .into_parts()
                .1
                .map_err(text)
        } else {
            tx.close();
            Ok(())
        }
    }
}

/// The study's own waiting policy: the ladder these arms were written against, run
/// through the library's retry loop so an arm's headline number is attributable to
/// shipped surface.
///
/// It differs from `notify::Adaptive` in the two ways that are the subject of the
/// measurement rather than incidental detail: the spin budget is spent once per call
/// and a park does not refill it, and the arm that never parks yields the OS thread
/// instead of spinning. The deadline lives here, which is why the loop it drives has
/// no clock of its own.
struct Ladder<'a> {
    local: Local<'a>,
    deadline: Deadline,
    budget: usize,
    left: usize,
    park: bool,
}

impl<'a> Ladder<'a> {
    fn new<const SPINS: usize, const WAIT: bool>(local: Local<'a>, deadline: Deadline) -> Self {
        Self {
            local,
            deadline,
            budget: SPINS,
            left: SPINS,
            park: WAIT,
        }
    }
}

impl Patience for Ladder<'_> {
    type Error = std::io::Error;

    fn begin(&mut self) {
        self.left = self.budget;
    }

    async fn step(&mut self, reason: Contention) -> Result<bool, Self::Error> {
        let remaining = self
            .deadline
            .remaining(Instant::now())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))?;
        match reason {
            // A busy slot is resolved inside this process, so the ladder spends one
            // pause and re-attempts; only a peer-bound reason may spend the budget or
            // reach the latch, which is what lets a park be counted as one wait.
            Contention::Busy => std::hint::spin_loop(),
            Contention::Full | Contention::Empty if !self.park => std::thread::yield_now(),
            Contention::Full | Contention::Empty if self.left > 0 => {
                self.left -= 1;
                std::hint::spin_loop();
            }
            Contention::Full | Contention::Empty => {
                tokio::time::timeout(remaining, self.local.wait.wait())
                    .await
                    .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))??;
            }
        }
        Ok(true)
    }
}

/// The step-end strings the two arms reported before the loop was shared: a stopped
/// policy and an expired deadline are the loop's own outcomes, not the peer's.
fn send_stop(error: ProgressError<std::io::Error>) -> String {
    match error {
        ProgressError::Closed => "disconnected".into(),
        ProgressError::Busy => "waiting policy stopped".into(),
        ProgressError::Wait(error) if error.kind() == std::io::ErrorKind::TimedOut => {
            "timeout".into()
        }
        ProgressError::Wait(error) => text(error),
    }
}

fn adopt_stop(error: ProgressError<std::io::Error>) -> String {
    match error {
        ProgressError::Closed => "sender disconnected".into(),
        ProgressError::Busy => "waiting policy stopped".into(),
        ProgressError::Wait(error) if error.kind() == std::io::ErrorKind::TimedOut => {
            "timeout".into()
        }
        ProgressError::Wait(error) => format!("receive wait failed: {error:?}"),
    }
}

async fn retry_send<'p, const SPINS: usize, const WAIT: bool, H: Repr>(
    endpoint: &Tx<H>,
    local: Local<'_>,
    deadline: Deadline,
    value: evering::Transfer<'p, H>,
) -> Result<(), String> {
    let mut ladder = Ladder::new::<SPINS, WAIT>(local, deadline);
    // An arm that never parks is also an arm that never rings: the same library loop
    // over a notifier that cannot notify, which is the shape `Local::try_send` selects
    // for `WAIT = false`.
    if WAIT {
        let signals = Signals::new(local.ring, local.wait);
        let permit = signals
            .reserve_with(endpoint, &mut ladder)
            .await
            .map_err(send_stop)?;
        permit
            .send(value)
            .map_err(|_| "lost block authority".to_string())?
            .into_parts()
            .1
            .map_err(text)
    } else {
        let signals = Signals::none();
        let permit = signals
            .reserve_with(endpoint, &mut ladder)
            .await
            .map_err(send_stop)?;
        permit
            .send(value)
            .map_err(|_| "lost block authority".to_string())?
            .into_parts()
            .1
            .map_err(text)
    }
}

async fn retry_adopt<'p, const SPINS: usize, const WAIT: bool, H, T>(
    endpoint: &Rx<H>,
    pool: evering::PoolRef<'p>,
    local: Local<'_>,
    deadline: Deadline,
) -> Result<(H, evering::Block<'p, T>), String>
where
    H: Repr,
    T: Repr + evering::layout::Shape + ?Sized,
{
    let mut ladder = Ladder::new::<SPINS, WAIT>(local, deadline);
    let signals = Signals::new(local.ring, local.wait);
    let received = signals
        .claim_with(endpoint, &mut ladder)
        .await
        .map_err(adopt_stop)?;
    local
        .adopt::<WAIT, H, T>(received, pool)
        .map_err(|error| format!("transfer admission failed: {error:?}"))?
}

struct Client<'a, M, O, D, const SPINS: usize, const WAIT: bool> {
    tx: Tx<Envelope>,
    rx: Rx<Envelope>,
    staged: Option<evering::Transfer<'a, Envelope>>,
    make: M,
    open: O,
    discard: D,
    pool: evering::PoolRef<'a>,
    local: Local<'a>,
    send_spins: usize,
    recv_spins: usize,
    contended: bool,
    woke: bool,
    progressed: bool,
}

impl<'a, M, O, D, const SPINS: usize, const WAIT: bool> Client<'a, M, O, D, SPINS, WAIT> {
    fn new(
        tx: Tx<Envelope>,
        rx: Rx<Envelope>,
        make: M,
        open: O,
        discard: D,
        pool: evering::PoolRef<'a>,
        local: Local<'a>,
    ) -> Self {
        Self {
            tx,
            rx,
            staged: None,
            make,
            open,
            discard,
            pool,
            local,
            send_spins: SPINS,
            recv_spins: SPINS,
            contended: false,
            woke: false,
            progressed: false,
        }
    }
}

impl<M, O, D, const SPINS: usize, const WAIT: bool> Client<'_, M, O, D, SPINS, WAIT> {
    fn close(&self) -> Result<(), String> {
        self.local.close::<WAIT, _>(&self.tx)
    }
}

impl<'a, M, O, D, const SPINS: usize, const WAIT: bool> drive::Endpoint
    for Client<'a, M, O, D, SPINS, WAIT>
where
    M: FnMut(u64, &[u8]) -> Result<evering::Transfer<'a, Envelope>, String>,
    O: for<'p> FnMut((Envelope, evering::Block<'p, [u8]>), Expected) -> Result<bool, String>,
    D: FnMut(evering::Transfer<'a, Envelope>) -> Result<(), String>,
{
    type Error = String;

    fn stage(&mut self, operation: u64, payload: &[u8]) -> Result<(), String> {
        if self.staged.is_some() {
            return Err("already staged".into());
        }
        self.staged = Some((self.make)(operation, payload)?);
        Ok(())
    }

    fn try_send(&mut self, path: &mut PathCounts) -> Result<Step<(), String>, String> {
        let value = self.staged.take().ok_or("nothing staged")?;
        match self.local.try_send::<WAIT, _>(&self.tx, value) {
            Ok(notified) => {
                self.progressed = true;
                self.send_spins = SPINS;
                Ok(Step::Committed(notified))
            }
            Err(Decline { cause, value }) => match (cause, value) {
                (DeclineCause::NoRoom { .. }, Some(value)) => {
                    self.staged = Some(value);
                    path.send_full().map_err(|_| "path count overflow")?;
                    Ok(Step::Pending)
                }
                (DeclineCause::Contested { .. }, Some(value)) => {
                    self.staged = Some(value);
                    self.contended = true;
                    path.send_busy().map_err(|_| "path count overflow")?;
                    Ok(Step::Pending)
                }
                (DeclineCause::Sealed, Some(value)) => {
                    self.staged = Some(value);
                    Err("disconnected".into())
                }
                (_, None) => Err("lost block authority".into()),
                (cause, Some(value)) => {
                    self.staged = Some(value);
                    Err(format!("send refused for {cause:?}"))
                }
            },
        }
    }

    fn try_recv(
        &mut self,
        path: &mut PathCounts,
        expected: Expected,
    ) -> Result<Step<bool, String>, String> {
        match self.rx.claim() {
            Ok(received) => match self
                .local
                .adopt::<WAIT, Envelope, [u8]>(received, self.pool)
            {
                Ok(value) => {
                    self.progressed = true;
                    self.recv_spins = SPINS;
                    Ok(Step::Committed(
                        value.and_then(|value| (self.open)(value, expected)),
                    ))
                }
                Err(error) => Err(format!("transfer admission failed: {error:?}")),
            },
            Err(evering::ReceiveError::Empty) => {
                path.recv_empty().map_err(|_| "path count overflow")?;
                Ok(Step::Pending)
            }
            Err(evering::ReceiveError::Busy) => {
                self.contended = true;
                path.recv_busy().map_err(|_| "path count overflow")?;
                Ok(Step::Pending)
            }
            Err(evering::ReceiveError::Closed) => Err("sender disconnected".into()),
        }
    }

    fn wait(
        &mut self,
        interest: Interest,
        deadline: Deadline,
        path: &mut PathCounts,
    ) -> Result<(), String> {
        // Deliberately not `Signals::reserve_with`: the library loop runs one direction
        // to commit, while this side owns both directions of the duplex and must drain
        // responses between attempts. Looping here would put a task poll on every
        // attempt of the non-parking arm — the quantity under measurement — and would
        // stall the send until the response ring drains at a window above one. The
        // worker side, which serves one direction per iteration, does run the library
        // loop; this loop is shared with the framed control arms and stays as it is.
        if self.contended {
            self.contended = false;
            std::hint::spin_loop();
            return Ok(());
        }
        if !WAIT {
            std::thread::yield_now();
            return Ok(());
        }
        let spin =
            (interest.write && self.send_spins > 0) || (interest.read && self.recv_spins > 0);
        if spin {
            self.send_spins -= usize::from(interest.write && self.send_spins > 0);
            self.recv_spins -= usize::from(interest.read && self.recv_spins > 0);
            std::hint::spin_loop();
            return Ok(());
        }
        if self.woke && !self.progressed {
            path.stale_wake().map_err(|_| "path count overflow")?;
        }
        self.woke = false;
        self.progressed = false;
        path.wait().map_err(|_| "path count overflow")?;
        let timeout = deadline.remaining(Instant::now()).unwrap_or_default();
        let entered = self.local.runtime.enter();
        let future = tokio::time::timeout(timeout, self.local.wait.wait());
        drop(entered);
        match self.local.runtime.block_on(future) {
            Ok(Ok(())) => {
                path.wake().map_err(|_| "path count overflow")?;
                self.woke = true;
                Ok(())
            }
            Ok(Err(error)) => Err(text(error)),
            Err(_) => Err("timeout".into()),
        }
    }

    fn abort(&mut self) -> Result<(), String> {
        let staged = self.staged.take().map(|value| (self.discard)(value));
        self.local.close::<WAIT, _>(&self.tx)?;
        staged.transpose()?;
        Ok(())
    }
}

async fn serve<const SPINS: usize, const WAIT: bool>(
    session: Session,
    port: Port<Envelope>,
    pool: PoolId,
    local: Local<'_>,
    timeout: Duration,
    trailer: &str,
) -> Result<(), String> {
    let pool = session
        .open_pool(pool)
        .map_err(|_| "worker could not acquire Pool")?;
    let channel = session
        .adopt(port)
        .map_err(|error| format!("worker could not adopt channel: {error:?}"))?;
    let (tx, rx) = channel.split();
    let mut phase: Option<boundary::Phase> = None;
    let served = 'rounds: loop {
        let deadline = Deadline::after(Instant::now(), timeout)?;
        let (mut header, value) =
            match retry_adopt::<SPINS, WAIT, Envelope, [u8]>(&rx, pool.as_ref(), local, deadline)
                .await
            {
                Ok(value) => value,
                Err(error) if error == "sender disconnected" => {
                    if let Err(error) = local.close::<WAIT, _>(&tx) {
                        break 'rounds Err(error);
                    }
                    break 'rounds Ok(());
                }
                Err(error) => break 'rounds Err(error),
            };
        let mut ready = false;
        match header.kind {
            DATA => header.digest = model::digest(&value),
            READY if value.is_empty() => {
                header.digest = 0;
                ready = true;
            }
            _ => break 'rounds Err("worker rejected message envelope".into()),
        }
        let record = value.transfer(header);
        if let Err(error) = retry_send::<SPINS, WAIT, _>(&tx, local, deadline, record).await {
            break 'rounds Err(error);
        }
        // The READY frame is the probe that closes setup, so bracketing from it
        // keeps warmup out of the worker figure and off the accounting path.
        if ready && phase.is_none() {
            phase = boundary::Phase::begin(trailer);
        }
    };
    if let Some(phase) = phase {
        phase.record(trailer)?;
    }
    served
}

type Worker = (
    Session,
    Port<Envelope>,
    PoolId,
    evering::os::Ring,
    evering::runtime::Wait,
);

#[cfg(unix)]
fn open_worker(address: &str) -> Result<Worker, String> {
    use evering::os::unix::{UnixFd, process::Socket};

    let socket = Socket::bind(address).map_err(text)?;
    let (bootstrap, resources) = socket.recv(3).map_err(text)?.into_parts();
    let (port, pool, extent) = parse(bootstrap.as_ref())?;
    let mut resources = resources.into_vec();
    let source = UnixFd::from_fd(resources.remove(0)).map_err(text)?;
    let parent_event = evering::os::Event::from_owned_fd(resources.remove(0));
    let child_ring = evering::os::Ring::from_owned_fd(resources.remove(0));
    let session = open_session(source, extent)?;
    let wait = evering::runtime::Wait::new(parent_event).map_err(text)?;
    Ok((session, port, pool, child_ring, wait))
}

#[cfg(windows)]
fn open_worker(address: &str) -> Result<Worker, String> {
    use evering::os::windows::{Section, process::Socket};

    let socket = Socket::connect(address).map_err(text)?;
    let (bootstrap, resources) = socket.recv(3).map_err(text)?.into_parts();
    let (port, pool, extent) = parse(bootstrap.as_ref())?;
    let mut resources = resources.into_vec();
    let source = Section::from_owned_handle(resources.remove(0));
    let parent_event = evering::os::Event::from_owned_handle(resources.remove(0));
    let child_ring = evering::os::Ring::from_owned_handle(resources.remove(0));
    let session = open_session(source, extent)?;
    let wait = evering::runtime::Wait::new(parent_event).map_err(text)?;
    Ok((session, port, pool, child_ring, wait))
}

fn worker_with<const SPINS: usize, const WAIT: bool>(
    address: &str,
    timeout: Duration,
    trailer: &str,
) -> Result<(), String> {
    let runtime = runtime()?;
    let entered = runtime.enter();
    let (session, port, pool, ring, wait) = open_worker(address)?;
    drop(entered);
    runtime.block_on(serve::<SPINS, WAIT>(
        session,
        port,
        pool,
        Local {
            ring: &ring,
            wait: &wait,
            runtime: &runtime,
        },
        timeout,
        trailer,
    ))
}

pub fn worker(
    address: &str,
    policy: &str,
    timeout: Duration,
    expected_environment: &str,
    trailer: &str,
) -> Result<(), String> {
    environment::admit(expected_environment)?;
    match policy {
        "busy" => worker_with::<{ usize::MAX }, false>(address, timeout, trailer),
        "adaptive" => worker_with::<ADAPTIVE_SPINS, true>(address, timeout, trailer),
        "notified" => worker_with::<0, true>(address, timeout, trailer),
        _ => Err("unknown Evering retry policy".into()),
    }
}

struct Setup {
    session: Session,
    channel: Channel<Envelope>,
    pool: Pool,
    child: Supervisor,
    runtime: tokio::runtime::Runtime,
    ring: evering::os::Ring,
    wait: evering::runtime::Wait,
    trailer: Option<Trailer>,
    #[cfg(unix)]
    _socket_path: SocketPath,
}

#[cfg(unix)]
struct SocketPath(std::path::PathBuf);

#[cfg(unix)]
impl Drop for SocketPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(unix)]
fn setup(
    extent: usize,
    capacity: usize,
    deadline: Deadline,
    policy: &str,
    environment: &str,
) -> Result<Setup, String> {
    use evering::os::unix::{UnixFd, process::Socket};
    use std::os::fd::AsFd;

    let runtime = runtime()?;
    let entered = runtime.enter();
    let source = UnixFd::memfd("evering-bench", extent, false).map_err(text)?;
    let session = create_session(source.borrow(), extent)?;
    let (channel, port) = session
        .create_channel::<Envelope>(capacity)
        .map_err(|error| format!("could not create channel: {error:?}"))?;
    let pool = session
        .create_pool(POOL_EXTENT, Some(pool_range()))
        .map_err(|error| format!("could not create Pool: {error:?}"))?;
    let pool_id = pool.id();
    let path = std::env::temp_dir().join(format!(
        "evering-bench-{}-{}.sock",
        std::process::id(),
        NEXT_SOCKET.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut command = Command::new(std::env::current_exe().map_err(text)?);
    let timeout = deadline.remaining(Instant::now())?.as_millis().to_string();
    let trailer = Trailer::claim()?;
    command.args([
        "worker-evering",
        path.to_string_lossy().as_ref(),
        policy,
        &timeout,
        environment,
        trailer
            .as_ref()
            .map_or(boundary::ABSENT, |trailer| trailer.argument()),
    ]);
    let mut child = Supervisor::spawn(&mut command).map_err(text)?;
    let socket = loop {
        match Socket::connect(&path) {
            Ok(socket) => break socket,
            Err(_) if deadline.remaining(Instant::now()).is_ok() => {
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
            &bootstrap(&port, pool_id, extent)?,
            &[source.as_fd(), parent_event.as_fd(), child_ring.as_fd()],
        )
        .map_err(text)?;
    let wait = evering::runtime::Wait::new(child_event).map_err(text)?;
    drop(entered);
    Ok(Setup {
        session,
        channel,
        pool,
        child,
        runtime,
        ring: parent_ring,
        wait,
        trailer,
        _socket_path: SocketPath(path),
    })
}

#[cfg(windows)]
fn setup(
    extent: usize,
    capacity: usize,
    deadline: Deadline,
    policy: &str,
    environment: &str,
) -> Result<Setup, String> {
    use std::os::windows::io::AsHandle;

    use evering::os::windows::{Section, process::Listener};

    let runtime = runtime()?;
    let entered = runtime.enter();
    let listener = Listener::bind().map_err(text)?;
    let mut command = Command::new(std::env::current_exe().map_err(text)?);
    let timeout = deadline.remaining(Instant::now())?.as_millis().to_string();
    let trailer = Trailer::claim()?;
    command.args([
        "worker-evering",
        listener.name(),
        policy,
        &timeout,
        environment,
        trailer
            .as_ref()
            .map_or(boundary::ABSENT, |trailer| trailer.argument()),
    ]);
    let child = Supervisor::spawn(&mut command).map_err(text)?;
    let now = Instant::now();
    let socket = listener
        .accept(&child, now + deadline.remaining(now)?)
        .map_err(text)?;
    let source = Section::anonymous(extent, Access::READ | Access::WRITE).map_err(text)?;
    let session = create_session(source.borrow(), extent)?;
    let (channel, port) = session
        .create_channel::<Envelope>(capacity)
        .map_err(|error| format!("could not create channel: {error:?}"))?;
    let pool = session
        .create_pool(POOL_EXTENT, Some(pool_range()))
        .map_err(|error| format!("could not create Pool: {error:?}"))?;
    let pool_id = pool.id();
    let (parent_ring, parent_event) = evering::os::event().map_err(text)?;
    let (child_ring, child_event) = evering::os::event().map_err(text)?;
    socket
        .send(
            &child,
            &bootstrap(&port, pool_id, extent)?,
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
        channel,
        pool,
        child,
        runtime,
        ring: parent_ring,
        wait,
        trailer,
    })
}

fn run_with<const SPINS: usize, const WAIT: bool>(
    policy: &str,
    cell: &Cell,
    requested: u64,
    warmup: u64,
    seed: u64,
    deadline: Deadline,
    environment: &str,
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
    let setup = setup(extent, capacity, deadline, policy, environment);
    let mut setup = setup.map_err(|error| fail(Status::SetupError, error, None))?;
    let (tx, rx) = setup.channel.split();
    let local = Local {
        ring: &setup.ring,
        wait: &setup.wait,
        runtime: &setup.runtime,
    };
    let geometry = pool_geometry(&setup.pool);
    let mut counts = Counts {
        observed: Some(Observed {
            payload: cell.payload,
            capacity: cell.capacity,
            in_flight: cell.in_flight,
            window: window(requested, cell.capacity, cell.in_flight),
            topology: "1c1w".into(),
            transport: "shared-memory".into(),
            extent: Some(cell.memory),
            allocator: Some(geometry),
            socket_send: None,
            socket_recv: None,
        }),
        ..Counts::default()
    };
    let pool = setup.pool.as_ref();
    let make = move |operation, payload: &[u8]| {
        pool.copy(payload)
            .map(|block| {
                block.transfer(Envelope {
                    kind: if operation == u64::MAX { READY } else { DATA },
                    operation,
                    digest: 0,
                })
            })
            .map_err(|error| format!("allocate request: {error:?}"))
    };
    let open = move |(header, value): (Envelope, evering::Block<'_, [u8]>), expected: Expected| {
        let kind = if header.operation == u64::MAX {
            READY
        } else {
            DATA
        };
        Ok(header.kind == kind
            && expected.matches_digest(header.operation, header.digest, value.len()))
    };
    let discard = move |record| {
        drop(record);
        Ok(())
    };
    let mut client = Client::<_, _, _, SPINS, WAIT>::new(tx, rx, make, open, discard, pool, local);
    let window = cell.capacity.min(cell.in_flight);
    measured(
        &mut counts,
        drive::measure(
            &mut client,
            drive::Work {
                start: 0,
                count: requested,
                window,
                payload: payload_len,
                seed,
            },
            warmup,
            began,
            deadline,
        ),
    )?;
    client
        .close()
        .map_err(|error| fail(Status::DrainError, error, Some(&counts)))?;
    drop(client);
    let drain = Instant::now();
    let exit = wait_child(&mut setup.child, deadline)
        .map_err(|error| fail(Status::DrainError, error, Some(&counts)))?;
    if !exit {
        return Err(fail(Status::DrainError, "exit", Some(&counts)));
    }
    setup
        .session
        .remove(setup.channel)
        .map_err(|_| fail(Status::DrainError, "channel remained busy", Some(&counts)))?;
    counts.phase_ns[2] = drain.elapsed().as_nanos().max(1) as u64;
    counts.worker = setup.trailer.as_ref().and_then(|trailer| trailer.read());
    Ok(counts)
}

pub fn busy(
    cell: &Cell,
    requested: u64,
    warmup: u64,
    seed: u64,
    deadline: Deadline,
    environment: &str,
) -> Result<Counts, RunError> {
    run_with::<{ usize::MAX }, false>("busy", cell, requested, warmup, seed, deadline, environment)
}

pub fn adaptive(
    cell: &Cell,
    requested: u64,
    warmup: u64,
    seed: u64,
    deadline: Deadline,
    environment: &str,
) -> Result<Counts, RunError> {
    run_with::<ADAPTIVE_SPINS, true>(
        "adaptive",
        cell,
        requested,
        warmup,
        seed,
        deadline,
        environment,
    )
}

pub fn notified(
    cell: &Cell,
    requested: u64,
    warmup: u64,
    seed: u64,
    deadline: Deadline,
    environment: &str,
) -> Result<Counts, RunError> {
    run_with::<0, true>(
        "notified",
        cell,
        requested,
        warmup,
        seed,
        deadline,
        environment,
    )
}
