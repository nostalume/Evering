#![cfg(all(feature = "os", windows))]
#![expect(
    clippy::result_large_err,
    reason = "uncommitted transfers stay inline so retry retains exact ownership"
)]

//! The Windows leg of the two recovery cases in `tests/ipc_process.rs` that only a second process can
//! ask of: a member that died mid-queue has to be reaped by a handler naming the schema its channel
//! carries, and a completely reaped slot has to come back around. `tests/recovery_process.rs` is
//! platform-neutral and already drives `Recovery::reap_with` with the exact handler for the unit schema
//! to `Ok` here, against four real peers cut at four different points — so the accepted face of that
//! dispatch is not new evidence on this host. What these two cases add is the way round it: a reap that
//! names no handler is refused, once where every member carries the unit schema and once where one
//! member carries a foreign one, so the refusal is the absence of a handler and not a mismatch; a
//! handler naming the wrong schema is refused, and so is the right handler named twice, where naming it
//! once completes; and each refusal hands the caller back the same bounded recovery rather than
//! spending it. Then the aftermath of a complete reap, where the slot is reusable again, its generation
//! is not reproducible, and the stale identity can no longer be pointed at.
//!
//! Two seams differ from the unix leg, both because this host names no shared region across processes.
//! The region reaches the child as a duplicated section handle over the control pipe, so no POSIX shm
//! name and no `PARENT_BASE` travel anywhere: the numbers of the channel entry and the pool travel as
//! the bytes sent beside that handle, and in the first case the dead member's identity comes back the
//! way the unix leg sends it — two records through the channel and pool that member adopted, written
//! into shared bytes so the report outlives the process that wrote it. The second case cannot use that
//! route, because its queue is typed by the foreign schema and would not carry a plain number, so the
//! doomed owner writes its own slot and generation to a file and the parent reads the file only after
//! the exit. Neither leg asks where a mapping landed: [`crate::mapping::REGION_ALIGN`]'s non-unix value
//! is the section allocation granularity, `MapViewOfFile` takes no address, and the replacement
//! attachment here is therefore a second view of the same handle inside this process rather than a
//! process placed at a chosen base.
//!
//! Not evidence here, and stated so it is not mistaken for coverage: `different_base_process_round_trip`
//! is not ported and has no Windows face at all, because the alternate base it asserts cannot be
//! arranged by this backend. The child in each case leaves by `std::process::exit`, skipping Rust
//! destructors the way the unix leg requires — a child that unwound cleanly would release its own slot
//! and leave nothing for `reap_with` to find. Nothing here widens `nMaxInstances`, exercises the
//! `MAX_MESSAGE` ceiling or reaches `rollback` after a failed duplication, all of which stay recorded
//! as unexecuted obligations of the Windows legs they belong to.

use std::{
    env, fs,
    os::windows::io::AsHandle,
    path::Path,
    time::{Duration, Instant},
};

use evering::{
    ChannelId as Id, PoolId, Port, Session,
    layout::{self, RegionId, Repr, SchemaId, SchemaKey, SharedSchema},
    mapping::{Access, Peer, Request},
    os::windows::{
        Section,
        process::{Listener, Socket},
    },
    process::{Bootstrap, Supervisor},
};

mod support;
use crate::support::{cuts, introduce, role};

const ADDRESS: &str = "EVERING_RECOVERY_ADDRESS";
const READY: &str = "EVERING_RECOVERY_READY";
const SIZE: usize = 4 * 1024 * 1024;
const REGION: RegionId = RegionId::new(0x0052_4e44_4f57_5300, 1);
const QUEUE_CAPACITY: usize = 8;
const TIMEOUT: Duration = Duration::from_secs(5);

/// The doomed member of the first case: it publishes its own identity and leaves without releasing it.
const DOOMED: i32 = 77;
/// The doomed owner of the second case: it adopts a channel of the foreign schema and leaves.
const FOREIGN: i32 = 73;

/// A schema no recovery handler in this suite is ever handed by default, so its members can only be
/// reaped by a caller who names it.
#[repr(transparent)]
struct AlternateEnvelope(u64);

impl SharedSchema for AlternateEnvelope {
    const SCHEMA: SchemaKey = SchemaKey::new(SchemaId(0x414c_5445_524e_4154), 1);
}

unsafe impl Repr for AlternateEnvelope {
    const SCHEMA: SchemaKey = <Self as SharedSchema>::SCHEMA;
}

