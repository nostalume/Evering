#![cfg(all(feature = "process", feature = "notify"))]

use std::{
    env, fs,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use evering::{
    Listen, Notify, Peer, RegionId, Request, TryRecvError,
    os::{Event, Ring, event},
    perlude::talc::{Access, Id, Session, SessionBy},
    process::{Bootstrap, Supervisor},
};

const ROLE: &str = "EVERING_RECOVERY_ROLE";
const READY: &str = "EVERING_RECOVERY_READY";
const ADDRESS: &str = "EVERING_RECOVERY_ADDRESS";
const SIZE: usize = 4 * 1024 * 1024;
const REGION: RegionId = RegionId::new(0x7265_636f_7665_7279, 1);
const VALUE: u64 = 0x0051_a7e5;
const TIMEOUT: Duration = Duration::from_secs(5);

type TestSession = Session<()>;

#[derive(Clone, Copy, Debug)]
enum Cut {
    Reserved,
    Staged,
    Published,
    Claimed,
}

impl Cut {
    const ALL: [Self; 4] = [Self::Reserved, Self::Staged, Self::Published, Self::Claimed];

    const fn name(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Staged => "staged",
            Self::Published => "published",
            Self::Claimed => "claimed",
        }
    }

    const fn code(self) -> i32 {
        match self {
            Self::Reserved => 81,
            Self::Staged => 82,
            Self::Published => 83,
            Self::Claimed => 84,
        }
    }

    fn parse(value: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|cut| cut.name() == value)
            .expect("known crash cut")
    }
}

fn bootstrap(id: Id<()>) -> Bootstrap {
    Bootstrap::new(format!(
        "{},{},{},{}",
        id.slab(),
        id.entry(),
        id.generation(),
        id.capacity()
    ))
    .unwrap()
}

fn parse_id(bytes: &[u8]) -> Id<()> {
    let text = std::str::from_utf8(bytes).unwrap();
    let mut parts = text.split(',').map(|part| part.parse::<usize>().unwrap());
    Id::new(
        REGION,
        parts.next().unwrap() as u32,
        parts.next().unwrap() as u32,
        parts.next().unwrap(),
        parts.next().unwrap(),
    )
}

fn announce(peer: Peer, ring: &Ring, code: i32) -> ! {
    fs::write(
        env::var_os(READY).expect("ready path"),
        format!("{},{}", peer.slot(), peer.generation()),
    )
    .unwrap();
    ring.notify().unwrap();
    std::process::exit(code)
}

fn crash(session: TestSession, id: Id<()>, ring: Ring, cut: Cut) -> ! {
    let view = session.acquire(id).expect("crash subject");
    let (send, recv) = view.rsplit();
    let peer = session.peer();
    match cut {
        Cut::Reserved => {
            let _reserved = send.reserve().expect("reserve");
            announce(peer, &ring, cut.code());
        }
        Cut::Staged => {
            let record = session.heap().put(VALUE).unwrap().pack(());
            let _staged = send.reserve().unwrap().stage(record);
            announce(peer, &ring, cut.code());
        }
        Cut::Published => {
            let record = session.heap().put(VALUE).unwrap().pack(());
            send.reserve().unwrap().stage(record).publish();
            announce(peer, &ring, cut.code());
        }
        Cut::Claimed => {
            let _claim = recv.claim().expect("claim");
            announce(peer, &ring, cut.code());
        }
    }
}

#[cfg(unix)]
type Source = evering::os::unix::UnixFd<std::os::fd::OwnedFd>;
#[cfg(windows)]
type Source = evering::os::windows::Section<std::os::windows::io::OwnedHandle>;

struct Setup {
    source: Source,
    session: TestSession,
    id: Id<()>,
    child: Supervisor,
    event: Event,
    ready: PathBuf,
}

fn command(cut: Cut, ready: &PathBuf, address: &str) -> Command {
    let mut command = Command::new(env::current_exe().unwrap());
    command
        .args(["--exact", "recovery_conserves_every_accepted_record"])
        .env(ROLE, cut.name())
        .env(READY, ready)
        .env(ADDRESS, address);
    command
}

