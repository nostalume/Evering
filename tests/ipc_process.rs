#![cfg(all(unix, feature = "map"))]
#![expect(
    clippy::result_large_err,
    reason = "uncommitted transfers stay inline so retry retains exact ownership"
)]

use std::{
    io::Write,
    process::{Child, Command, ExitStatus},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(feature = "process")]
use evering::process::Supervisor;
use evering::{
    Block, BlockRange, ChannelId as Id, PoolId, PoolRef, PoolReserveError, Port, Rx, Session,
    SessionError, TrySendError,
    layout::{self, RegionId, Repr, SchemaId, SchemaKey, Shape, SharedSchema},
    mapping::{Access, LayoutField, Peer, Request},
    os::unix::UnixFd,
};

const ROLE: &str = "EVERING_PROCESS_ROLE";
const SHM_NAME: &str = "EVERING_PROCESS_SHM";
const REGION_SIZE_ENV: &str = "EVERING_PROCESS_REGION_SIZE";
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

fn spawn_case(test: &str, role: &str, name: &str) -> ChildGuard {
    ChildGuard(
        Command::new(std::env::current_exe().expect("test executable"))
            .arg("--exact")
            .arg(test)
            .arg("--nocapture")
            .env(ROLE, role)
            .env(SHM_NAME, name)
            .spawn()
            .expect("spawn peer process"),
    )
}

fn creator_session(name: &str) -> UnixSession {
    let fd = UnixFd::shm_create(name, REGION_SIZE).expect("create shared region");
    Session::create(
        fd,
        Request::new(REGION_SIZE, Access::READ | Access::WRITE),
        REGION_ID,
    )
    .expect("create session")
}

fn joiner_session(name: &str, parent_base: usize) -> UnixSession {
    let fd = UnixFd::shm_open(name).expect("open shared region");
    Session::open(
        fd.mapping().at(parent_base.wrapping_add(1 << 30)),
        Request::new(REGION_SIZE, Access::READ | Access::WRITE),
        REGION_ID,
    )
    .expect("join session")
}

fn send_bounded<T>(
    mut value: T,
    mut send: impl FnMut(T) -> Result<(), TrySendError<T>>,
    deadline: Instant,
) {
    loop {
        match send(value) {
            Ok(()) => return,
            Err(TrySendError::Full(returned) | TrySendError::Busy(returned)) => value = returned,
            Err(TrySendError::Disconnected(_returned)) => {
                panic!("peer disconnected while sending")
            }
        }
        assert!(Instant::now() < deadline, "queue remained full");
        thread::yield_now();
    }
}