fn channel_only(port: &Port<AlternateEnvelope>) -> Bootstrap {
    let id = port.id();
    Bootstrap::new(format!(
        "{},{},{},{},{},{}",
        id.slab(),
        id.entry(),
        id.generation(),
        id.capacity(),
        port.role(),
        port.generation(),
    ))
    .unwrap()
}

/// The six numbers identifying one channel endpoint, read off the handover's bytes.
fn parse_port<T: Repr>(parts: &mut impl Iterator<Item = usize>) -> Port<T> {
    let id = Id::new(
        REGION,
        parts.next().unwrap() as u32,
        parts.next().unwrap() as u32,
        parts.next().unwrap(),
        parts.next().unwrap(),
    );
    Port::from_parts(id, parts.next().unwrap() as u8, parts.next().unwrap()).unwrap()
}

fn parse_channel_and_pool(bytes: &[u8]) -> (Port<()>, PoolId) {
    let mut parts = std::str::from_utf8(bytes)
        .unwrap()
        .split(',')
        .map(|part| part.parse::<usize>().unwrap());
    let port = parse_port(&mut parts);
    let pool = PoolId::new(
        REGION,
        parts.next().unwrap() as u32,
        parts.next().unwrap() as u32,
        parts.next().unwrap(),
    );
    (port, pool)
}

fn parse_channel(bytes: &[u8]) -> Port<AlternateEnvelope> {
    let mut parts = std::str::from_utf8(bytes)
        .unwrap()
        .split(',')
        .map(|part| part.parse::<usize>().unwrap());
    parse_port(&mut parts)
}

/// The child half of either case: one section handle over the pipe, and the numbers sent with it.
fn meet() -> (Session, Bootstrap) {
    let socket = Socket::connect(&env::var(ADDRESS).expect("rendezvous address"))
        .expect("child meets the parent that spawned it");
    let (bytes, resources) = socket.recv(1).expect("one section handle").into_parts();
    let mut resources = resources.into_vec();
    let section = Section::from_owned_handle(resources.remove(0));
    let session = Session::open(
        section,
        Request::new(SIZE, Access::READ | Access::WRITE),
        REGION,
    )
    .expect("join the session the parent created");
    (session, bytes)
}

/// The child's whole program: the roles this claim defines, and nothing else.
#[test]
fn role_entry() {
    role::dispatch(&[
        ("doomed-member", doomed_member),
        ("foreign-owner", foreign_owner),
    ]);
}

/// The first case's role: publish its own identity twice, then leave with the number the parent handed so
/// no destructor runs.
fn doomed_member() -> i32 {
    let (session, bytes) = meet();
    let (port, pool_id) = parse_channel_and_pool(bytes.as_ref());
    let channel = session.adopt(port).expect("adopt the current channel role");
    let pool = session.open_pool(pool_id).expect("open the identity pool");
    let (send, _) = channel.split();
    let peer = session.peer();
    let deadline = Instant::now() + TIMEOUT;
    for value in [peer.slot() as u64, peer.generation() as u64] {
        let record = pool.as_ref().put(value).expect("allocate identity");
        introduce::send_bounded(
            record.transfer(()),
            |record| send.try_send(record),
            deadline,
        );
    }
    // Diverges in tail position rather than returning the code: a body that returned would drop the
    // session it just stood in, and the parent would find a politely released slot instead of a reap.
    role::leave(cuts::code())
}

/// The second case's role: become the member of a schema no default handler names, report itself, and
/// leave the same way.
fn foreign_owner() -> i32 {
    let (session, bytes) = meet();
    let port = parse_channel(bytes.as_ref());
    let _channel = session.adopt(port).expect("adopt the foreign schema role");
    let peer = session.peer();
    fs::write(
        env::var_os(READY).expect("ready path"),
        format!("{},{}", peer.slot(), peer.generation()),
    )
    .expect("report the member this process became");
    role::leave(cuts::code())
}

fn admit(name: &str, code: i32, report: Option<&Path>) -> (Supervisor, Listener) {
    let listener = Listener::bind().expect("a rendezvous for the doomed peer");
    let mut command = role::child(name);
    command.env(ADDRESS, listener.name());
    if let Some(report) = report {
        command.env(READY, report);
    }
    cuts::hand(&mut command, code);
    let child = Supervisor::spawn(&mut command).expect("spawn the doomed peer");
    (child, listener)
}

