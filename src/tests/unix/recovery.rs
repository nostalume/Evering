use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::channel::{QueueChannel, QueueOps, TryRecvError};
use crate::mem::{Access, Request};
use crate::os::unix::UnixFd;
use crate::perlude::talc::{Id, Session, SessionBy};
use crate::{Peer, RegionId};

const ROLE: &str = "EVERING_GUARD_ROLE";
const SHM: &str = "EVERING_GUARD_SHM";
const ENTRY: &str = "EVERING_GUARD_ENTRY";
const BASE: &str = "EVERING_GUARD_BASE";
const SOURCE: &str = "EVERING_GUARD_SOURCE";
const DIRECTORY_CUT: &str = "EVERING_DIRECTORY_CUT";
const TALC_CUT: &str = "EVERING_TALC_CUT";
const CLOSE_CUT: &str = "EVERING_CLOSE_CUT";
const SIZE: usize = 4 * 1024 * 1024;
const REGION: RegionId = RegionId::new(0x4755_4152_445f_5445, 1);
const TIMEOUT: Duration = Duration::from_secs(5);

type TestSession = Session<()>;

#[derive(Clone, Copy)]
enum Phase {
    Reserved,
    Staged,
    Claimed,
}

impl Phase {
    const fn role(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Staged => "staged",
            Self::Claimed => "claimed",
        }
    }
}

struct ShmGuard(String);

impl Drop for ShmGuard {
    fn drop(&mut self) {
        let _ = UnixFd::shm_unlink(&self.0);
    }
}

fn session(name: &str, create: bool, base: usize) -> TestSession {
    let fd = if create {
        UnixFd::shm_create(name, SIZE)
    } else {
        UnixFd::shm_open(name)
    }
    .expect("shared region");
    let request = Request::new(SIZE, Access::READ | Access::WRITE);
    if create {
        SessionBy::<()>::create(fd, request, REGION).expect("create session")
    } else {
        SessionBy::<()>::open(fd.mapping().at(base.wrapping_add(1 << 30)), request, REGION)
            .expect("join session")
    }
}

fn id_from_env() -> Id<()> {
    let parts: Vec<_> = std::env::var(ENTRY)
        .expect("entry")
        .split(',')
        .map(|part| part.parse::<usize>().expect("numeric entry"))
        .collect();
    Id::new(REGION, parts[0] as u32, parts[1] as u32, parts[2], parts[3])
}

fn child(phase: Phase) -> ! {
    let name = std::env::var(SHM).expect("shared region name");
    let base = std::env::var(BASE).unwrap().parse().unwrap();
    let session = session(&name, false, base);
    let view = session.acquire(id_from_env()).expect("acquire subject");
    let (send, recv) = view.rsplit();
    let peer = session.peer();
    match phase {
        Phase::Reserved => {
            let _guard = send.handle().reserve().expect("reserve");
            announce(peer);
        }
        Phase::Staged => {
            let record = session.heap().put(41_u64).expect("allocate staged value");
            let _guard = send
                .handle()
                .reserve()
                .expect("reserve")
                .stage(record.pack(()));
            announce(peer);
        }
        Phase::Claimed => {
            let _guard = recv.handle().claim().unwrap_or_else(|_| panic!("claim"));
            announce(peer);
        }
    }
}

fn announce(peer: Peer) -> ! {
    report(peer);
    std::process::exit(73)
}

fn report(peer: Peer) {
    println!("EVERING_PEER {} {}", peer.slot(), peer.generation());
    use std::io::Write;
    std::io::stdout().flush().expect("flush identity");
}

