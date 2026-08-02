use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::mem::{Access, Request};
use crate::os::unix::UnixFd;
use crate::{ChannelId as Id, Session};
use crate::{mem::Peer, schema::RegionId};

const ROLE: &str = "EVERING_GUARD_ROLE";
const SHM: &str = "EVERING_GUARD_SHM";
const ENTRY: &str = "EVERING_GUARD_ENTRY";
const BASE: &str = "EVERING_GUARD_BASE";
const DIRECTORY_CUT: &str = "EVERING_DIRECTORY_CUT";
const TALC_CUT: &str = "EVERING_TALC_CUT";
const TRANSFER: &str = "EVERING_TRANSFER";
const SIZE: usize = 4 * 1024 * 1024;
const REGION: RegionId = RegionId::new(0x4755_4152_445f_5445, 1);
const TIMEOUT: Duration = Duration::from_secs(5);

type TestSession = Session;

fn create_id(session: &TestSession, capacity: usize) -> Option<Id<()>> {
    let (channel, _) = session.create_channel::<()>(capacity).ok()?;
    let id = channel.id();
    drop(channel);
    Some(id)
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
        Session::create(fd, request, REGION).expect("create session")
    } else {
        Session::open(fd.mapping().at(base.wrapping_add(1 << 30)), request, REGION)
            .expect("join session")
    }
}

fn report(peer: Peer) {
    println!("EVERING_PEER {} {}", peer.slot(), peer.generation());
    use std::io::Write;
    std::io::stdout().flush().expect("flush identity");
}