fn adopt_bounded<'p, H, T>(recv: &Rx<H>, pool: PoolRef<'p>, deadline: Instant) -> (H, Block<'p, T>)
where
    H: Repr,
    T: Repr + Shape + ?Sized,
{
    loop {
        match recv.claim() {
            Ok(received) => match received.adopt(pool) {
                Ok(value) => return value,
                Err(error) => panic!("matching Pool rejected transfer: {error:?}"),
            },
            Err(evering::ReceiveError::Empty | evering::ReceiveError::Busy) => {}
            Err(evering::ReceiveError::Closed) => panic!("peer disconnected while receiving"),
        }
        assert!(Instant::now() < deadline, "queue remained empty");
        thread::yield_now();
    }
}

fn pool_id_from_env() -> PoolId {
    PoolId::new(
        REGION_ID,
        std::env::var(POOL_SLAB).unwrap().parse().unwrap(),
        std::env::var(POOL_INDEX).unwrap().parse().unwrap(),
        std::env::var(POOL_GENERATION).unwrap().parse().unwrap(),
    )
}

fn server() {
    let name = std::env::var(SHM_NAME).expect("shared region name");
    let region_size = std::env::var(REGION_SIZE_ENV)
        .expect("region size")
        .parse::<usize>()
        .expect("numeric region size");
    assert_eq!(region_size, REGION_SIZE);
    let id = Id::new(
        REGION_ID,
        std::env::var(ENTRY_SLAB)
            .expect("entry slab")
            .parse()
            .expect("numeric entry slab"),
        std::env::var(ENTRY_INDEX)
            .expect("entry index")
            .parse()
            .expect("numeric entry index"),
        std::env::var(ENTRY_GENERATION)
            .expect("entry generation")
            .parse()
            .expect("numeric entry generation"),
        std::env::var(ENTRY_CAPACITY)
            .expect("entry capacity")
            .parse()
            .expect("numeric entry capacity"),
    );
    let port = Port::from_parts(
        id,
        std::env::var(PORT_ROLE)
            .expect("port role")
            .parse()
            .expect("numeric port role"),
        std::env::var(PORT_GENERATION)
            .expect("port generation")
            .parse()
            .expect("numeric port generation"),
    )
    .expect("canonical Port");
    let parent_base = std::env::var(PARENT_BASE)
        .expect("parent base")
        .parse::<usize>()
        .expect("numeric parent base");

    let session = joiner_session(&name, parent_base);
    let pool = session
        .open_pool(pool_id_from_env())
        .expect("acquire transfer Pool");
    let server_base = session.base_addr();
    let channel = session.adopt(port).expect("adopt current channel role");
    let (send, recv) = channel.split();
    let deadline = Instant::now() + TIMEOUT;

    let (_, request) = adopt_bounded::<(), u64>(&recv, pool.as_ref(), deadline);
    let value = *request;
    drop(request);

    let response_data = [value.wrapping_mul(3), server_base as u64];
    let response = pool
        .as_ref()
        .copy(&response_data)
        .expect("allocate response");
    send_bounded(
        response.transfer(()),
        |value| send.try_send(value),
        deadline,
    );
    send.close();
}

#[test]
fn different_base_process_round_trip() {
    if std::env::var_os(ROLE).is_some() {
        server();
        return;
    }

    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let session = creator_session(&name);
    let parent_base = session.base_addr();
    let pool = session.create_pool(64 * 1024, None).unwrap();
    let pool_id = pool.id();
    let (_, pool_slab, pool_index, pool_generation) = pool_id.parts();
    let (channel, port) = session
        .create_channel::<()>(QUEUE_CAPACITY)
        .expect("create channel");
    let id = port.id();
    let (send, recv) = channel.split();

    let mut child = ChildGuard(
        Command::new(std::env::current_exe().expect("test executable"))
            .arg("--exact")
            .arg("different_base_process_round_trip")
            .arg("--nocapture")
            .env(ROLE, "server")
            .env(SHM_NAME, &name)
            .env(REGION_SIZE_ENV, REGION_SIZE.to_string())
            .env(ENTRY_SLAB, id.slab().to_string())
            .env(ENTRY_INDEX, id.entry().to_string())
            .env(ENTRY_GENERATION, id.generation().to_string())
            .env(ENTRY_CAPACITY, id.capacity().to_string())
            .env(PORT_ROLE, port.role().to_string())
            .env(PORT_GENERATION, port.generation().to_string())
            .env(POOL_SLAB, pool_slab.to_string())
            .env(POOL_INDEX, pool_index.to_string())
            .env(POOL_GENERATION, pool_generation.to_string())
            .env(PARENT_BASE, parent_base.to_string())
            .spawn()
            .expect("spawn server process"),
    );

    let deadline = Instant::now() + TIMEOUT;
    let request = pool.as_ref().put(14_u64).expect("allocate request");
    send_bounded(request.transfer(()), |value| send.try_send(value), deadline);

    let (_, response) = adopt_bounded::<(), [u64]>(&recv, pool.as_ref(), deadline);
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
    if std::env::var(ROLE).as_deref() == Ok("announce-peer-and-exit") {
        let name = std::env::var(SHM_NAME).expect("shared region name");
        let parent_base = std::env::var(PARENT_BASE)
            .expect("parent base")
            .parse::<usize>()
            .expect("numeric parent base");
        let id = Id::new(
            REGION_ID,
            std::env::var(ENTRY_SLAB).unwrap().parse().unwrap(),
            std::env::var(ENTRY_INDEX).unwrap().parse().unwrap(),
            std::env::var(ENTRY_GENERATION).unwrap().parse().unwrap(),
            std::env::var(ENTRY_CAPACITY).unwrap().parse().unwrap(),
        );
        let port = Port::from_parts(
            id,
            std::env::var(PORT_ROLE).unwrap().parse().unwrap(),
            std::env::var(PORT_GENERATION).unwrap().parse().unwrap(),
        )
        .unwrap();
        let session = joiner_session(&name, parent_base);
        let channel = session.adopt(port).expect("adopt current channel role");
        let (send, _) = channel.split();
        let deadline = Instant::now() + TIMEOUT;
        let peer = session.peer();
        let pool = session.open_pool(pool_id_from_env()).unwrap();
        for value in [peer.slot() as u64, peer.generation() as u64] {
            let record = pool.as_ref().put(value).expect("allocate identity");
            send_bounded(
                record.transfer(()),
                |record| send.try_send(record),
                deadline,
            );
        }
        std::process::exit(77);
    }

    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let session = creator_session(&name);
    let parent_base = session.base_addr();
    let pool = session.create_pool(64 * 1024, None).unwrap();
    let pool_id = pool.id();
    let (_, pool_slab, pool_index, pool_generation) = pool_id.parts();
    let (channel, port) = session
        .create_channel::<()>(QUEUE_CAPACITY)
        .expect("create channel");
    let id = channel.id();
    let (_, recv) = channel.split();
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .arg("--exact")
        .arg("dead_process_membership_is_reaped_after_a_complete_layout_scan")
        .arg("--nocapture")
        .env(ROLE, "announce-peer-and-exit")
        .env(SHM_NAME, &name)
        .env(REGION_SIZE_ENV, REGION_SIZE.to_string())
        .env(ENTRY_SLAB, id.slab().to_string())
        .env(ENTRY_INDEX, id.entry().to_string())
        .env(ENTRY_GENERATION, id.generation().to_string())
        .env(ENTRY_CAPACITY, id.capacity().to_string())
        .env(PORT_ROLE, port.role().to_string())
        .env(PORT_GENERATION, port.generation().to_string())
        .env(POOL_SLAB, pool_slab.to_string())
        .env(POOL_INDEX, pool_index.to_string())
        .env(POOL_GENERATION, pool_generation.to_string())
        .env(PARENT_BASE, parent_base.to_string());
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
    assert_eq!(status.code(), Some(77), "child must skip Rust destructors");

    let deadline = Instant::now() + TIMEOUT;
    let mut identity = [0_u64; 2];
    for part in &mut identity {
        let (_, value) = adopt_bounded::<(), u64>(&recv, pool.as_ref(), deadline);
        *part = *value;
    }
    drop(recv);

    let peer = Peer::from_parts(identity[0] as u8, identity[1] as usize);
    #[cfg(feature = "process")]
    let recovery = unsafe { session.assume_dead(peer) }.expect("mark exact dead generation");
    #[cfg(not(feature = "process"))]
    let recovery = unsafe { session.assume_dead(peer) }.expect("mark exact dead generation");
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
    if std::env::var(ROLE).as_deref() == Ok("wrong-region") {
        let name = std::env::var(SHM_NAME).expect("shared region name");
        let wrong = RegionId::new(REGION_ID.high, REGION_ID.low.wrapping_add(1));
        assert!(matches!(
            Session::open(
                UnixFd::shm_open(&name).unwrap(),
                Request::new(REGION_SIZE, Access::READ | Access::WRITE),
                wrong,
            ),
            Err(SessionError::LayoutMismatch(LayoutField::Region))
        ));
        return;
    }
    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let _creator = creator_session(&name);
    assert!(
        spawn_case(
            "wrong_region_is_rejected_at_the_root",
            "wrong-region",
            &name
        )
        .wait()
        .success()
    );
}

#[test]
fn session_attachment_is_not_coupled_to_a_channel_count() {
    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let _creator = creator_session(&name);

    Session::open(
        UnixFd::shm_open(&name).unwrap(),
        Request::new(REGION_SIZE, Access::READ | Access::WRITE),
        REGION_ID,
    )
    .expect("directory capacity is dynamic");
}

#[test]
fn changed_talc_geometry_is_rejected_by_talc_information() {
    if std::env::var(ROLE).as_deref() == Ok("talc-geometry") {
        let name = std::env::var(SHM_NAME).expect("shared region name");
        let result = Session::open(
            UnixFd::shm_open(&name).unwrap(),
            Request::new(REGION_SIZE - 4096, Access::READ | Access::WRITE),
            REGION_ID,
        );
        assert!(matches!(
            result,
            Err(SessionError::LayoutMismatch(LayoutField::Info))
        ));
        return;
    }
    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let _creator = creator_session(&name);
    assert!(
        spawn_case(
            "changed_talc_geometry_is_rejected_by_talc_information",
            "talc-geometry",
            &name,
        )
        .wait()
        .success()
    );
}

#[test]
fn session_attachment_is_not_coupled_to_a_channel_protocol() {
    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let _creator = creator_session(&name);

    let fd = UnixFd::shm_open(&name).unwrap();
    Session::open(
        fd,
        Request::new(REGION_SIZE, Access::READ | Access::WRITE),
        REGION_ID,
    )
    .expect("each channel validates its own protocol");
}

#[test]
fn session_attachment_is_not_coupled_to_a_channel_revision() {
    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let fd = UnixFd::shm_create(&name, REGION_SIZE).expect("create shared region");
    let _creator = Session::create(
        fd,
        Request::new(REGION_SIZE, Access::READ | Access::WRITE),
        REGION_ID,
    )
    .expect("create revision-one session");

    let fd = UnixFd::shm_open(&name).unwrap();
    Session::open(
        fd,
        Request::new(REGION_SIZE, Access::READ | Access::WRITE),
        REGION_ID,
    )
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
    if std::env::var(ROLE).as_deref() == Ok("alternate-layout-owner") {
        let name = std::env::var(SHM_NAME).unwrap();
        let parent_base = std::env::var(PARENT_BASE)
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let id: Id<AlternateEnvelope> = Id::new(
            REGION_ID,
            std::env::var(ENTRY_SLAB).unwrap().parse().unwrap(),
            std::env::var(ENTRY_INDEX).unwrap().parse().unwrap(),
            std::env::var(ENTRY_GENERATION).unwrap().parse().unwrap(),
            std::env::var(ENTRY_CAPACITY).unwrap().parse().unwrap(),
        );
        let fd = UnixFd::shm_open(&name).unwrap();
        let session = Session::open(
            fd.mapping().at(parent_base.wrapping_add(1 << 30)),
            Request::new(REGION_SIZE, Access::READ | Access::WRITE),
            REGION_ID,
        )
        .unwrap();
        let port = Port::from_parts(
            id,
            std::env::var(PORT_ROLE).unwrap().parse().unwrap(),
            std::env::var(PORT_GENERATION).unwrap().parse().unwrap(),
        )
        .unwrap();
        let _channel = session.adopt(port).unwrap();
        let peer = session.peer();
        println!("EVERING_PEER={},{}", peer.slot(), peer.generation());
        std::io::stdout().flush().unwrap();
        std::process::exit(73);
    }

    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let session = creator_session(&name);
    let parent_base = session.base_addr();
    let alternate = Session::open(
        UnixFd::shm_open(&name).unwrap(),
        Request::new(REGION_SIZE, Access::READ | Access::WRITE),
        REGION_ID,
    )
    .unwrap();
    let (_channel, port) = alternate.create_channel::<AlternateEnvelope>(2).unwrap();
    let id = port.id();
    drop(alternate);
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("recovery_dispatch_requires_the_exact_layout_handler")
        .arg("--nocapture")
        .env(ROLE, "alternate-layout-owner")
        .env(SHM_NAME, &name)
        .env(PARENT_BASE, parent_base.to_string())
        .env(ENTRY_SLAB, id.slab().to_string())
        .env(ENTRY_INDEX, id.entry().to_string())
        .env(ENTRY_GENERATION, id.generation().to_string())
        .env(ENTRY_CAPACITY, id.capacity().to_string())
        .env(PORT_ROLE, port.role().to_string())
        .env(PORT_GENERATION, port.generation().to_string())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(73));
    let output = String::from_utf8(output.stdout).unwrap();
    let identity = output
        .lines()
        .find_map(|line| line.strip_prefix("EVERING_PEER="))
        .unwrap();
    let mut identity = identity.split(',');
    let peer = Peer::from_parts(
        identity.next().unwrap().parse().unwrap(),
        identity.next().unwrap().parse().unwrap(),
    );
    let recovery = unsafe { session.assume_dead(peer) }.unwrap();
    let recovery = recovery.reap().unwrap_err();
    let handler = layout::recovery_handler::<AlternateEnvelope>();
    let recovery = recovery.reap_with(&[handler, handler]).unwrap_err();
    assert!(recovery.reap_with(&[handler]).is_ok());
}

