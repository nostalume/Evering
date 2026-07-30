#![cfg(all(unix, feature = "map"))]

use std::{
    process::{Child, Command, ExitStatus},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(feature = "process")]
use evering::process::Supervisor;
use evering::{
    Layout, LayoutContext, LayoutField, LayoutStatus, MapError, MapLayout, Peer, RcHeader,
    RegionAdmission, RegionId, Repr, Request, SchemaId, SchemaKey, SharedSchema, Source,
    os::unix::UnixFd,
    perlude::talc::{
        Access, Id, Session, SessionBy,
        channel::{QueueChannel, TryRecvError, TrySendError},
    },
};

const ROLE: &str = "EVERING_PROCESS_ROLE";
const SHM_NAME: &str = "EVERING_PROCESS_SHM";
const REGION_SIZE_ENV: &str = "EVERING_PROCESS_REGION_SIZE";
const ENTRY_SLAB: &str = "EVERING_PROCESS_ENTRY_SLAB";
const ENTRY_INDEX: &str = "EVERING_PROCESS_ENTRY_INDEX";
const ENTRY_GENERATION: &str = "EVERING_PROCESS_ENTRY_GENERATION";
const ENTRY_CAPACITY: &str = "EVERING_PROCESS_ENTRY_CAPACITY";
const PARENT_BASE: &str = "EVERING_PROCESS_PARENT_BASE";
const REGION_SIZE: usize = 4 * 1024 * 1024;
const QUEUE_CAPACITY: usize = 8;
const REGION_ID: RegionId = RegionId::new(0x4556_4552_494e_4701, 1);
const TIMEOUT: Duration = Duration::from_secs(5);

type UnixSession = Session<()>;
type AlternateSession = Session<AlternateEnvelope>;
type RevisionOneSession = Session<RevisionOne>;
type RevisionTwoSession = Session<RevisionTwo>;

#[repr(transparent)]
struct AlternateEnvelope(u64);

impl SharedSchema for AlternateEnvelope {
    const SCHEMA: SchemaKey = SchemaKey::new(SchemaId(0x414c_5445_524e_4154), 1);
}

unsafe impl Repr for AlternateEnvelope {
    const SCHEMA: SchemaKey = <Self as SharedSchema>::SCHEMA;
}

#[repr(transparent)]
struct RevisionOne(u64);

#[repr(transparent)]
struct RevisionTwo(u64);

impl SharedSchema for RevisionOne {
    const SCHEMA: SchemaKey = SchemaKey::new(SchemaId(0x5245_5649_5349_4f4e), 1);
}

impl SharedSchema for RevisionTwo {
    const SCHEMA: SchemaKey = SchemaKey::new(SchemaId(0x5245_5649_5349_4f4e), 2);
}

unsafe impl Repr for RevisionOne {
    const SCHEMA: SchemaKey = <Self as SharedSchema>::SCHEMA;
}
unsafe impl Repr for RevisionTwo {
    const SCHEMA: SchemaKey = <Self as SharedSchema>::SCHEMA;
}

struct CursorLayout;

impl SharedSchema for CursorLayout {
    const SCHEMA: SchemaKey = SchemaKey::new(SchemaId(0x4355_5253_4f52_0001), 1);
}

unsafe impl Layout for CursorLayout {
    type Config = ();
    type Info = ();

    const MAGIC: u16 = 0xC0A5;

    fn info(_: &Self::Config, _: LayoutContext) {}

    unsafe fn init(destination: *mut Self, _: Self::Config) -> LayoutStatus {
        unsafe { destination.write(Self) };
        LayoutStatus::Initialized
    }

    fn attach(&self, _: &Self::Config) -> LayoutStatus {
        LayoutStatus::Initialized
    }
}

struct ChildGuard(Child);

impl ChildGuard {
    fn running(&mut self) -> bool {
        self.0.try_wait().expect("observe child").is_none()
    }

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
    SessionBy::<()>::create(
        fd,
        Request::new(REGION_SIZE, Access::READ | Access::WRITE),
        REGION_ID,
    )
    .expect("create session")
}