#[cfg(unix)]
fn child() -> Option<()> {
    use evering::os::unix::{UnixFd, process::Socket};

    let cut = Cut::parse(&env::var(ROLE).ok()?);
    let socket = Socket::bind(env::var_os(ADDRESS).unwrap()).unwrap();
    let (bytes, resources) = socket.recv(2).unwrap().into_parts();
    let mut resources = resources.into_vec();
    let source = UnixFd::from_fd(resources.remove(0)).unwrap();
    let ring = unsafe { Ring::from_owned_fd(resources.remove(0)) };
    let session = SessionBy::open(
        source,
        Request::new(SIZE, Access::READ | Access::WRITE),
        REGION,
    )
    .unwrap();
    crash(session, parse_id(bytes.as_ref()), ring, cut)
}

#[cfg(windows)]
fn child() -> Option<()> {
    use std::os::windows::io::IntoRawHandle;

    use evering::os::windows::{Section, process::Socket};

    let cut = Cut::parse(&env::var(ROLE).ok()?);
    let socket = Socket::connect(&env::var(ADDRESS).unwrap()).unwrap();
    let (bytes, resources) = socket.recv(2).unwrap().into_parts();
    let mut resources = resources.into_vec();
    let source = Section::from_owned_handle(resources.remove(0));
    let ring = unsafe { Ring::from_owned_handle(resources.remove(0).into_raw_handle()) };
    let session = SessionBy::open(
        source,
        Request::new(SIZE, Access::READ | Access::WRITE),
        REGION,
    )
    .unwrap();
    crash(session, parse_id(bytes.as_ref()), ring, cut)
}

#[cfg(unix)]
fn setup(cut: Cut, ready: PathBuf) -> Setup {
    use std::{os::fd::AsFd, thread};

    use evering::os::unix::{UnixFd, process::Socket};

    let source = UnixFd::memfd("evering-recovery", SIZE, false).unwrap();
    let session = SessionBy::create(
        source.borrow(),
        Request::new(SIZE, Access::READ | Access::WRITE),
        REGION,
    )
    .unwrap();
    let id = session.prepare(4).unwrap();
    seed_claimed(&session, id, cut);
    let path = env::temp_dir().join(format!(
        "evering-recovery-{}-{}.sock",
        std::process::id(),
        cut.name()
    ));
    let _ = fs::remove_file(&path);
    let mut child = Supervisor::spawn(&mut command(cut, &ready, path.to_str().unwrap())).unwrap();
    let deadline = Instant::now() + TIMEOUT;
    let socket = loop {
        match Socket::connect(&path) {
            Ok(socket) => break socket,
            Err(_) if Instant::now() < deadline => {
                assert!(child.try_wait().unwrap().is_none(), "child exited in setup");
                thread::yield_now();
            }
            Err(error) => panic!("child rendezvous: {error}"),
        }
    };
    let (ring, event) = event().unwrap();
    socket
        .send(&bootstrap(id), &[source.as_fd(), ring.as_fd()])
        .unwrap();
    fs::remove_file(path).unwrap();
    Setup {
        source,
        session,
        id,
        child,
        event,
        ready,
    }
}

#[cfg(windows)]
fn setup(cut: Cut, ready: PathBuf) -> Setup {
    use std::os::windows::io::AsHandle;

    use evering::os::windows::{Section, process::Listener};

    let listener = Listener::bind().unwrap();
    let child = Supervisor::spawn(&mut command(cut, &ready, listener.name())).expect("spawn child");
    let socket = listener.accept(&child).unwrap();
    let source = Section::anonymous(SIZE, Access::READ | Access::WRITE).unwrap();
    let session = SessionBy::create(
        source.borrow(),
        Request::new(SIZE, Access::READ | Access::WRITE),
        REGION,
    )
    .unwrap();
    let id = session.prepare(4).unwrap();
    seed_claimed(&session, id, cut);
    let (ring, event) = event().unwrap();
    socket
        .send(
            &child,
            &bootstrap(id),
            &[source.as_handle(), ring.as_handle()],
        )
        .unwrap();
    Setup {
        source,
        session,
        id,
        child,
        event,
        ready,
    }
}