#[test]
fn pool_blocks_owned_by_an_exited_process_are_reclaimed() {
    if std::env::var(ROLE).as_deref() == Ok("pool-owner") {
        let name = std::env::var(SHM_NAME).unwrap();
        let parent_base = std::env::var(PARENT_BASE).unwrap().parse().unwrap();
        let id = PoolId::new(
            REGION_ID,
            std::env::var(ENTRY_SLAB).unwrap().parse().unwrap(),
            std::env::var(ENTRY_INDEX).unwrap().parse().unwrap(),
            std::env::var(ENTRY_GENERATION).unwrap().parse().unwrap(),
        );
        let session = joiner_session(&name, parent_base);
        let peer = session.peer();
        let pool = session.open_pool(id).unwrap();
        let pool_ref = pool.as_ref();
        let mut blocks = Vec::new();
        loop {
            match pool_ref.put(7_u64) {
                Ok(block) => blocks.push(block),
                Err(PoolReserveError::Unavailable(7)) => break,
                Err(error) => panic!("unexpected Pool error: {error:?}"),
            }
        }
        println!(
            "EVERING_POOL_PEER={},{},{}",
            peer.slot(),
            peer.generation(),
            blocks.len()
        );
        std::io::stdout().flush().unwrap();
        core::mem::forget(blocks);
        core::mem::forget(pool);
        core::mem::forget(session);
        std::process::exit(74);
    }

    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let session = creator_session(&name);
    let parent_base = session.base_addr();
    let pool = session
        .create_pool(64 * 1024, Some(BlockRange::new(64, 64).unwrap()))
        .unwrap();
    let id = pool.id();
    let (_, slab, entry, generation) = id.parts();
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("pool_blocks_owned_by_an_exited_process_are_reclaimed")
        .arg("--nocapture")
        .env(ROLE, "pool-owner")
        .env(SHM_NAME, &name)
        .env(PARENT_BASE, parent_base.to_string())
        .env(ENTRY_SLAB, slab.to_string())
        .env(ENTRY_INDEX, entry.to_string())
        .env(ENTRY_GENERATION, generation.to_string())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(74));
    let line = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("EVERING_POOL_PEER="))
        .unwrap()
        .to_owned();
    let mut fields = line.split(',');
    let peer = Peer::from_parts(
        fields.next().unwrap().parse().unwrap(),
        fields.next().unwrap().parse().unwrap(),
    );
    assert!(fields.next().unwrap().parse::<usize>().unwrap() >= 64);
    assert!(matches!(
        pool.as_ref().put(9_u64),
        Err(PoolReserveError::Unavailable(9))
    ));
    let recovery = unsafe { session.assume_dead(peer) }.unwrap();
    assert!(recovery.reap().is_ok());
    assert_eq!(*pool.as_ref().put(11_u64).unwrap(), 11);
}