fn open_layout(name: &str, size: usize, admission: RegionAdmission) -> Result<MapLayout, MapError> {
    let fd = UnixFd::shm_open(name).expect("open shared region");
    let map = fd
        .map(Request::new(size, Access::READ | Access::WRITE))
        .expect("map shared region");
    MapLayout::new(map, admission)
}

fn joiner_session(name: &str, parent_base: usize) -> UnixSession {
    let fd = UnixFd::shm_open(name).expect("open shared region");
    SessionBy::<()>::open(
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
            Err(TrySendError::Full(returned)) => value = returned,
            Err(TrySendError::Disconnected(_returned)) => {
                panic!("peer disconnected while sending")
            }
        }
        assert!(Instant::now() < deadline, "queue remained full");
        thread::yield_now();
    }
}

fn recv_bounded<T>(
    mut recv: impl FnMut() -> Result<T, TryRecvError>,
    deadline: Instant,
    mut child: Option<&mut ChildGuard>,
) -> T {
    loop {
        match recv() {
            Ok(value) => return value,
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => panic!("peer disconnected while receiving"),
        }
        if let Some(child) = child.as_deref_mut()
            && !child.running()
        {
            return recv().unwrap_or_else(|_| panic!("child exited without publishing a response"));
        }
        assert!(Instant::now() < deadline, "queue remained empty");
        thread::yield_now();
    }
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
    let parent_base = std::env::var(PARENT_BASE)
        .expect("parent base")
        .parse::<usize>()
        .expect("numeric parent base");

    let session = joiner_session(&name, parent_base);
    let server_base = session.base_addr();
    let view = session.acquire(id).expect("acquire current channel");
    let (send, recv) = view.rsplit();
    let deadline = Instant::now() + TIMEOUT;

    let request = recv_bounded(|| recv.try_recv(), deadline, None);
    let heap = session.heap();
    let (_, request) = heap.open::<(), u64>(request).expect("identify request");
    let value = *request;
    drop(request);

    let response_data = [value.wrapping_mul(3), server_base as u64];
    let response = heap.copy(&response_data).expect("allocate response");
    send_bounded(response.pack(()), |value| send.try_send(value), deadline);
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
    let id = session.prepare(QUEUE_CAPACITY).expect("prepare channel");
    let view = session.acquire(id).expect("acquire channel");
    let (send, recv) = view.lsplit();

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
            .env(PARENT_BASE, parent_base.to_string())
            .spawn()
            .expect("spawn server process"),
    );

    let deadline = Instant::now() + TIMEOUT;
    let heap = session.heap();
    let request = heap.put(14_u64).expect("allocate request");
    send_bounded(request.pack(()), |value| send.try_send(value), deadline);

    let response = recv_bounded(|| recv.try_recv(), deadline, Some(&mut child));
    let (_, response) = heap.open::<(), [u64]>(response).expect("identify response");
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
        let session = joiner_session(&name, parent_base);
        let view = session.acquire(id).expect("acquire current channel");
        let (send, _) = view.rsplit();
        let deadline = Instant::now() + TIMEOUT;
        let peer = session.peer();
        let heap = session.heap();
        for value in [peer.slot() as u64, peer.generation() as u64] {
            let record = heap.put(value).expect("allocate identity");
            send_bounded(record.pack(()), |record| send.try_send(record), deadline);
        }
        std::process::exit(77);
    }

    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let session = creator_session(&name);
    let parent_base = session.base_addr();
    let id = session.prepare(QUEUE_CAPACITY).expect("prepare channel");
    let view = session.acquire(id).expect("acquire channel");
    let (_, recv) = view.lsplit();
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
    let heap = session.heap();
    let mut identity = [0_u64; 2];
    for part in &mut identity {
        let record = recv_bounded(|| recv.try_recv(), deadline, None);
        let (_, value) = heap.open::<(), u64>(record).expect("open identity");
        *part = *value;
    }
    drop(recv);

    let peer = Peer::from_parts(identity[0] as u8, identity[1] as usize);
    #[cfg(feature = "process")]
    let recovery =
        unsafe { session.assume_exited(peer, &exit) }.expect("mark exact dead generation");
    #[cfg(not(feature = "process"))]
    let recovery = unsafe { session.assume_dead(peer) }.expect("mark exact dead generation");
    assert!(session.reap(recovery).is_ok(), "complete coupled recovery");
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
            open_layout(&name, REGION_SIZE, RegionAdmission::Expect(wrong)),
            Err(MapError::LayoutMismatch(LayoutField::Region))
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

    open_layout(&name, REGION_SIZE, RegionAdmission::Expect(REGION_ID))
        .and_then(UnixSession::try_from)
        .expect("directory capacity is dynamic");
}

