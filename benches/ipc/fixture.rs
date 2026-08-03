use std::{num::NonZeroU64, path::Path};

use evering::{
    Pool, Rx, Session, Tx,
    layout::{RegionId, Repr, SchemaId, SchemaKey},
    mapping::{Access, Request, Source},
    notify::{Notify as _, Wait as _},
};
use serde::de::DeserializeOwned;

use super::{
    analysis::{self, MechanismAnalysis},
    environment,
    mechanism::{self, Body, Fixture, FixtureSchema},
    study::{Header, Identity, Run},
    system,
};

const REGION: RegionId = RegionId::new(0x6d65_6368_616e_6973, 1);
pub(crate) const KEYS: [&str; 6] = [
    "mechanism.queue.reserve-publish",
    "mechanism.queue.claim-recycle",
    "mechanism.pool.allocate-release",
    "mechanism.talc.allocate-release",
    "mechanism.notify",
    "mechanism.signal-wait",
];

#[repr(C)]
#[derive(Clone, Copy)]
struct Record(u64);

unsafe impl Repr for Record {
    const SCHEMA: SchemaKey = SchemaKey::new(SchemaId(0x6d65_6368_2d72_6563), 1);
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Memory {
    pub extent: u64,
    pub storage: u64,
    pub placement: String,
}

impl Identity for Memory {
    fn identity(&self, hash: &mut blake3::Hasher) {
        self.extent.identity(hash);
        self.storage.identity(hash);
        self.placement.identity(hash);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Allocation {
    pub bytes: u64,
    pub alignment: u64,
    pub occupancy: u64,
    pub touch: bool,
}

impl Identity for Allocation {
    fn identity(&self, hash: &mut blake3::Hasher) {
        self.bytes.identity(hash);
        self.alignment.identity(hash);
        self.occupancy.identity(hash);
        hash.update(&[self.touch as u8]);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct QueueParameters {
    pub extent: u64,
    pub storage: u64,
    pub capacity: u64,
}

impl Identity for QueueParameters {
    fn identity(&self, hash: &mut blake3::Hasher) {
        self.extent.identity(hash);
        self.storage.identity(hash);
        self.capacity.identity(hash);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct QueueCase {
    pub payload: u64,
}

impl Identity for QueueCase {
    fn identity(&self, hash: &mut blake3::Hasher) {
        self.payload.identity(hash);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct SignalParameters {
    pub limit: u64,
}

impl Identity for SignalParameters {
    fn identity(&self, hash: &mut blake3::Hasher) {
        self.limit.identity(hash);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct SignalCase {
    pub sticky: bool,
}

impl Identity for SignalCase {
    fn identity(&self, hash: &mut blake3::Hasher) {
        hash.update(&[self.sticky as u8]);
    }
}

#[derive(serde::Deserialize)]
struct Probe {
    fixture: String,
}

#[derive(serde::Deserialize)]
struct Input<P, C> {
    fixture: String,
    targets: Vec<String>,
    parameters: P,
    cases: Vec<C>,
    #[serde(default)]
    policy: mechanism::Policy,
    alpha: f64,
    delta_ns: f64,
    system_delta: f64,
    seed: u64,
    budget_ms: u64,
}

fn extent(value: u64) -> Result<usize, String> {
    usize::try_from(value).map_err(|_| "fixture extent exceeds native address space".into())
}

fn create<S: Source>(source: S, bytes: usize) -> Result<Session, String>
where
    S::Error: core::fmt::Debug,
{
    Session::create(
        source,
        Request::new(bytes, Access::READ | Access::WRITE),
        REGION,
    )
    .map_err(|error| format!("{error:?}"))
}

fn open_pair<A: Source, B: Source>(
    create_source: A,
    open_source: B,
    bytes: usize,
) -> Result<(Session, Session), String>
where
    A::Error: core::fmt::Debug,
    B::Error: core::fmt::Debug,
{
    let request = Request::new(bytes, Access::READ | Access::WRITE);
    Ok((
        Session::create(create_source, request, REGION).map_err(|error| format!("{error:?}"))?,
        Session::open(open_source, request, REGION).map_err(|error| format!("{error:?}"))?,
    ))
}

#[cfg(unix)]
fn session(bytes: usize) -> Result<Session, String> {
    let source = evering::os::unix::UnixFd::memfd("evering-mechanism", bytes, false)
        .map_err(|error| error.to_string())?;
    create(source.borrow(), bytes)
}

#[cfg(windows)]
fn session(bytes: usize) -> Result<Session, String> {
    let source = evering::os::windows::Section::anonymous(bytes, Access::READ | Access::WRITE)
        .map_err(|error| error.to_string())?;
    create(source.borrow(), bytes)
}

#[cfg(unix)]
fn session_pair(bytes: usize) -> Result<(Session, Session), String> {
    let source = evering::os::unix::UnixFd::memfd("evering-mechanism-pair", bytes, false)
        .map_err(|error| error.to_string())?;
    open_pair(source.borrow(), source.borrow(), bytes)
}

#[cfg(windows)]
fn session_pair(bytes: usize) -> Result<(Session, Session), String> {
    let source = evering::os::windows::Section::anonymous(bytes, Access::READ | Access::WRITE)
        .map_err(|error| error.to_string())?;
    open_pair(source.borrow(), source.borrow(), bytes)
}

struct QueueFixture<const CLAIM: bool> {
    _session: Session,
    _peer: Session,
    tx: Tx<Record>,
    rx: Rx<Record>,
    pool: Pool,
    peer_pool: Pool,
    capacity: NonZeroU64,
    pending: u64,
}

impl<const CLAIM: bool> QueueFixture<CLAIM> {
    fn send(&mut self, count: u64) -> Result<(), String> {
        let pool = self.pool.as_ref();
        for value in 0..count {
            self.tx
                .try_send(
                    pool.copy::<u8>(&[])
                        .map_err(|error| format!("{error:?}"))?
                        .transfer(Record(value)),
                )
                .map_err(|_| "queue fixture send failed".to_owned())?;
            self.pending += 1;
        }
        Ok(())
    }

    fn drain(&mut self, count: u64) -> Result<(), String> {
        let pool = self.peer_pool.as_ref();
        for _ in 0..count {
            self.rx
                .claim()
                .map_err(|error| format!("{error:?}"))?
                .discard(pool)
                .map_err(|error| format!("{error:?}"))?;
            self.pending -= 1;
        }
        Ok(())
    }
}

impl<const CLAIM: bool> Fixture for QueueFixture<CLAIM> {
    type Parameters = QueueParameters;
    type Case = QueueCase;

    fn prepare(parameters: &QueueParameters, case: &QueueCase) -> Result<Self, String> {
        if case.payload != 0 {
            return Err("queue mechanism currently requires an empty Pool payload".into());
        }
        let (session, peer) = session_pair(extent(parameters.extent)?)?;
        let (left, port) = session
            .create_channel::<Record>(extent(parameters.capacity)?)
            .map_err(|error| format!("{error:?}"))?;
        let right = peer.adopt(port).map_err(|error| format!("{error:?}"))?;
        let (tx, _) = left.split();
        let (_, rx) = right.split();
        let pool = session
            .create_pool(extent(parameters.storage)?, None)
            .map_err(|error| format!("{error:?}"))?;
        let peer_pool = peer
            .open_pool(pool.id())
            .map_err(|error| format!("{error:?}"))?;
        Ok(Self {
            _session: session,
            _peer: peer,
            tx,
            rx,
            pool,
            peer_pool,
            capacity: NonZeroU64::new(parameters.capacity).ok_or("zero queue capacity")?,
            pending: 0,
        })
    }

    fn state(&self) -> Result<String, String> {
        Ok(self.pending.to_string())
    }

    fn limit(&self) -> NonZeroU64 {
        self.capacity
    }

    fn setup(&mut self, operations: u64, _: Body) -> Result<(), String> {
        if self.pending != 0 {
            return Err("queue setup observed pending transfers".into());
        }
        if CLAIM {
            self.send(operations)?;
        }
        Ok(())
    }

    fn gross(&mut self, operations: u64) -> Result<(), String> {
        if CLAIM {
            self.drain(operations)
        } else {
            self.send(operations)
        }
    }

    fn control(&mut self, operations: u64) -> Result<(), String> {
        let pool = self.pool.as_ref();
        for value in 0..operations {
            std::hint::black_box(
                pool.copy::<u8>(&[])
                    .map_err(|error| format!("{error:?}"))?
                    .transfer(Record(value)),
            );
        }
        Ok(())
    }

    fn reset(&mut self) -> Result<(), String> {
        self.drain(self.pending)
    }
}

struct AllocationFixture<const POOL: bool> {
    session: Session,
    pool: Option<Pool>,
    bytes: Vec<u8>,
    scratch: Vec<u8>,
}

impl<const POOL: bool> Fixture for AllocationFixture<POOL> {
    type Parameters = Memory;
    type Case = Allocation;

    fn prepare(parameters: &Memory, case: &Allocation) -> Result<Self, String> {
        if parameters.storage == 0
            || parameters.storage > parameters.extent
            || parameters.placement != "shared-mapping"
            || case.alignment != 1
            || case.occupancy != 0
        {
            return Err("unsupported allocation placement, alignment, or occupancy".into());
        }
        let session = session(extent(parameters.extent)?)?;
        let pool = if POOL {
            Some(
                session
                    .create_pool(extent(parameters.storage)?, None)
                    .map_err(|error| format!("{error:?}"))?,
            )
        } else {
            None
        };
        let bytes = extent(case.bytes)?;
        if bytes == 0 || !case.touch {
            return Err("allocation fixture requires nonzero touched bytes".into());
        }
        Ok(Self {
            session,
            pool,
            bytes: vec![0; bytes],
            scratch: vec![0; bytes],
        })
    }

    fn state(&self) -> Result<String, String> {
        Ok("released".into())
    }
    fn limit(&self) -> NonZeroU64 {
        NonZeroU64::MAX
    }
    fn setup(&mut self, _: u64, _: Body) -> Result<(), String> {
        Ok(())
    }

    fn gross(&mut self, operations: u64) -> Result<(), String> {
        for _ in 0..operations {
            if let Some(pool) = &self.pool {
                drop(
                    pool.as_ref()
                        .copy(&self.bytes)
                        .map_err(|error| format!("{error:?}"))?,
                );
            } else {
                drop(
                    self.session
                        .heap()
                        .copy(&self.bytes)
                        .map_err(|error| format!("{error:?}"))?,
                );
            }
        }
        Ok(())
    }

    fn control(&mut self, operations: u64) -> Result<(), String> {
        for _ in 0..operations {
            self.scratch.copy_from_slice(&self.bytes);
            std::hint::black_box(self.scratch.as_ptr());
        }
        Ok(())
    }

    fn reset(&mut self) -> Result<(), String> {
        Ok(())
    }
}

struct SignalFixture<const WAIT: bool> {
    runtime: tokio::runtime::Runtime,
    ring: evering::os::Ring,
    wait: evering::runtime::Wait,
    limit: NonZeroU64,
    pending: bool,
}

impl<const WAIT: bool> Fixture for SignalFixture<WAIT> {
    type Parameters = SignalParameters;
    type Case = SignalCase;

    fn prepare(parameters: &SignalParameters, case: &SignalCase) -> Result<Self, String> {
        if !case.sticky {
            return Err("signal fixture requires sticky notification semantics".into());
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        let (ring, event) = evering::os::event().map_err(|error| error.to_string())?;
        let wait = {
            let _entered = runtime.enter();
            evering::runtime::Wait::new(event).map_err(|error| error.to_string())?
        };
        Ok(Self {
            runtime,
            ring,
            wait,
            limit: NonZeroU64::new(parameters.limit).ok_or("zero signal batch limit")?,
            pending: false,
        })
    }

    fn state(&self) -> Result<String, String> {
        Ok(self.pending.to_string())
    }
    fn limit(&self) -> NonZeroU64 {
        self.limit
    }
    fn setup(&mut self, _: u64, _: Body) -> Result<(), String> {
        Ok(())
    }

    fn gross(&mut self, operations: u64) -> Result<(), String> {
        for _ in 0..operations {
            self.ring.notify().map_err(|error| error.to_string())?;
            self.pending = true;
            if WAIT {
                self.runtime
                    .block_on(self.wait.wait())
                    .map_err(|error| error.to_string())?;
                self.pending = false;
            }
        }
        Ok(())
    }

    fn control(&mut self, operations: u64) -> Result<(), String> {
        for value in 0..operations {
            if WAIT {
                self.ring.notify().map_err(|error| error.to_string())?;
                self.pending = true;
            } else {
                std::hint::black_box(value);
            }
        }
        Ok(())
    }

    fn reset(&mut self) -> Result<(), String> {
        if self.pending {
            self.runtime
                .block_on(self.wait.wait())
                .map_err(|error| error.to_string())?;
            self.pending = false;
        }
        Ok(())
    }
}

struct QueueTag<const CLAIM: bool>;
struct AllocationTag<const POOL: bool>;
struct SignalTag<const WAIT: bool>;

impl<const CLAIM: bool> FixtureSchema for QueueTag<CLAIM> {
    const KEY: &'static str = KEYS[CLAIM as usize];
    type Parameters = QueueParameters;
    type Case = QueueCase;
    type Fixture = QueueFixture<CLAIM>;

    fn matches(parameters: &QueueParameters, _: &QueueCase, target: &system::Case) -> bool {
        parameters.extent == target.workload.memory
            && parameters.capacity == target.workload.capacity
            && target.resources.transport == "shared-memory"
    }
}
impl<const POOL: bool> FixtureSchema for AllocationTag<POOL> {
    const KEY: &'static str = KEYS[3 - POOL as usize];
    type Parameters = Memory;
    type Case = Allocation;
    type Fixture = AllocationFixture<POOL>;

    fn matches(parameters: &Memory, case: &Allocation, target: &system::Case) -> bool {
        parameters.extent == target.workload.memory
            && case.bytes == target.workload.payload
            && target.resources.allocator
    }
}
impl<const WAIT: bool> FixtureSchema for SignalTag<WAIT> {
    const KEY: &'static str = KEYS[4 + WAIT as usize];
    type Parameters = SignalParameters;
    type Case = SignalCase;
    type Fixture = SignalFixture<WAIT>;

    fn matches(_: &SignalParameters, _: &SignalCase, target: &system::Case) -> bool {
        target.resources.transport == "shared-memory"
    }
}

fn run<F: FixtureSchema>(encoded: &str, output: &Path) -> Result<(), String>
where
    Input<F::Parameters, F::Case>: DeserializeOwned,
{
    let input: Input<F::Parameters, F::Case> =
        serde_json::from_str(encoded).map_err(|error| error.to_string())?;
    if input.fixture != F::KEY {
        return Err("fixture key changed during typed admission".into());
    }
    let environment = environment::capture()?;
    let capture = environment::metadata(&environment)?;
    let (schedule, orders) = mechanism::schedule(input.seed, input.cases.len(), input.policy.pairs);
    let header = Header::new::<mechanism::Mechanism<F>>(
        capture.context,
        mechanism::Specification {
            targets: input.targets,
            parameters: input.parameters,
            policy: input.policy,
            alpha: input.alpha,
            delta_ns: input.delta_ns,
            system_delta: input.system_delta,
            orders,
        },
        input.cases,
        Run {
            seed: input.seed,
            started: capture.started,
            command: capture.command,
            budget_ms: input.budget_ms,
            schedule,
            ..Run::default()
        },
    );
    mechanism::record::<F>(output, header).map(drop)
}

pub fn record(specification: &Path, output: &Path) -> Result<(), String> {
    let encoded = std::fs::read_to_string(specification).map_err(|error| error.to_string())?;
    let probe: Probe = serde_json::from_str(&encoded).map_err(|error| error.to_string())?;
    match probe.fixture.as_str() {
        "mechanism.queue.reserve-publish" => run::<QueueTag<false>>(&encoded, output),
        "mechanism.queue.claim-recycle" => run::<QueueTag<true>>(&encoded, output),
        "mechanism.pool.allocate-release" => run::<AllocationTag<true>>(&encoded, output),
        "mechanism.talc.allocate-release" => run::<AllocationTag<false>>(&encoded, output),
        "mechanism.notify" => run::<SignalTag<false>>(&encoded, output),
        "mechanism.signal-wait" => run::<SignalTag<true>>(&encoded, output),
        key => Err(format!("unknown mechanism fixture: {key}")),
    }
}

fn analyze_as<F: FixtureSchema>(
    encoded: &str,
    systems: &[system::Evidence],
) -> Result<MechanismAnalysis, String> {
    let evidence = super::study::load_str::<mechanism::Mechanism<F>>(encoded)
        .map_err(|error| format!("{error:?}"))?;
    analysis::analyze_mechanism::<F>(&evidence, systems).map_err(|error| format!("{error:?}"))
}

pub fn analyze_str(
    schema: &super::study::SchemaId,
    encoded: &str,
    systems: &[system::Evidence],
) -> Result<MechanismAnalysis, String> {
    match schema.name.as_str() {
        "mechanism.queue.reserve-publish" => analyze_as::<QueueTag<false>>(encoded, systems),
        "mechanism.queue.claim-recycle" => analyze_as::<QueueTag<true>>(encoded, systems),
        "mechanism.pool.allocate-release" => analyze_as::<AllocationTag<true>>(encoded, systems),
        "mechanism.talc.allocate-release" => analyze_as::<AllocationTag<false>>(encoded, systems),
        "mechanism.notify" => analyze_as::<SignalTag<false>>(encoded, systems),
        "mechanism.signal-wait" => analyze_as::<SignalTag<true>>(encoded, systems),
        key => Err(format!("unknown mechanism evidence: {key}")),
    }
}