fn region() -> (Section<std::os::windows::io::OwnedHandle>, Session) {
    let source = Section::anonymous(SIZE, Access::READ | Access::WRITE).expect("anonymous region");
    let session = Session::create(
        source.borrow(),
        Request::new(SIZE, Access::READ | Access::WRITE),
        REGION,
    )
    .expect("create the session");
    (source, session)
}

#[test]
fn a_dead_member_is_reaped_by_its_exact_handler_and_its_slot_comes_back() {
    let (mut child, listener) = admit("doomed-member", DOOMED, None);
    let (source, session) = region();
    let pool = session.create_pool(64 * 1024, None).expect("identity pool");
    let pool_id = pool.id();
    let (channel, port) = session
        .create_channel::<()>(QUEUE_CAPACITY)
        .expect("create the queue the doomed member adopts");
    let (_, recv) = channel.split();
    let socket = listener
        .accept(&child, Instant::now() + TIMEOUT)
        .expect("the doomed peer connects");
    socket
        .send(
            &child,
            &introduce::bootstrap(&port, pool_id),
            &[source.as_handle()],
        )
        .expect("hand the region over");

    let exit = child.wait().expect("wait for the doomed member");
    assert_eq!(exit.status().code(), Some(DOOMED), "no Rust destructor ran");

    let deadline = Instant::now() + TIMEOUT;
    let mut identity = [0_u64; 2];
    for part in &mut identity {
        let (_, value) = introduce::adopt_bounded::<(), u64>(&recv, pool.as_ref(), deadline);
        *part = *value;
    }
    drop(recv);

    let peer = Peer::from_parts(identity[0] as u8, identity[1] as usize);
    let recovery = unsafe { session.assume_dead(peer) }.expect("mark the exact dead generation");
    // Naming no handler repairs nothing here either, although every member of this region is of the
    // unit schema — the refusal is about the absence of a handler, not about a foreign one — and the
    // claim comes back rather than being spent.
    let recovery = recovery.reap().unwrap_err();
    assert!(
        recovery
            .reap_with(&[layout::recovery_handler::<()>()])
            .is_ok(),
        "complete coupled recovery"
    );
    let replacement = Session::open(
        source.borrow(),
        Request::new(SIZE, Access::READ | Access::WRITE),
        REGION,
    )
    .expect("join again after the reap");
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
fn a_foreign_schema_reap_refuses_every_handler_short_of_the_exact_one() {
    let ready = env::temp_dir().join(format!("evering-foreign-owner-{}.txt", std::process::id()));
    let _ = fs::remove_file(&ready);
    let (mut child, listener) = admit("foreign-owner", FOREIGN, Some(&ready));
    let (source, session) = region();
    let alternate = Session::open(
        source.borrow(),
        Request::new(SIZE, Access::READ | Access::WRITE),
        REGION,
    )
    .expect("a second view, to create the foreign queue from");
    let (_channel, port) = alternate
        .create_channel::<AlternateEnvelope>(2)
        .expect("create the foreign-schema queue");
    drop(alternate);
    let socket = listener
        .accept(&child, Instant::now() + TIMEOUT)
        .expect("the foreign owner connects");
    socket
        .send(&child, &channel_only(&port), &[source.as_handle()])
        .expect("hand the region over");

    let exit = child.wait().expect("wait for the foreign owner");
    assert_eq!(
        exit.status().code(),
        Some(FOREIGN),
        "no Rust destructor ran"
    );
    let report = fs::read_to_string(&ready).expect("the foreign owner's report");
    fs::remove_file(&ready).expect("remove the report");
    let mut parts = report.split(',').map(|part| part.parse::<usize>().unwrap());
    let peer = Peer::from_parts(parts.next().unwrap() as u8, parts.next().unwrap());

    let recovery = unsafe { session.assume_dead(peer) }.expect("mark the exact dead generation");
    let recovery = recovery.reap().unwrap_err();
    let handler = layout::recovery_handler::<AlternateEnvelope>();
    // A handler naming a schema this member does not carry is refused the same way naming none is, so
    // what completes the reap is the match, not the mere fact that a handler was supplied.
    let recovery = recovery
        .reap_with(&[layout::recovery_handler::<()>()])
        .unwrap_err();
    let recovery = recovery.reap_with(&[handler, handler]).unwrap_err();
    assert!(
        recovery.reap_with(&[handler]).is_ok(),
        "the exact handler completes the reap"
    );
}