#[test]
fn changed_talc_geometry_is_rejected_by_talc_information() {
    if std::env::var(ROLE).as_deref() == Ok("talc-geometry") {
        let name = std::env::var(SHM_NAME).expect("shared region name");
        let result = open_layout(
            &name,
            REGION_SIZE - 4096,
            RegionAdmission::Expect(REGION_ID),
        )
        .and_then(UnixSession::try_from);
        assert!(matches!(
            result,
            Err(MapError::LayoutMismatch(LayoutField::Info))
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
fn attach_only_mapping_cannot_initialize_a_missing_layout() {
    if std::env::var(ROLE).as_deref() == Ok("missing-layout") {
        let name = std::env::var(SHM_NAME).expect("shared region name");
        let result = open_layout(&name, REGION_SIZE, RegionAdmission::Expect(REGION_ID))
            .and_then(UnixSession::try_from);
        assert!(matches!(
            result,
            Err(MapError::LayoutMismatch(LayoutField::State))
        ));
        return;
    }
    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let fd = UnixFd::shm_create(&name, REGION_SIZE).expect("create shared region");
    let layout = MapLayout::map(
        fd,
        Request::new(REGION_SIZE, Access::READ | Access::WRITE),
        RegionAdmission::Create(REGION_ID),
    )
    .expect("create root-only mapping");

    assert!(
        spawn_case(
            "attach_only_mapping_cannot_initialize_a_missing_layout",
            "missing-layout",
            &name,
        )
        .wait()
        .success()
    );
    UnixSession::try_from(layout).expect("failed attachment must not mutate the missing layout");
}

#[test]
fn session_attachment_is_not_coupled_to_a_channel_protocol() {
    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let _creator = creator_session(&name);

    open_layout(&name, REGION_SIZE, RegionAdmission::Expect(REGION_ID))
        .and_then(AlternateSession::try_from)
        .expect("each channel validates its own protocol");
}

#[test]
fn session_attachment_is_not_coupled_to_a_channel_revision() {
    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let fd = UnixFd::shm_create(&name, REGION_SIZE).expect("create shared region");
    let _creator: RevisionOneSession = SessionBy::<RevisionOne>::create(
        fd,
        Request::new(REGION_SIZE, Access::READ | Access::WRITE),
        REGION_ID,
    )
    .expect("create revision-one session");

    open_layout(&name, REGION_SIZE, RegionAdmission::Expect(REGION_ID))
        .and_then(RevisionTwoSession::try_from)
        .expect("each channel validates its own revision");
}

#[test]
fn cursor_overflow_is_rejected_before_reservation() {
    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let fd = UnixFd::shm_create(&name, REGION_SIZE).expect("create shared region");
    let mut layout = MapLayout::map(
        fd,
        Request::new(REGION_SIZE, Access::READ | Access::WRITE),
        RegionAdmission::Create(REGION_ID),
    )
    .expect("create root mapping");
    assert!(matches!(
        layout.forward(usize::MAX),
        Err(MapError::ArithmeticOverflow)
    ));
    let remaining = layout.rest_size();
    layout.forward(remaining).expect("advance to one-past-end");
    assert!(matches!(
        layout.reserve::<RcHeader<CursorLayout>>(),
        Err(MapError::UnenoughSpace { .. })
    ));
}

#[test]
fn admission_requires_write_permission() {
    let name = unique_name();
    let _shm = ShmGuard(name.clone());
    let _creator = creator_session(&name);
    let fd = UnixFd::shm_open(&name).expect("open shared region");
    let map = fd
        .map(Request::new(REGION_SIZE, Access::READ))
        .expect("map read-only region");
    let result = MapLayout::new(map, RegionAdmission::Expect(REGION_ID));
    assert!(matches!(
        result,
        Err(MapError::PermissionDenied {
            requested: Access::WRITE
        })
    ));
}
