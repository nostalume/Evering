#![cfg(all(unix, feature = "map"))]
#![expect(
    clippy::result_large_err,
    reason = "uncommitted transfers stay inline so retry retains exact ownership"
)]

//! What a second process can ask of the layout and recovery layers: that a mapping joined at a
//! different base still reads the same directory, that a region whose root names another region is
//! refused at the root, that a read-only attachment is refused, and that session attachment is not
//! secretly coupled to how a queue is counted, typed or revised — each of those answered by a peer
//! that read the numbers out of its own environment rather than by a caller who could have arranged
//! them. It also asks that geometry and schema information from a different build be refused rather
//! than trusted, that a member which died mid-queue is reaped only by a handler naming its schema, and
//! that after a complete reap its slot is reusable, its generation is not, and its stale identity
//! targets nothing.
//!
//! The two recovery cases have a Windows leg, in `tests/recovery_dispatch_windows.rs`; that file says
//! which faces of the dispatch were already executed there and what the transport looks like without a
//! POSIX shared-memory name. The rest of this file is a unix-only claim and is not ported: several of
//! these cases assert where a mapping landed, and the Windows backend does not ask the kernel for an
//! address at all, so there is no alternate base to join at and nothing for such an assertion to
//! answer to.
//!
//! Two seams here differ from every other multi-process claim, and both follow from the transport being
//! the environment rather than a socket. The numbers of a channel entry, a port and a pool travel one
//! key per number, spelled out by [`declare_port`]/[`declare_pool`] and read back by
//! [`env_port`]/[`pool_id_from_env`], because there is no handover to carry them as bytes; and a role
//! that has to report what only it can see — the member it became, the claims it holds — writes a file
//! the parent reads after the exit, because a re-entered child's harness output goes to the void and a
//! number printed there would be read by no one. The verdict each doomed role leaves with is handed to
//! it as a number ([`cuts`]) rather than fixed in the arm that dies, so one number has one spelling
//! here; [`AlternateEnvelope`]'s schema, by contrast, is the same words on both hosts by design, since
//! it is the layout's own claim and not a process seam.

use std::{
    env, fs,
    path::PathBuf,
    process::{Child, Command, ExitStatus},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(feature = "process")]
use evering::process::Supervisor;
use evering::{
    BlockRange, ChannelId as Id, DeclineCause, PoolId, Port, RoomWait, Session, SessionError,
    layout::{self, RegionId, Repr, SchemaId, SchemaKey, SharedSchema},
    mapping::{Access, LayoutField, Peer, Request},
    os::unix::UnixFd,
};

mod support;
use crate::support::{cuts, introduce, region, role};

const NAME: &str = "EVERING_PROCESS_SHM";
const READY: &str = "EVERING_PROCESS_READY";
const ENTRY_SLAB: &str = "EVERING_PROCESS_ENTRY_SLAB";
const ENTRY_INDEX: &str = "EVERING_PROCESS_ENTRY_INDEX";
const ENTRY_GENERATION: &str = "EVERING_PROCESS_ENTRY_GENERATION";
const ENTRY_CAPACITY: &str = "EVERING_PROCESS_ENTRY_CAPACITY";
const PORT_ROLE: &str = "EVERING_PROCESS_PORT_ROLE";
const PORT_GENERATION: &str = "EVERING_PROCESS_PORT_GENERATION";
const POOL_SLAB: &str = "EVERING_PROCESS_POOL_SLAB";
const POOL_INDEX: &str = "EVERING_PROCESS_POOL_INDEX";
const POOL_GENERATION: &str = "EVERING_PROCESS_POOL_GENERATION";
const PARENT_BASE: &str = "EVERING_PROCESS_PARENT_BASE";
const REGION_SIZE: usize = 4 * 1024 * 1024;
const QUEUE_CAPACITY: usize = 8;
const REGION_ID: RegionId = RegionId::new(0x4556_4552_494e_4701, 1);
const TIMEOUT: Duration = Duration::from_secs(5);

/// The doomed member of the layout scan: it publishes its own identity and leaves without releasing it.
const DOOMED: i32 = 77;
/// The member of a schema no default handler names: it reports itself and leaves the same way.
const FOREIGN: i32 = 73;
/// The pool filler: it exhausts the pool with claims it never releases, and leaves.
const FILLED: i32 = 74;

type UnixSession = Session;

#[repr(transparent)]
struct AlternateEnvelope(u64);