fn seed_claimed(session: &TestSession, id: Id<()>, cut: Cut) {
    if matches!(cut, Cut::Claimed) {
        let (send, _) = session.acquire(id).unwrap().lsplit();
        send.try_send(session.heap().put(VALUE).unwrap().pack(()))
            .unwrap();
    }
}

fn peer(path: &PathBuf) -> Peer {
    let text = fs::read_to_string(path).unwrap();
    let mut parts = text.split(',');
    Peer::from_parts(
        parts.next().unwrap().parse::<u8>().unwrap(),
        parts.next().unwrap().parse::<usize>().unwrap(),
    )
}

fn drain(session: &TestSession, id: Id<()>) -> Vec<u64> {
    let (_, recv) = session.acquire(id).unwrap().lsplit();
    let mut values = Vec::new();
    loop {
        match recv.try_recv() {
            Ok(record) => {
                let heap = session.heap();
                let (_, value) = heap.open::<(), u64>(record).unwrap();
                values.push(*value);
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return values,
        }
    }
}

fn clean_attach(source: &Source, session: &TestSession, dead: Peer) {
    let attached = SessionBy::<()>::open(
        source.borrow(),
        Request::new(SIZE, Access::READ | Access::WRITE),
        REGION,
    )
    .unwrap();
    assert_eq!(
        attached.peer().slot(),
        dead.slot(),
        "dead slot was not reaped"
    );
    assert_ne!(attached.peer().generation(), dead.generation());
    let id = session.prepare(1).unwrap();
    let (send, _) = attached.acquire(id).unwrap().rsplit();
    send.try_send(attached.heap().put(VALUE + 1).unwrap().pack(()))
        .unwrap();
    assert_eq!(drain(session, id), [VALUE + 1]);
    drop(send);
    drop(attached);
    assert!(
        session.remove(id, session.acquire(id).unwrap()).is_ok(),
        "clean channel removal"
    );
}

#[test]
fn recovery_conserves_every_accepted_record() {
    child();
    for cut in Cut::ALL {
        let ready = env::temp_dir().join(format!(
            "evering-recovery-ready-{}-{}",
            std::process::id(),
            cut.name()
        ));
        let _ = fs::remove_file(&ready);
        let mut setup = setup(cut, ready);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let wait = {
            let _entered = runtime.enter();
            evering::runtime::Wait::new(setup.event).unwrap()
        };
        runtime
            .block_on(async { tokio::time::timeout(TIMEOUT, wait.ready()).await })
            .expect("advisory wait timed out")
            .unwrap();
        wait.clear().unwrap();

        let dead = peer(&setup.ready);
        fs::remove_file(&setup.ready).unwrap();
        let started = Instant::now();
        let exit = setup.child.wait().unwrap();
        assert_eq!(exit.status().code(), Some(cut.code()));
        let recovery = unsafe { setup.session.assume_exited(dead, &exit) }
            .expect("exact participant generation");
        assert!(setup.session.reap(recovery).is_ok(), "complete repair");
        let recovery_ns = started.elapsed().as_nanos();

        let values = drain(&setup.session, setup.id);
        let accepted = usize::from(!matches!(cut, Cut::Reserved));
        let validated = values.iter().filter(|&&value| value == VALUE).count();
        let fabricated = values.iter().filter(|&&value| value != VALUE).count();
        let duplicates = validated.saturating_sub(1);
        let recovered_loss = usize::from(matches!(cut, Cut::Claimed));
        assert_eq!(accepted, validated + recovered_loss);
        assert_eq!((duplicates, fabricated), (0, 0));
        println!(
            "RECOVERY\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            cut.name(),
            cut.code(),
            accepted,
            validated,
            recovered_loss,
            duplicates,
            fabricated,
            recovery_ns
        );

        clean_attach(&setup.source, &setup.session, dead);
        assert!(
            setup
                .session
                .remove(setup.id, setup.session.acquire(setup.id).unwrap())
                .is_ok(),
            "crash subject removal"
        );
    }
}