fn spawn_doomed(
    test: &str,
    role: &str,
    name: &str,
    id: Id<()>,
    base: usize,
    configure: impl FnOnce(&mut Command),
) -> (Peer, i32) {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", test, "--nocapture"])
        .env(SHM, name)
        .env(
            ENTRY,
            format!(
                "{},{},{},{}",
                id.slab(),
                id.entry(),
                id.generation(),
                id.capacity()
            ),
        )
        .env(BASE, base.to_string())
        .stdout(Stdio::piped());
    command.env(ROLE, role);
    configure(&mut command);
    let mut child = command.spawn().expect("spawn doomed participant");
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().expect("observe child") {
            break status;
        }
        assert!(Instant::now() < deadline, "child did not terminate");
        thread::yield_now();
    };
    let mut output = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    let marker = output
        .lines()
        .find(|line| line.starts_with("EVERING_PEER "))
        .expect("reported peer");
    let mut identity = marker.split_whitespace().skip(1);
    (
        Peer::from_parts(
            identity.next().unwrap().parse().unwrap(),
            identity.next().unwrap().parse().unwrap(),
        ),
        status.code().expect("signal-free process exit"),
    )
}

fn crash_case(test: &str, phase: Phase) {
    if std::env::var(ROLE).as_deref() == Ok(phase.role()) {
        child(phase);
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("evering-guard-{}-{nonce}", std::process::id());
    let _shm = ShmGuard(name.clone());
    let session = session(&name, true, 0);
    let id = session.prepare(4).expect("prepare subject");
    if matches!(phase, Phase::Claimed) {
        let view = session.acquire(id).unwrap();
        let (send, _) = view.lsplit();
        let record = session.heap().put(41_u64).unwrap();
        send.try_send(record.pack(())).unwrap();
    }
    let (peer, status) = spawn_doomed(test, phase.role(), &name, id, session.base_addr(), |_| {});
    assert_eq!(status, 73);
    let recovery = unsafe { session.assume_dead(peer) }.expect("exact dead generation");
    assert!(session.reap(recovery).is_ok(), "repair guarded queue phase");

    match phase {
        Phase::Staged => assert_eq!(receive(&session, id), 41),
        Phase::Reserved => {
            let view = session.acquire(id).unwrap();
            let (_, recv) = view.lsplit();
            assert!(matches!(recv.try_recv(), Err(TryRecvError::Empty)));
            drop(recv);
            send_right(&session, id, 42);
            assert_eq!(receive(&session, id), 42);
        }
        Phase::Claimed => {
            let view = session.acquire(id).unwrap();
            let (send, _) = view.lsplit();
            let record = session.heap().put(42_u64).unwrap();
            send.try_send(record.pack(())).unwrap();
            drop(send);
            let view = session.acquire(id).unwrap();
            let (_, recv) = view.rsplit();
            assert_eq!(open(&session, recv.try_recv().unwrap()), 42);
        }
    }
}

fn send_right(session: &TestSession, id: Id<()>, value: u64) {
    let view = session.acquire(id).unwrap();
    let (send, _) = view.rsplit();
    let record = session.heap().put(value).unwrap();
    send.try_send(record.pack(())).unwrap();
}

fn receive(session: &TestSession, id: Id<()>) -> u64 {
    let view = session.acquire(id).unwrap();
    let (_, recv) = view.lsplit();
    open(session, recv.try_recv().unwrap())
}

fn open(session: &TestSession, record: crate::token::PackToken<(), crate::talc::Meta>) -> u64 {
    let heap = session.heap();
    let (_, value) = heap.open::<(), u64>(record).unwrap();
    *value
}

fn reaper_child() -> ! {
    let name = std::env::var(SHM).unwrap();
    let base = std::env::var(BASE).unwrap().parse().unwrap();
    let source_env = std::env::var(SOURCE).unwrap();
    let mut source = source_env
        .split(',')
        .map(|part| part.parse::<usize>().expect("numeric source participant"));
    let source = Peer::from_parts(source.next().unwrap() as u8, source.next().unwrap());
    let session = session(&name, false, base);
    let peer = session.peer();
    report(peer);
    crate::channel::exit_after_reaper(peer.slot());
    let recovery = unsafe { session.assume_dead(source) }.expect("dead source");
    let _ = session.reap(recovery);
    std::process::exit(75)
}

fn directory_create_child() -> ! {
    let name = std::env::var(SHM).unwrap();
    let base = std::env::var(BASE).unwrap().parse().unwrap();
    let cut = std::env::var(DIRECTORY_CUT).unwrap().parse().unwrap();
    let session = session(&name, false, base);
    report(session.peer());
    crate::dir::crash_after_for_test(cut);
    let _ = session.prepare(4);
    std::process::exit(99)
}

fn directory_remove_child() -> ! {
    let name = std::env::var(SHM).unwrap();
    let base = std::env::var(BASE).unwrap().parse().unwrap();
    let cut = std::env::var(DIRECTORY_CUT).unwrap().parse().unwrap();
    let session = session(&name, false, base);
    let id = id_from_env();
    let view = session.acquire(id).expect("acquire removal target");
    report(session.peer());
    crate::dir::crash_after_for_test(cut);
    let _ = session.remove(id, view);
    std::process::exit(99)
}

fn directory_grow_child() -> ! {
    let name = std::env::var(SHM).unwrap();
    let base = std::env::var(BASE).unwrap().parse().unwrap();
    let cut = std::env::var(DIRECTORY_CUT).unwrap().parse().unwrap();
    let session = session(&name, false, base);
    report(session.peer());
    crate::dir::crash_after_for_test(cut);
    let _ = session.prepare(4);
    std::process::exit(99)
}

fn talc_child(release: bool) -> ! {
    let name = std::env::var(SHM).unwrap();
    let base = std::env::var(BASE).unwrap().parse().unwrap();
    let cut = std::env::var(TALC_CUT).unwrap().parse().unwrap();
    let session = session(&name, false, base);
    report(session.peer());
    if release {
        let heap = session.heap();
        let record = heap.put(42_u64).unwrap();
        let (_, value) = heap.open::<(), u64>(record.pack(())).unwrap();
        crate::talc::crash_after_for_test(cut);
        let _ = value.release();
    } else {
        crate::talc::crash_after_for_test(cut);
        let _ = session.heap().put(42_u64);
    }
    std::process::exit(99)
}

fn close_child(send_side: bool) -> ! {
    let name = std::env::var(SHM).unwrap();
    let base = std::env::var(BASE).unwrap().parse().unwrap();
    let cut = std::env::var(CLOSE_CUT).unwrap().parse().unwrap();
    let session = session(&name, false, base);
    let view = session.acquire(id_from_env()).unwrap();
    let (send, recv) = view.rsplit();
    report(session.peer());
    crate::channel::crash_close_for_test(cut);
    if send_side {
        send.close();
        let _ = send.handle().send_closed();
    } else {
        recv.close();
        let _ = recv.handle().recv_closed();
    }
    std::process::exit(99)
}

#[test]
fn process_death_while_reserved_recovers_an_ordered_skip() {
    crash_case(
        "tests::unix::recovery::process_death_while_reserved_recovers_an_ordered_skip",
        Phase::Reserved,
    );
}

#[test]
fn process_death_while_staged_publishes_the_committed_record() {
    crash_case(
        "tests::unix::recovery::process_death_while_staged_publishes_the_committed_record",
        Phase::Staged,
    );
}

#[test]
fn process_death_while_claimed_does_not_redeliver_the_record() {
    crash_case(
        "tests::unix::recovery::process_death_while_claimed_does_not_redeliver_the_record",
        Phase::Claimed,
    );
}

#[test]
fn a_second_process_takes_over_after_the_first_reaper_dies() {
    const TEST: &str =
        "tests::unix::recovery::a_second_process_takes_over_after_the_first_reaper_dies";
    match std::env::var(ROLE).as_deref() {
        Ok("takeover-source") => child(Phase::Reserved),
        Ok("takeover-reaper") => reaper_child(),
        _ => {}
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("evering-takeover-{}-{nonce}", std::process::id());
    let _shm = ShmGuard(name.clone());
    let session = session(&name, true, 0);
    let id = session.prepare(4).unwrap();
    let (source, status) = spawn_doomed(
        TEST,
        "takeover-source",
        &name,
        id,
        session.base_addr(),
        |_| {},
    );
    assert_eq!(status, 73);
    let (reaper, status) = spawn_doomed(
        TEST,
        "takeover-reaper",
        &name,
        id,
        session.base_addr(),
        |command| {
            command.env(SOURCE, format!("{},{}", source.slot(), source.generation()));
        },
    );
    assert_eq!(status, 74, "first reaper died after its durable claim");

    let source_recovery = unsafe { session.assume_dead(source) }.unwrap();
    let source_recovery = session
        .reap(source_recovery)
        .expect_err("live protocol still names the dead first reaper");
    let reaper_recovery = unsafe { session.assume_dead(reaper) }.unwrap();
    assert!(session.reap(reaper_recovery).is_ok());
    assert!(session.reap(source_recovery).is_ok());

    let view = session.acquire(id).unwrap();
    let (_, recv) = view.lsplit();
    assert!(matches!(recv.try_recv(), Err(TryRecvError::Empty)));
    drop(recv);
    send_right(&session, id, 42);
    assert_eq!(receive(&session, id), 42);
}

#[test]
fn directory_create_crash_windows_follow_durable_evidence() {
    const TEST: &str =
        "tests::unix::recovery::directory_create_crash_windows_follow_durable_evidence";
    if std::env::var(ROLE).as_deref() == Ok("directory-create") {
        directory_create_child();
    }
    use crate::dir::{
        CREATE_ALLOCATED, CREATE_EVIDENCE, CREATE_HEAP_CLEAN, CREATE_PENDING, CREATE_PUBLISHED,
    };
    for cut in [
        CREATE_PENDING,
        CREATE_ALLOCATED,
        CREATE_EVIDENCE,
        CREATE_HEAP_CLEAN,
        CREATE_PUBLISHED,
    ] {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("evering-create-{}-{cut}-{nonce}", std::process::id());
        let _shm = ShmGuard(name.clone());
        let session = session(&name, true, 0);
        let dummy = Id::new(REGION, 0, 0, 0, 1);
        let (peer, status) = spawn_doomed(
            TEST,
            "directory-create",
            &name,
            dummy,
            session.base_addr(),
            |command| {
                command.env(DIRECTORY_CUT, cut.to_string());
            },
        );
        assert_eq!(status, 80 + cut as i32);
        let recovery = unsafe { session.assume_dead(peer) }.unwrap();
        let result = session.reap(recovery);
        if matches!(cut, CREATE_ALLOCATED | CREATE_EVIDENCE) {
            assert!(result.is_err(), "ambiguous heap mutation stays fail-stop");
            continue;
        }
        assert!(result.is_ok(), "recoverable create cut {cut}");
        let id = session.prepare(4).expect("clean heap remains usable");
        assert_eq!(
            id.entry(),
            u32::from(cut == CREATE_PUBLISHED),
            "only a published-but-uncommitted layout is quarantined"
        );
    }
}

#[test]
fn directory_remove_crash_windows_never_deallocate_twice() {
    const TEST: &str =
        "tests::unix::recovery::directory_remove_crash_windows_never_deallocate_twice";
    if std::env::var(ROLE).as_deref() == Ok("directory-remove") {
        directory_remove_child();
    }
    use crate::dir::{
        REMOVE_CLOSED, REMOVE_CLOSING, REMOVE_DEALLOCATED, REMOVE_HEAP_CLEAN, REMOVE_RELEASED,
        REMOVE_REMOVING, REMOVE_VACANT,
    };
    for cut in [
        REMOVE_CLOSING,
        REMOVE_CLOSED,
        REMOVE_REMOVING,
        REMOVE_DEALLOCATED,
        REMOVE_RELEASED,
        REMOVE_HEAP_CLEAN,
        REMOVE_VACANT,
    ] {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("evering-remove-{}-{cut}-{nonce}", std::process::id());
        let _shm = ShmGuard(name.clone());
        let session = session(&name, true, 0);
        let id = session.prepare(4).unwrap();
        let (peer, status) = spawn_doomed(
            TEST,
            "directory-remove",
            &name,
            id,
            session.base_addr(),
            |command| {
                command.env(DIRECTORY_CUT, cut.to_string());
            },
        );
        assert_eq!(status, 80 + cut as i32);
        let recovery = unsafe { session.assume_dead(peer) }.unwrap();
        let result = session.reap(recovery);
        if matches!(cut, REMOVE_REMOVING | REMOVE_DEALLOCATED) {
            assert!(result.is_err(), "ambiguous deallocation stays fail-stop");
            continue;
        }
        assert!(result.is_ok(), "recoverable removal cut {cut}");
        if cut == REMOVE_CLOSING {
            assert!(session.acquire(id).is_some(), "pre-close layout survives");
        } else {
            assert!(session.acquire(id).is_none(), "closed layout stays removed");
            let replacement = session.prepare(4).expect("clean heap remains usable");
            assert_eq!(
                replacement.entry(),
                0,
                "posterior removal evidence must not quarantine a vacant entry"
            );
            assert_eq!(replacement.generation(), id.generation() + 1);
        }
    }
}

#[test]
fn directory_growth_crash_windows_preserve_the_slab_chain() {
    const TEST: &str =
        "tests::unix::recovery::directory_growth_crash_windows_preserve_the_slab_chain";
    if std::env::var(ROLE).as_deref() == Ok("directory-grow") {
        directory_grow_child();
    }
    use crate::dir::{GROW_ALLOCATED, GROW_CLEARED, GROW_EVIDENCE, GROW_HEAP_CLEAN, GROW_LINKED};
    for cut in [
        GROW_ALLOCATED,
        GROW_EVIDENCE,
        GROW_HEAP_CLEAN,
        GROW_LINKED,
        GROW_CLEARED,
    ] {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("evering-grow-{}-{cut}-{nonce}", std::process::id());
        let _shm = ShmGuard(name.clone());
        let session = session(&name, true, 0);
        for _ in 0..32 {
            session.prepare(4).expect("fill root slab");
        }
        let dummy = Id::new(REGION, 0, 0, 0, 1);
        let (peer, status) = spawn_doomed(
            TEST,
            "directory-grow",
            &name,
            dummy,
            session.base_addr(),
            |command| {
                command.env(DIRECTORY_CUT, cut.to_string());
            },
        );
        assert_eq!(status, 80 + cut as i32);
        let recovery = unsafe { session.assume_dead(peer) }.unwrap();
        let result = session.reap(recovery);
        if matches!(cut, GROW_ALLOCATED | GROW_EVIDENCE) {
            assert!(result.is_err(), "ambiguous allocation stays fail-stop");
            continue;
        }
        assert!(result.is_ok(), "recoverable growth cut {cut}");
        let id = session.prepare(4).expect("recovered Directory can grow");
        assert_eq!(id.slab(), 1, "growth resumes in the second slab");
        assert_eq!(id.entry(), 0, "no second-slab entry is lost");
    }
}

#[test]
fn standalone_talc_crash_windows_are_fail_stop_or_clean() {
    const TEST: &str =
        "tests::unix::recovery::standalone_talc_crash_windows_are_fail_stop_or_clean";
    match std::env::var(ROLE).as_deref() {
        Ok("talc-allocate") => talc_child(false),
        Ok("talc-release") => talc_child(true),
        _ => {}
    }
    use crate::talc::{
        ALLOC_CLAIMED, ALLOC_CLEAN, ALLOC_MUTATED, FREE_CLAIMED, FREE_CLEAN, FREE_MUTATED,
    };
    for (role, cut) in [
        ("talc-allocate", ALLOC_CLAIMED),
        ("talc-allocate", ALLOC_MUTATED),
        ("talc-allocate", ALLOC_CLEAN),
        ("talc-release", FREE_CLAIMED),
        ("talc-release", FREE_MUTATED),
        ("talc-release", FREE_CLEAN),
    ] {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("evering-talc-{}-{cut}-{nonce}", std::process::id());
        let _shm = ShmGuard(name.clone());
        let session = session(&name, true, 0);
        let dummy = Id::new(REGION, 0, 0, 0, 1);
        let (peer, status) =
            spawn_doomed(TEST, role, &name, dummy, session.base_addr(), |command| {
                command.env(TALC_CUT, cut.to_string());
            });
        assert_eq!(status, 120 + cut as i32);
        let recovery = unsafe { session.assume_dead(peer) }.unwrap();
        let result = session.reap(recovery);
        if matches!(
            cut,
            ALLOC_CLAIMED | ALLOC_MUTATED | FREE_CLAIMED | FREE_MUTATED
        ) {
            assert!(result.is_err(), "ambiguous Talc mutation stays fail-stop");
            continue;
        }
        assert!(result.is_ok(), "clean Talc cut {cut} is recoverable");
        assert!(
            session.heap().put(7_u64).is_ok(),
            "clean Talc remains usable"
        );
    }
}

#[test]
fn close_gate_crash_windows_follow_the_gate_word() {
    const TEST: &str = "tests::unix::recovery::close_gate_crash_windows_follow_the_gate_word";
    match std::env::var(ROLE).as_deref() {
        Ok("close-send") => close_child(true),
        Ok("close-recv") => close_child(false),
        _ => {}
    }
    use crate::channel::{
        RECV_CLOSED, RECV_CLOSING, RECV_FINISHED, SEND_CLOSED, SEND_CLOSING, SEND_FINISHED,
    };
    for (role, cut) in [
        ("close-send", SEND_CLOSING),
        ("close-send", SEND_CLOSED),
        ("close-send", SEND_FINISHED),
        ("close-recv", RECV_CLOSING),
        ("close-recv", RECV_CLOSED),
        ("close-recv", RECV_FINISHED),
    ] {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("evering-close-{}-{cut}-{nonce}", std::process::id());
        let _shm = ShmGuard(name.clone());
        let session = session(&name, true, 0);
        let id = session.prepare(4).unwrap();
        let (peer, status) = spawn_doomed(TEST, role, &name, id, session.base_addr(), |command| {
            command.env(CLOSE_CUT, cut.to_string());
        });
        assert_eq!(status, 140 + cut as i32);
        let recovery = unsafe { session.assume_dead(peer) }.unwrap();
        assert!(session.reap(recovery).is_ok());
        let view = session.acquire(id).unwrap();
        let (_, opposite_recv) = view.clone().lsplit();
        let (send, recv) = view.rsplit();
        if role == "close-send" {
            let record = session.heap().put(42_u64).unwrap().pack(());
            let result = send.try_send(record);
            if cut == SEND_CLOSING {
                assert!(
                    result.is_ok(),
                    "death before gate publication leaves it open"
                );
                assert_eq!(open(&session, opposite_recv.try_recv().unwrap()), 42);
            } else {
                let record = match result {
                    Err(crate::channel::TrySendError::Disconnected(record)) => record,
                    _ => panic!("published send gate rejects new sends"),
                };
                let _ = session.heap().discard(record);
            }
        } else if cut == RECV_CLOSING {
            assert!(matches!(recv.try_recv(), Err(TryRecvError::Empty)));
        } else {
            assert!(matches!(recv.try_recv(), Err(TryRecvError::Disconnected)));
        }
    }
}
