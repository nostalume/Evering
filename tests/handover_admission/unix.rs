//! Which process a handoff will meet: one that only claims an introduction is refused, one that holds a
//! grant it never spent is refused, and the one the grant was admitted into names the peer it met.
//!
//! Delta 31 recorded the Unix handshake's missing peer attestation, and the impostor run that would
//! falsify it, as a prediction read from the code. The red run of that prediction is recorded in the
//! plan; these are the two assertions P0 is accepted on: a process that holds the announcement but
//! not the descriptor is refused before it is handed anything, and the process the grant was admitted
//! to can name the peer it met. Both now also hold the introducer's policy word — one written by the
//! claimant for itself, one written by the grant that admitted the other child — because a value that
//! can complete an unattested handover has to be shown to open no door and to close none.
//!
//! The refusals of a process that was never admitted are kept apart rather than merged because they differ in
//! what that process still holds: one spent its grant into a command that never spawned, so the granted
//! descriptor is still open in this process's own table, and the other let the command go too. Both are
//! answered at the first line of `granted_socket`, which is what the pair is for — a descriptor you were
//! handed is not an announcement made to you. They came from the file that borrowed a handoff to move a
//! region, which is not where a claim about who may meet belongs.

use std::{
    env,
    os::fd::AsRawFd,
    os::unix::net::UnixDatagram,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use evering::{
    mapping::Access,
    os::{Handoff, Provenance, shared},
    process::{Bootstrap, Supervisor},
};

use crate::support::{introduce, role};

/// The environment key a grant announces its rendezvous under, as frozen in `src/process.rs`.
const RENDEZVOUS: &str = "EVERING_RENDEZVOUS";
/// The introducer's policy key, which the claimant below writes for itself and the honest grant below
/// writes through `Grant::trusting_introduction`.
const TRUST: &str = "EVERING_TRUST";
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
        Ok((_, resources, _report)) => {
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
        Ok((peer, resources, _report)) => {
            if peer.pid() != Some(expected) {
                eprintln!(
                    "attested peer {:?} is not the expected {expected}",
                    peer.pid()
                );
                return FAILED;
            }
            if peer.provenance() != Provenance::Attested {
                eprintln!(
                    "a rendezvous that answered is reported as {:?}, and the grant's policy cannot \
                     make a kernel's silence out of an answer",
                    peer.provenance()
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

/// The roles this leg defines, kept beside their bodies. The claim root's single `role_entry` runs whichever
/// platform leg was compiled, because the harness re-enters a child by exact item name and only one item in a
/// target may answer to it.
pub(crate) const ROLES: &[(&str, role::Body)] = &[(ATTACKER, attacker), (CHILD, child)];

#[test]
fn a_claimed_rendezvous_slot_is_refused_and_the_granted_child_still_meets() {
    let (handoff, grant) = Handoff::bind().unwrap();
    let mapping = shared("attestation", SIZE, Access::READ | Access::WRITE).unwrap();
    let (_notify, child_wait) = evering::os::event().unwrap();
    let (child_ring, _event) = evering::os::event().unwrap();
    let bootstrap = Bootstrap::new(b"selected-protocol-id").unwrap();

    let mut command = role::child(ATTACKER);
    command.env(RENDEZVOUS, ANNOUNCED).env(TRUST, "1");
    let mut attacker = Supervisor::spawn(&mut command).expect("a role process spawns");

    let mut command = role::child(CHILD);
    command.env(EXPECT_PEER, std::process::id().to_string());
    grant.trusting_introduction().admit(&mut command).unwrap();
    let mut granted = Supervisor::spawn(&mut command).expect("a role process spawns");

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
        introduce::exit_code(&mut attacker, "the claimant", Duration::from_secs(10)),
        REFUSED,
        "holding the announcement is not holding the rendezvous, and a policy word written by the \
         claimant itself releases a handover no introducer ever announced"
    );
    assert_eq!(
        introduce::exit_code(&mut granted, "the granted child", Duration::from_secs(10)),
        ATTESTED,
        "the process the grant was admitted to meets and names its introducer, and the policy that \
         grant carried leaves an answer the kernel gave exactly as it was"
    );
}

#[test]
fn a_handoff_names_nothing_and_an_ungranted_process_is_refused() {
    let (handoff, grant) = Handoff::bind().unwrap();
    let mut command = Command::new(env::current_exe().unwrap());
    grant.admit(&mut command).unwrap();
    drop(handoff);
    let error = Handoff::receive()
        .err()
        .expect("a process holding no grant meets no one");
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
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