fn entry() -> Id<()> {
    let value = std::env::var(ENTRY).unwrap();
    let mut parts = value.split(',').map(|part| part.parse::<usize>().unwrap());
    Id::new(
        REGION,
        parts.next().unwrap() as u32,
        parts.next().unwrap() as u32,
        parts.next().unwrap(),
        parts.next().unwrap(),
    )
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

fn directory_create_child() -> ! {
    let name = std::env::var(SHM).unwrap();
    let base = std::env::var(BASE).unwrap().parse().unwrap();
    let cut = std::env::var(DIRECTORY_CUT).unwrap().parse().unwrap();
    let session = session(&name, false, base);
    report(session.peer());
    crate::dir::crash_after_for_test(cut);
    let _ = session.create_channel::<()>(4);
    std::process::exit(99)
}

fn directory_remove_child() -> ! {
    let name = std::env::var(SHM).unwrap();
    let base = std::env::var(BASE).unwrap().parse().unwrap();
    let cut = std::env::var(DIRECTORY_CUT).unwrap().parse().unwrap();
    let session = session(&name, false, base);
    let (channel, _) = session
        .create_channel::<()>(4)
        .expect("create removal target");
    report(session.peer());
    crate::dir::crash_after_for_test(cut);
    let _ = session.remove(channel);
    std::process::exit(99)
}

fn directory_grow_child() -> ! {
    let name = std::env::var(SHM).unwrap();
    let base = std::env::var(BASE).unwrap().parse().unwrap();
    let cut = std::env::var(DIRECTORY_CUT).unwrap().parse().unwrap();
    let session = session(&name, false, base);
    report(session.peer());
    crate::dir::crash_after_for_test(cut);
    let _ = session.create_channel::<()>(4);
    std::process::exit(99)
}

fn talc_child(release: bool) -> ! {
    let name = std::env::var(SHM).unwrap();
    let base = std::env::var(BASE).unwrap().parse().unwrap();
    let cut = std::env::var(TALC_CUT).unwrap().parse().unwrap();
    let session = session(&name, false, base);
    report(session.peer());
    if release {
        let record = session.heap().put(42_u64).unwrap();
        crate::talc::crash_after_for_test(cut);
        drop(record);
    } else {
        crate::talc::crash_after_for_test(cut);
        let _ = session.heap().put(42_u64);
    }
    std::process::exit(99)
}

fn transfer_source_child() -> ! {
    let session = session(
        &std::env::var(SHM).unwrap(),
        false,
        std::env::var(BASE).unwrap().parse().unwrap(),
    );
    let value = std::env::var(TRANSFER).unwrap();
    let mut parts = value.split(',').map(|part| part.parse::<usize>().unwrap());
    let port = crate::Port::from_parts(entry(), parts.next().unwrap() as u8, parts.next().unwrap())
        .unwrap();
    let pool = crate::PoolId::new(
        REGION,
        parts.next().unwrap() as u32,
        parts.next().unwrap() as u32,
        parts.next().unwrap(),
    );
    report(session.peer());
    let channel = session.adopt(port).unwrap();
    let pool = session.open_pool(pool).unwrap();
    let (send, _) = channel.split();
    let _staged = send
        .reserve()
        .unwrap()
        .stage(pool.as_ref().put(41_u64).unwrap().transfer(()));
    std::process::exit(96)
}

fn transfer_reaper_child() -> ! {
    let session = session(
        &std::env::var(SHM).unwrap(),
        false,
        std::env::var(BASE).unwrap().parse().unwrap(),
    );
    let value = std::env::var(TRANSFER).unwrap();
    let mut parts = value.split(',').map(|part| part.parse::<usize>().unwrap());
    let source = Peer::from_parts(parts.next().unwrap() as u8, parts.next().unwrap());
    report(session.peer());
    let recovery = unsafe { session.assume_dead(source) }.unwrap();
    crate::queue::crash_repair_for_test();
    let _ = recovery.reap_with(&[crate::layout::recovery_handler::<()>()]);
    std::process::exit(99)
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
        let result = recovery.reap_with(&[crate::layout::recovery_handler::<()>()]);
        if matches!(cut, CREATE_ALLOCATED | CREATE_EVIDENCE) {
            assert!(result.is_ok(), "poisoned heap must not block reap");
            assert!(
                session.create_channel::<()>(4).is_err(),
                "poison forbids new mutation"
            );
            continue;
        }
        assert!(result.is_ok(), "recoverable create cut {cut}");
        let id = create_id(&session, 4).expect("clean heap remains usable");
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
        let id = Id::new(REGION, 0, 0, 1, 4);
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
        let result = recovery.reap_with(&[crate::layout::recovery_handler::<()>()]);
        if matches!(cut, REMOVE_REMOVING | REMOVE_DEALLOCATED) {
            assert!(result.is_ok(), "poisoned heap must not block reap");
            assert!(
                session.create_channel::<()>(4).is_err(),
                "poison forbids new mutation"
            );
            continue;
        }
        assert!(result.is_ok(), "recoverable removal cut {cut}");
        if cut == REMOVE_CLOSING {
            assert!(session.contains_channel(id), "pre-close layout survives");
        } else {
            assert!(!session.contains_channel(id), "closed layout stays removed");
            let replacement = create_id(&session, 4).expect("clean heap remains usable");
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
            create_id(&session, 4).expect("fill root slab");
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
        let result = recovery.reap_with(&[crate::layout::recovery_handler::<()>()]);
        if matches!(cut, GROW_ALLOCATED | GROW_EVIDENCE) {
            assert!(result.is_ok(), "poisoned heap must not block reap");
            assert!(
                session.create_channel::<()>(4).is_err(),
                "poison forbids new mutation"
            );
            continue;
        }
        assert!(result.is_ok(), "recoverable growth cut {cut}");
        let id = create_id(&session, 4).expect("recovered Directory can grow");
        assert_eq!(id.slab(), 1, "growth resumes in the second slab");
        assert_eq!(id.entry(), 0, "no second-slab entry is lost");
    }
}

#[test]
fn standalone_talc_crash_windows_poison_mutation_without_blocking_reap() {
    const TEST: &str = "tests::unix::recovery::standalone_talc_crash_windows_poison_mutation_without_blocking_reap";
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
        let record = session.heap().put(7_u64).unwrap();
        let dummy = Id::new(REGION, 0, 0, 0, 1);
        let (peer, status) =
            spawn_doomed(TEST, role, &name, dummy, session.base_addr(), |command| {
                command.env(TALC_CUT, cut.to_string());
            });
        assert_eq!(status, 120 + cut as i32);
        let recovery = unsafe { session.assume_dead(peer) }.unwrap();
        let result = recovery.reap_with(&[crate::layout::recovery_handler::<()>()]);
        if matches!(
            cut,
            ALLOC_CLAIMED | ALLOC_MUTATED | FREE_CLAIMED | FREE_MUTATED
        ) {
            assert!(result.is_ok(), "poisoned heap must not block reap");
            let error = match session.heap().put(8_u64) {
                Err(error) => error,
                Ok(_) => panic!("poison admitted a new mutation"),
            };
            assert_eq!(error.error, crate::talc::MutationError::Poisoned);
            assert_eq!(*record, 7, "pre-existing data remains readable");
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
fn a_second_reaper_finishes_the_original_transfer_owner() {
    const TEST: &str =
        "tests::unix::recovery::a_second_reaper_finishes_the_original_transfer_owner";
    match std::env::var(ROLE).as_deref() {
        Ok("transfer-source") => transfer_source_child(),
        Ok("transfer-reaper") => transfer_reaper_child(),
        _ => {}
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("evering-transfer-{}-{nonce}", std::process::id());
    let _shm = ShmGuard(name.clone());
    let session = session(&name, true, 0);
    let (channel, port) = session.create_channel::<()>(1).unwrap();
    let pool_id = session.create_pool(64 * 1024, None).unwrap().id();
    let (_, pool_slab, pool_entry, pool_generation) = pool_id.parts();
    let (source, status) = spawn_doomed(
        TEST,
        "transfer-source",
        &name,
        channel.id(),
        session.base_addr(),
        |command| {
            command.env(
                TRANSFER,
                format!(
                    "{},{},{pool_slab},{pool_entry},{pool_generation}",
                    port.role(),
                    port.generation()
                ),
            );
        },
    );
    assert_eq!(status, 96);
    drop(unsafe { session.assume_dead(source) }.unwrap());

    let (reaper, status) = spawn_doomed(
        TEST,
        "transfer-reaper",
        &name,
        channel.id(),
        session.base_addr(),
        |command| {
            command.env(
                TRANSFER,
                format!("{},{}", source.slot(), source.generation()),
            );
        },
    );
    assert_eq!(status, 95);
    let recovery = unsafe { session.assume_dead(reaper) }.unwrap();
    assert!(
        recovery
            .reap_with(&[crate::layout::recovery_handler::<()>()])
            .is_ok()
    );
    let recovery = unsafe { session.assume_dead(source) }.unwrap();
    assert!(
        recovery
            .reap_with(&[crate::layout::recovery_handler::<()>()])
            .is_ok()
    );

    let (_, receive) = channel.split();
    assert!(matches!(receive.claim(), Err(crate::ReceiveError::Busy)));
    assert!(matches!(receive.claim(), Err(crate::ReceiveError::Empty)));
    let pool = session.open_pool(pool_id).unwrap();
    assert_eq!(*pool.as_ref().put(43_u64).unwrap(), 43);
}