impl SharedSchema for AlternateEnvelope {
    const SCHEMA: SchemaKey = SchemaKey::new(SchemaId(0x414c_5445_524e_4154), 1);
}

unsafe impl Repr for AlternateEnvelope {
    const SCHEMA: SchemaKey = <Self as SharedSchema>::SCHEMA;
}

struct ChildGuard(Child);

impl ChildGuard {
    fn wait(&mut self) -> ExitStatus {
        self.0.wait().expect("wait for child")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct ShmGuard(String);

impl Drop for ShmGuard {
    fn drop(&mut self) {
        let _ = UnixFd::shm_unlink(&self.0);
    }
}

fn unique_name() -> String {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    format!("evering-process-{}-{nonce}", std::process::id())
}

/// Where a role reports what only it can see, named per case so two roles cannot write one file.
fn ready_path(case: &str) -> PathBuf {
    env::temp_dir().join(format!(
        "evering-process-ready-{}-{case}",
        std::process::id()
    ))
}

/// The child half of a case that only asserts from inside the role: meet in the named region, run, and
/// be waited for.
fn joins_as(role_name: &str, name: &str) -> ChildGuard {
    let mut command = role::child(role_name);
    command.env(NAME, name);
    ChildGuard(command.spawn().expect("spawn the joining process"))
}

fn creator_session(name: &str) -> UnixSession {
    let fd = UnixFd::shm_create(name, REGION_SIZE).expect("create shared region");
    Session::create(fd, region::extent(REGION_SIZE), REGION_ID).expect("create session")
}

fn joiner_session(name: &str, parent_base: usize) -> UnixSession {
    let fd = UnixFd::shm_open(name).expect("open shared region");
    Session::open(
        fd.mapping().at(parent_base.wrapping_add(1 << 30)),
        region::extent(REGION_SIZE),
        REGION_ID,
    )
    .expect("join session")
}

/// Spells one channel endpoint out, one number per key, for a role that can only be told by name.
fn declare_port<T: Repr>(command: &mut Command, port: &Port<T>) {
    let id = port.id();
    command
        .env(ENTRY_SLAB, id.slab().to_string())
        .env(ENTRY_INDEX, id.entry().to_string())
        .env(ENTRY_GENERATION, id.generation().to_string())
        .env(ENTRY_CAPACITY, id.capacity().to_string())
        .env(PORT_ROLE, port.role().to_string())
        .env(PORT_GENERATION, port.generation().to_string());
}

/// Spells one pool out the same way.
fn declare_pool(command: &mut Command, pool: PoolId) {
    let (_, slab, entry, generation) = pool.parts();
    command
        .env(POOL_SLAB, slab.to_string())
        .env(POOL_INDEX, entry.to_string())
        .env(POOL_GENERATION, generation.to_string());
}

fn number(key: &str) -> usize {
    env::var(key)
        .expect(key)
        .parse()
        .expect("a numeric value beside the region name")
}

/// The region this role was pointed at, and the base its parent mapped it at: the alternate base a
/// joining peer reaches is `PARENT_BASE` plus a deliberate offset, never an address it chose.
fn shm_name() -> String {
    env::var(NAME).expect("shared region name")
}

fn parent_base() -> usize {
    number(PARENT_BASE)
}

fn env_port<T: Repr>() -> Port<T> {
    let id = Id::new(
        REGION_ID,
        number(ENTRY_SLAB) as u32,
        number(ENTRY_INDEX) as u32,
        number(ENTRY_GENERATION),
        number(ENTRY_CAPACITY),
    );
    Port::from_parts(id, number(PORT_ROLE) as u8, number(PORT_GENERATION)).expect("canonical Port")
}

fn pool_id_from_env() -> PoolId {
    PoolId::new(
        REGION_ID,
        number(POOL_SLAB) as u32,
        number(POOL_INDEX) as u32,
        number(POOL_GENERATION),
    )
}

/// The child's whole program: the roles this claim defines, and nothing else.
#[test]
fn role_entry() {
    role::dispatch(&[
        ("server", server),
        ("announce-peer-and-exit", announce_peer_and_exit),
        ("wrong-region", wrong_region),
        ("talc-geometry", talc_geometry),
        ("alternate-layout-owner", alternate_layout_owner),
        ("pool-owner", pool_owner),
    ]);
}

/// The answering peer of the round trip: join at a base it was handed, serve one request, and report
/// where its own allocator landed.
fn server() -> i32 {
    let session = joiner_session(&shm_name(), parent_base());
    let pool = session
        .open_pool(pool_id_from_env())
        .expect("acquire transfer Pool");
    let server_base = session.base_addr();
    let channel = session
        .adopt(env_port::<()>())
        .expect("adopt current channel role");
    let (send, recv) = channel.split();
    let deadline = Instant::now() + TIMEOUT;

    let (_, request) = introduce::adopt_bounded::<(), u64>(&recv, pool.as_ref(), deadline);
    let value = *request;
    drop(request);

    let response_data = [value.wrapping_mul(3), server_base as u64];
    let response = pool
        .as_ref()
        .copy(&response_data)
        .expect("allocate response");
    introduce::send_bounded(
        response.transfer(()),
        |value| send.try_send(value),
        deadline,
    );
    send.close();
    0
}

/// The doomed member: publish its own identity twice, then leave with the number the parent handed so
/// no destructor runs.
fn announce_peer_and_exit() -> i32 {
    let session = joiner_session(&shm_name(), parent_base());
    let channel = session
        .adopt(env_port::<()>())
        .expect("adopt current channel role");
    let (send, _) = channel.split();
    let deadline = Instant::now() + TIMEOUT;
    let peer = session.peer();
    let pool = session.open_pool(pool_id_from_env()).unwrap();
    for value in [peer.slot() as u64, peer.generation() as u64] {
        let record = pool.as_ref().put(value).expect("allocate identity");
        introduce::send_bounded(
            record.transfer(()),
            |record| send.try_send(record),
            deadline,
        );
    }
    role::leave(cuts::code())
}

/// The refusal a role expects, read as a pass: a peer that joined a root naming another region.
fn wrong_region() -> i32 {
    let wrong = RegionId::new(REGION_ID.high, REGION_ID.low.wrapping_add(1));
    let opened = Session::open(
        UnixFd::shm_open(&shm_name()).unwrap(),
        region::extent(REGION_SIZE),
        wrong,
    );
    assert!(matches!(
        opened,
        Err(SessionError::LayoutMismatch(LayoutField::Region))
    ));
    0
}

/// The same shape for geometry: a request narrower than the creating build's is refused by the
/// information that describes it.
fn talc_geometry() -> i32 {
    let opened = Session::open(
        UnixFd::shm_open(&shm_name()).unwrap(),
        region::extent(REGION_SIZE - 4096),
        REGION_ID,
    );
    assert!(matches!(
        opened,
        Err(SessionError::LayoutMismatch(LayoutField::Info))
    ));
    0
}

/// The foreign-schema member: adopt the queue no default handler names, report the member this process
/// became, and leave without unwinding it.
fn alternate_layout_owner() -> i32 {
    let session = joiner_session(&shm_name(), parent_base());
    let _channel = session.adopt(env_port::<AlternateEnvelope>()).unwrap();
    let peer = session.peer();
    fs::write(
        env::var_os(READY).expect("ready path"),
        format!("{},{}", peer.slot(), peer.generation()),
    )
    .expect("report the member this process became");
    role::leave(cuts::code())
}

/// The pool filler: exhaust the pool with claims this process never releases, report how many it holds,
/// and leave the same way.
fn pool_owner() -> i32 {
    let session = joiner_session(&shm_name(), parent_base());
    let peer = session.peer();
    let pool = session.open_pool(pool_id_from_env()).unwrap();
    let pool_ref = pool.as_ref();
    let mut blocks = Vec::new();
    let shortage = DeclineCause::NoRoom {
        waits: RoomWait::Release,
    };
    loop {
        match pool_ref.put(7_u64) {
            Ok(block) => blocks.push(block),
            Err(decline) if decline.cause == shortage => break,
            Err(decline) => panic!("unexpected Pool decline: {:?}", decline.cause),
        }
    }
    fs::write(
        env::var_os(READY).expect("ready path"),
        format!("{},{},{}", peer.slot(), peer.generation(), blocks.len()),
    )
    .expect("report the member this process became");
    role::leave(cuts::code())
}

#[test]
fn different_base_process_round_trip() {
    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let session = creator_session(&name);
    let parent_base = session.base_addr();
    let pool = session.create_pool(64 * 1024, None).unwrap();
    let (channel, port) = session
        .create_channel::<()>(QUEUE_CAPACITY)
        .expect("create channel");
    let (send, recv) = channel.split();

    let mut command = role::child("server");
    command
        .env(NAME, &name)
        .env(PARENT_BASE, parent_base.to_string());
    declare_port(&mut command, &port);
    declare_pool(&mut command, pool.id());
    let mut child = ChildGuard(command.spawn().expect("spawn server process"));

    let deadline = Instant::now() + TIMEOUT;
    let request = pool.as_ref().put(14_u64).expect("allocate request");
    introduce::send_bounded(request.transfer(()), |value| send.try_send(value), deadline);

    let (_, response) = introduce::adopt_bounded::<(), [u64]>(&recv, pool.as_ref(), deadline);
    assert_eq!(&*response, &[42, response[1]]);
    assert_ne!(
        response[1] as usize, parent_base,
        "the processes must use different allocator bases"
    );
    drop(response);
    send.close();

    assert!(child.wait().success(), "server process failed");
}

#[test]
fn dead_process_membership_is_reaped_after_a_complete_layout_scan() {
    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let session = creator_session(&name);
    let parent_base = session.base_addr();
    let pool = session.create_pool(64 * 1024, None).unwrap();
    let (channel, port) = session
        .create_channel::<()>(QUEUE_CAPACITY)
        .expect("create channel");
    let (_, recv) = channel.split();
    let mut command = role::child("announce-peer-and-exit");
    command
        .env(NAME, &name)
        .env(PARENT_BASE, parent_base.to_string());
    declare_port(&mut command, &port);
    declare_pool(&mut command, pool.id());
    cuts::hand(&mut command, DOOMED);
    #[cfg(feature = "process")]
    let mut child = Supervisor::spawn(&mut command).expect("spawn doomed peer");
    #[cfg(not(feature = "process"))]
    let mut child = ChildGuard(command.spawn().expect("spawn doomed peer"));

    #[cfg(feature = "process")]
    let exit = child.wait().expect("wait for child");
    #[cfg(feature = "process")]
    let status = exit.status();
    #[cfg(not(feature = "process"))]
    let status = child.wait();
    assert_eq!(
        status.code(),
        Some(DOOMED),
        "child must skip Rust destructors"
    );

    let deadline = Instant::now() + TIMEOUT;
    let mut identity = [0_u64; 2];
    for part in &mut identity {
        let (_, value) = introduce::adopt_bounded::<(), u64>(&recv, pool.as_ref(), deadline);
        *part = *value;
    }
    drop(recv);

    let peer = Peer::from_parts(identity[0] as u8, identity[1] as usize);
    let recovery = unsafe { session.assume_dead(peer) }.expect("mark exact dead generation");
    // Naming no handler repairs nothing, although every member of this region is of the unit schema:
    // the refusal is the absence of a handler, and the claim comes back rather than being spent.
    let recovery = recovery.reap().unwrap_err();
    assert!(
        recovery
            .reap_with(&[layout::recovery_handler::<()>()])
            .is_ok(),
        "complete coupled recovery"
    );
    let replacement = joiner_session(&name, parent_base);
    assert_eq!(
        replacement.peer().slot(),
        peer.slot(),
        "a completely reaped slot becomes reusable"
    );
    assert_ne!(
        replacement.peer().generation(),
        peer.generation(),
        "slot reuse cannot recreate stale process authority"
    );
    assert!(
        unsafe { session.assume_dead(peer) }.is_none(),
        "the stale generation cannot target its live replacement"
    );
}

#[test]
fn wrong_region_is_rejected_at_the_root() {
    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let _creator = creator_session(&name);
    assert!(
        joins_as("wrong-region", &name).wait().success(),
        "a peer joined at a foreign root"
    );
}

#[test]
fn session_attachment_is_not_coupled_to_a_channel_count() {
    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let _creator = creator_session(&name);

    Session::open(
        UnixFd::shm_open(&name).unwrap(),
        region::extent(REGION_SIZE),
        REGION_ID,
    )
    .expect("directory capacity is dynamic");
}

#[test]
fn changed_talc_geometry_is_rejected_by_talc_information() {
    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let _creator = creator_session(&name);
    assert!(
        joins_as("talc-geometry", &name).wait().success(),
        "a peer asked for a narrower geometry"
    );
}

#[test]
fn session_attachment_is_not_coupled_to_a_channel_protocol() {
    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let _creator = creator_session(&name);

    let fd = UnixFd::shm_open(&name).unwrap();
    Session::open(fd, region::extent(REGION_SIZE), REGION_ID)
        .expect("each channel validates its own protocol");
}

#[test]
fn session_attachment_is_not_coupled_to_a_channel_revision() {
    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let fd = UnixFd::shm_create(&name, REGION_SIZE).expect("create shared region");
    let _creator = Session::create(fd, region::extent(REGION_SIZE), REGION_ID)
        .expect("create revision-one session");

    let fd = UnixFd::shm_open(&name).unwrap();
    Session::open(fd, region::extent(REGION_SIZE), REGION_ID)
        .expect("each channel validates its own revision");
}

#[test]
fn admission_requires_write_permission() {
    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let _creator = creator_session(&name);
    let result = Session::open(
        UnixFd::shm_open(&name).expect("open shared region"),
        Request::new(REGION_SIZE, Access::READ),
        REGION_ID,
    );
    assert!(matches!(
        result,
        Err(SessionError::PermissionDenied {
            requested: Access::WRITE
        })
    ));
}

#[test]
fn recovery_dispatch_requires_the_exact_layout_handler() {
    let ready = ready_path("layout");
    let _ = fs::remove_file(&ready);
    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let session = creator_session(&name);
    let parent_base = session.base_addr();
    let alternate = Session::open(
        UnixFd::shm_open(&name).unwrap(),
        region::extent(REGION_SIZE),
        REGION_ID,
    )
    .unwrap();
    let (_channel, port) = alternate.create_channel::<AlternateEnvelope>(2).unwrap();
    drop(alternate);

    let mut command = role::child("alternate-layout-owner");
    command
        .env(NAME, &name)
        .env(READY, &ready)
        .env(PARENT_BASE, parent_base.to_string());
    declare_port(&mut command, &port);
    cuts::hand(&mut command, FOREIGN);
    let mut child = ChildGuard(command.spawn().expect("spawn the foreign owner"));
    assert_eq!(child.wait().code(), Some(FOREIGN), "no Rust destructor ran");
    let report = fs::read_to_string(&ready).expect("the foreign owner's report");
    fs::remove_file(&ready).expect("remove the report");
    let mut fields = report
        .split(',')
        .map(|field| field.parse::<usize>().unwrap());
    let peer = Peer::from_parts(fields.next().unwrap() as u8, fields.next().unwrap());

    let recovery = unsafe { session.assume_dead(peer) }.expect("mark the exact dead generation");
    let recovery = recovery.reap().unwrap_err();
    let handler = layout::recovery_handler::<AlternateEnvelope>();
    // A handler naming a schema this member does not carry is refused the same way naming none is, so
    // what completes the reap is the match, not the mere fact that a handler was supplied.
    let recovery = recovery
        .reap_with(&[layout::recovery_handler::<()>()])
        .unwrap_err();
    let recovery = recovery.reap_with(&[handler, handler]).unwrap_err();
    assert!(recovery.reap_with(&[handler]).is_ok());
}

#[test]
fn pool_blocks_owned_by_an_exited_process_are_reclaimed() {
    let ready = ready_path("pool");
    let _ = fs::remove_file(&ready);
    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let session = creator_session(&name);
    let parent_base = session.base_addr();
    let pool = session
        .create_pool(64 * 1024, Some(BlockRange::new(64, 64).unwrap()))
        .unwrap();

    let mut command = role::child("pool-owner");
    command
        .env(NAME, &name)
        .env(READY, &ready)
        .env(PARENT_BASE, parent_base.to_string());
    declare_pool(&mut command, pool.id());
    cuts::hand(&mut command, FILLED);
    let mut child = ChildGuard(command.spawn().expect("spawn the pool filler"));
    assert_eq!(child.wait().code(), Some(FILLED), "no Rust destructor ran");
    let report = fs::read_to_string(&ready).expect("the filler's report");
    fs::remove_file(&ready).expect("remove the report");
    let mut fields = report
        .split(',')
        .map(|field| field.parse::<usize>().unwrap());
    let peer = Peer::from_parts(fields.next().unwrap() as u8, fields.next().unwrap());
    assert!(fields.next().unwrap() >= 64);

    let decline = pool
        .as_ref()
        .put(9_u64)
        .err()
        .expect("a dead member's claims still occupy the pool");
    assert_eq!(
        decline.cause,
        DeclineCause::NoRoom {
            waits: RoomWait::Release
        },
        "the pool is full of one member's claims, so what frees room is a release"
    );
    assert_eq!(decline.value, Some(9_u64), "the declined block comes back");
    let recovery = unsafe { session.assume_dead(peer) }.unwrap();
    assert!(recovery.reap().is_ok());
    assert_eq!(*pool.as_ref().put(11_u64).unwrap(), 11);
}
