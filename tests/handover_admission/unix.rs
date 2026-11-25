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
//!
//! The other two legs measure the route that needs no announcement at all. `Handoff::granted` is met by a host
//! that places a descriptor and writes none of this crate's keys — a shell's redirection, a socket a service
//! manager opened, a supervisor with a convention of its own — so the placed leg installs the granted end by
//! `dup2` and meets, while the placebo leg hands a pipe to *both* entrances and requires one answer from both.
//! That pair is what the weld is for and what falsifies it: a descriptor that has to be inherited cannot be
//! claimed, and a descriptor is only ever asked one question about whether it is a rendezvous.

use std::{
    env,
    os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd},
    os::unix::net::UnixDatagram,
    os::unix::process::CommandExt,
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
/// The process whose rendezvous was placed in its table by the host, with no announcement of it.
const PLACED: &str = "placed";
/// The process handed a descriptor that names no rendezvous, both ways one can arrive.
const PLACEBO: &str = "placebo";
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
        Ok((provenance, resources, _report)) => {
            let Provenance::Attested { pid } = provenance else {
                eprintln!(
                    "a rendezvous that answered is reported as {provenance:?}, and the grant's policy \
                     cannot make a kernel's silence out of an answer"
                );
                return FAILED;
            };
            if pid != expected {
                eprintln!("attested peer {pid} is not the expected {expected}");
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

/// The host's own entrance taken: a rendezvous that was placed in this process's table and never announced.
///
/// Nothing here reads `EVERING_RENDEZVOUS`. The driver installed the granted end at `ANNOUNCED` and said no
/// word about it, which is the shape of a host that speaks no evering protocol — a shell redirection, a
/// socket a service manager opened, a supervisor with its own convention. What the meeting is answered by
/// the descriptor and the kernel, exactly as an announced one is.
fn placed() -> i32 {
    let expected: u32 = env::var(EXPECT_PEER)
        .expect("the placed process is told which peer the test expects")
        .parse()
        .expect("a numeric peer id");
    // SAFETY: the driver dup2'd the granted end onto this number between fork and exec and owns nothing
    // of it afterwards, so this process is the only owner and takes the single-use grant over whole.
    let rendezvous = unsafe { OwnedFd::from_raw_fd(ANNOUNCED.parse().unwrap()) };
    match Handoff::granted(rendezvous) {
        Ok((provenance, resources, _report)) => {
            let Provenance::Attested { pid } = provenance else {
                eprintln!("a placed rendezvous that answered is reported as {provenance:?}");
                return FAILED;
            };
            if pid != expected {
                eprintln!("the peer behind a placed rendezvous is {pid}, not {expected}");
                return FAILED;
            }
            if resources.bootstrap.as_ref() != b"selected-protocol-id" {
                eprintln!("the placed handover carried the wrong bootstrap");
                return FAILED;
            }
            if resources.mapping.size() != SIZE {
                eprintln!("the placed handover carried the wrong region");
                return FAILED;
            }
            if resources.seat.is_some() {
                eprintln!(
                    "a handover nobody announced a slot into reported {:?} as its seat",
                    resources.seat
                );
                return FAILED;
            }
            ATTESTED
        }
        Err(error) => {
            eprintln!("the placed rendezvous was refused: {error}");
            FAILED
        }
    }
}

/// The same false descriptor both ways it can arrive, measured against each other.
///
/// One pipe end is handed over as the rendezvous; the other is installed at the number an announcement
/// names. Both are refused, and what this role checks is not that they are refused — the attacker above
/// already shows that — but that they are refused *alike*, which is the claim that a port has one question
/// to answer about a descriptor rather than one per entrance.
fn placebo() -> i32 {
    let (handed, announced) =
        nix::unistd::pipe().expect("a role makes two pipes it holds no rendezvous in");
    let handed_over = match Handoff::granted(handed) {
        Ok(_) => {
            eprintln!("a pipe was admitted as a rendezvous");
            return FAILED;
        }
        Err(error) => error,
    };
    // SAFETY: this process runs nothing else at this point, and `dup2` is async-signal-safe. The pipe end
    // is copied onto the number the driver announced, so the announced leg is offered the same kind of
    // thing the attacker offered it: a descriptor this process made for itself.
    unsafe { nix::libc::dup2(announced.as_raw_fd(), ANNOUNCED.parse().unwrap()) };
    // Left open on purpose: the announced leg classifies a claim through a borrow and closes nothing, so a
    // close here would say the refusal had handed the descriptor back before it had read it.
    core::mem::forget(announced);
    let announced = match Handoff::receive() {
        Ok(_) => {
            eprintln!("an announced pipe was admitted as a rendezvous");
            return FAILED;
        }
        Err(error) => error,
    };
    if handed_over.kind() != announced.kind() || handed_over.to_string() != announced.to_string() {
        eprintln!("handed over: {handed_over}\nannounced: {announced}");
        return FAILED;
    }
    REFUSED
}

/// The roles this leg defines, kept beside their bodies. The claim root's single `role_entry` runs whichever
/// platform leg was compiled, because the harness re-enters a child by exact item name and only one item in a
/// target may answer to it.
pub(crate) const ROLES: &[(&str, role::Body)] = &[
    (ATTACKER, attacker),
    (CHILD, child),
    (PLACED, placed),
    (PLACEBO, placebo),
];

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
            child_wait,
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

/// Weld A: the entrance for a host that places the descriptor and writes no announcement.
///
/// The grant is spent the way a shell's redirection or a service manager's socket spends it — `dup2` into
/// the number the launcher and its child agree on, and nothing in the environment. What the child answers
/// with is the same triple `receive` returns, from the same kernel answer about who owns the other end, so
/// the only thing this route gives up is the announcement, which is the one thing a foreign host cannot
/// make correctly anyway.
#[test]
fn a_rendezvous_the_host_placed_by_hand_is_admitted_without_an_announcement() {
    let (handoff, grant) = Handoff::bind().unwrap();
    let mapping = shared("placed", SIZE, Access::READ | Access::WRITE).unwrap();
    let (_notify, child_wait) = evering::os::event().unwrap();
    let (child_ring, _event) = evering::os::event().unwrap();
    let bootstrap = Bootstrap::new(b"selected-protocol-id").unwrap();

    let mut command = role::child(PLACED);
    command.env(EXPECT_PEER, std::process::id().to_string());
    let placed = grant;
    // SAFETY: the hook runs between fork and exec in the child and calls only `dup2`, which is
    // async-signal-safe. `placed` is moved into it so the granted end stays open until the command
    // spawns; no `EVERING_*` key is written, because this child is met through the descriptor alone.
    unsafe {
        command.pre_exec(move || {
            let here = placed.as_fd().as_raw_fd();
            if nix::libc::dup2(here, ANNOUNCED.parse().unwrap()) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut granted = Supervisor::spawn(&mut command).expect("a role process spawns");

    handoff
        .send(
            &granted,
            Instant::now() + Duration::from_secs(5),
            &bootstrap,
            &mapping,
            child_wait,
            &child_ring,
        )
        .unwrap();

    assert_eq!(
        introduce::exit_code(&mut granted, "the placed child", Duration::from_secs(10)),
        ATTESTED,
        "a descriptor the host installed is a granted rendezvous whether or not anybody announced it, and \
         the handover behind it carries the same offer and the same peer the announced route answers with"
    );
}

/// The one question a descriptor is asked, asked from both entrances.
///
/// A port that classified an announced number and an owned descriptor separately could accept a rendezvous
/// on one route and refuse it on the other, which is the disagreement the two entrances exist to avoid. This
/// leg hands the same kind of impostor — a pipe — to both and requires the same answer from both.
#[test]
fn a_descriptor_names_no_rendezvous_the_same_way_handed_over_or_announced() {
    let mut command = role::child(PLACEBO);
    command.env(RENDEZVOUS, ANNOUNCED);
    let mut placebo = Supervisor::spawn(&mut command).expect("a role process spawns");

    assert_eq!(
        introduce::exit_code(
            &mut placebo,
            "the child holding two pipes",
            Duration::from_secs(10)
        ),
        REFUSED,
        "neither entrance takes a pipe for a rendezvous, and neither says something different about it \
         than the other does — an owned descriptor is refused in the same words an announced number is"
    );
}
