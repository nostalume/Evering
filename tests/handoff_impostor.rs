#![cfg(all(feature = "process", unix))]

//! Delta 31 recorded the Unix handshake's missing peer attestation, and the impostor run that would
//! falsify it, as a prediction read from the code. The red run of that prediction is recorded in the
//! plan; these are the two assertions P0 is accepted on: a process that holds the announcement but
//! not the descriptor is refused before it is handed anything, and the process the grant was admitted
//! to can name the peer it met.

use std::{
    env,
    os::fd::AsRawFd,
    os::unix::net::UnixDatagram,
    path::PathBuf,
    process::Command,
    thread,
    time::{Duration, Instant},
};

use evering::{
    mapping::Access,
    os::{Handoff, shared},
    process::{Bootstrap, Supervisor},
};

/// The environment key a grant announces its rendezvous under, as frozen in `src/process.rs`.
const RENDEZVOUS: &str = "EVERING_RENDEZVOUS";
const ROLE: &str = "EVERING_IMPOSTOR_ROLE";
const ATTACKER: &str = "attacker";
const CHILD: &str = "child";
const ANNOUNCED: &str = "3";
const EXPECT_PEER: &str = "EVERING_EXPECT_PEER";
const SIZE: usize = 4096;

/// Exited by a process that was handed the region, the doorbell and the ring without being granted
/// the meeting.
const HIJACKED: i32 = 7;
/// Exited by the process the grant was admitted to, after it attested its peer.
const ATTESTED: i32 = 4;
/// Exited by a process that was refused the meeting.
const REFUSED: i32 = 0;
/// Exited by the granted process for any other reason.
const FAILED: i32 = 1;

/// The name-shaped rendezvous the handshake used before the grant replaced it.
fn predicted_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "evering-{}-0.sock",
        nix::unistd::getppid().as_raw()
    ))
}

/// The attack delta 31 describes: take over the rendezvous slot before the intended participant
/// arrives, then wait to be handed what was meant for it.
fn attacker() -> i32 {
    let path = predicted_path();
    let _ = std::fs::remove_file(&path);
    let claimed = UnixDatagram::bind(&path).expect("the attacker claims a rendezvous name");
    // SAFETY: this process runs nothing else at this point, and `dup2` is async-signal-safe. The
    // claimed descriptor is copied onto the number the announcement names, so the handshake is
    // offered a socket this process made itself instead of a granted end.
    unsafe { nix::libc::dup2(claimed.as_raw_fd(), ANNOUNCED.parse().unwrap()) };
    // The claimed end is left open on purpose: the handshake takes the announced descriptor over, so
    // closing it is the refusal's decision and a second close would close a descriptor this process
    // no longer owns.
    core::mem::forget(claimed);
    let outcome = match Handoff::receive() {
        Ok((_, resources)) => {
            eprintln!(
                "attacker was handed a {}-byte region",
                resources.mapping.size()
            );
            HIJACKED
        }
        Err(error) => {
            eprintln!("attacker was refused: {error}");
            REFUSED
        }
    };
    let _ = std::fs::remove_file(&path);
    outcome
}

fn child() -> i32 {
    let expected: u32 = env::var(EXPECT_PEER)
        .expect("the granted process is told which peer the test expects")
        .parse()
        .expect("a numeric peer id");
    match Handoff::receive() {
        Ok((peer, resources)) => {
            if peer.pid() != expected {
                eprintln!(
                    "attested peer {} is not the expected {expected}",
                    peer.pid()
                );
                return FAILED;
            }
            if resources.bootstrap.as_ref() != b"selected-protocol-id" {
                eprintln!("the granted handover carried the wrong bootstrap");
                return FAILED;
            }
            if resources.mapping.size() != SIZE {
                eprintln!("the granted handover carried the wrong region");
                return FAILED;
            }
            ATTESTED
        }
        Err(error) => {
            eprintln!("the granted process was refused: {error}");
            FAILED
        }
    }
}

fn wait_for(child: &mut Supervisor, label: &str, budget: Duration) -> i32 {
    let until = Instant::now() + budget;
    loop {
        if let Some(exit) = child.try_wait().unwrap() {
            return exit.status().code().unwrap_or(-1);
        }
        if Instant::now() >= until {
            let _ = child.kill_wait();
            panic!("{label} never finished");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn spawn(command: &mut Command, role: &str) -> Supervisor {
    command.args(["--exact", TEST]).env(ROLE, role);
    Supervisor::spawn(command).expect("a role process spawns")
}

const TEST: &str = "a_claimed_rendezvous_slot_is_refused_and_the_granted_child_still_meets";

#[test]
fn a_claimed_rendezvous_slot_is_refused_and_the_granted_child_still_meets() {
    let role = env::var(ROLE).ok();
    if role.as_deref() == Some(ATTACKER) {
        std::process::exit(attacker());
    }
    if role.as_deref() == Some(CHILD) {
        std::process::exit(child());
    }

    let (handoff, grant) = Handoff::bind().unwrap();
    let mapping = shared("attestation", SIZE, Access::READ | Access::WRITE).unwrap();
    let (_notify, child_wait) = evering::os::event().unwrap();
    let (child_ring, _event) = evering::os::event().unwrap();
    let bootstrap = Bootstrap::new(b"selected-protocol-id").unwrap();

    let mut command = Command::new(env::current_exe().unwrap());
    command.env(RENDEZVOUS, ANNOUNCED);
    let mut attacker = spawn(&mut command, ATTACKER);

    let mut command = Command::new(env::current_exe().unwrap());
    command.env(EXPECT_PEER, std::process::id().to_string());
    grant.admit(&mut command).unwrap();
    let mut granted = spawn(&mut command, CHILD);

    handoff
        .send(
            &granted,
            Instant::now() + Duration::from_secs(5),
            &bootstrap,
            &mapping,
            &child_wait,
            &child_ring,
        )
        .unwrap();

    assert_eq!(
        wait_for(&mut attacker, "the claimant", Duration::from_secs(10)),
        REFUSED,
        "holding the announcement is not holding the rendezvous, so nothing was due to be handed over"
    );
    assert_eq!(
        wait_for(&mut granted, "the granted child", Duration::from_secs(10)),
        ATTESTED,
        "the process the grant was admitted to meets and names its introducer"
    );
}

#[test]
fn an_ungranted_process_finds_no_rendezvous_to_meet_at() {
    let (handoff, grant) = Handoff::bind().unwrap();
    let mut command = Command::new(env::current_exe().unwrap());
    grant.admit(&mut command).unwrap();
    drop((handoff, command));
    // This process was never granted anything, so there is no meeting for it to receive at all.
    let error = Handoff::receive()
        .err()
        .expect("an ungranted process meets no one");
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
}
