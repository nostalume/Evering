//! Which process a handoff will meet, off the mechanism this host has: a pipe answers for its own client,
//! so a process that reaches a rendezvous it does not hold is told so before it may read the offer, and a
//! process that was never admitted has no rendezvous to reach at all.
//!
//! The parent already refused a client that was not the child it retained
//! (`Listener::accept`), so what was missing was the other direction: a process that reaches a pipe
//! it does not own is now told so by the kernel before it is allowed to read the offer.
//!
//! The two runs also carry the introducer's policy word, one forged by the claimant and one written by
//! the grant that admitted the other child: `Provenance` makes an unattested peer a value, and these are
//! the assertions that it did not become a value a claimant could ask for.
//!
//! The refusal of a process that was never granted anything is here rather than in the file that borrowed a
//! handoff to move a region, because a claim about who may meet belongs with the machinery being attacked. It
//! has no near neighbour on this host: the claimant above is refused by the peer check one step into `receive`,
//! and this one never reaches a pipe, because a grant announces its address to the command it was spent into
//! and to nobody else.

use std::{
    env,
    process::Command,
    time::{Duration, Instant},
};

use evering::{
    mapping::Access,
    os::{Handoff, Provenance, Shared, event, windows::process::Listener},
    process::{Bootstrap, Supervisor},
};

use crate::support::{introduce, role};

const RENDEZVOUS: &str = "EVERING_RENDEZVOUS";
const INTRODUCER: &str = "EVERING_INTRODUCER";
/// The introducer's policy key: the claimant writes it for itself, and the honest grant below writes it
/// through `Grant::trusting_introduction`.
const TRUST: &str = "EVERING_TRUST";
const ATTACKER: &str = "attacker";
const CHILD: &str = "child";
const EXPECT_PEER: &str = "EVERING_EXPECT_PEER";
const SIZE: usize = 4096;

const HIJACKED: i32 = 7;
const ATTESTED: i32 = 4;
const REFUSED: i32 = 0;
const FAILED: i32 = 1;

/// Connects to a real listener the test process owns, and claims to have been introduced by someone
/// else. The kernel's answer for the pipe server is what refuses the claim, so the attacker asks for
/// that refusal by its reason: any other error would mean it never reached a pipe at all. It writes the
/// introducer's policy key for itself on the way, which is the word that must not answer for a pipe the
/// introducer never opened.
fn attacker() -> i32 {
    match Handoff::receive() {
        Ok((provenance, _, _report)) => {
            eprintln!("attacker was handed the meeting by {provenance:?}");
            HIJACKED
        }
        Err(error)
            if error
                .to_string()
                .contains("not the process that granted it") =>
        {
            eprintln!("attacker was refused by the peer check: {error}");
            REFUSED
        }
        Err(error) => {
            eprintln!("attacker failed before the peer check: {error}");
            FAILED
        }
    }
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
fn a_granted_process_names_the_peer_it_met() {
    let (handoff, grant) = Handoff::bind().unwrap();
    let mapping: Shared =
        evering::os::shared("attestation", SIZE, Access::READ | Access::WRITE).unwrap();
    let (_notify, child_wait) = event().unwrap();
    let (child_ring, _event) = event().unwrap();
    let bootstrap = Bootstrap::new(b"selected-protocol-id").unwrap();

    let mut command = role::child(CHILD);
    command.env(EXPECT_PEER, std::process::id().to_string());
    grant.trusting_introduction().admit(&mut command).unwrap();
    let mut granted = Supervisor::spawn(&mut command).expect("a role process spawns");

    handoff
        .send(
            &granted,
            Instant::now() + Duration::from_secs(10),
            &bootstrap,
            &mapping,
            child_wait,
            &child_ring,
        )
        .unwrap();

    assert_eq!(
        introduce::exit_code(&mut granted, "the granted child", Duration::from_secs(20)),
        ATTESTED,
        "the process the grant was admitted to meets and names its introducer, and the policy that \
         grant carried leaves an answer the kernel gave exactly as it was"
    );
}

#[test]
fn a_process_that_claims_another_introducer_is_refused_before_it_reads() {
    let listener = Listener::bind().unwrap();
    let mut command = role::child(ATTACKER);
    command
        .env(RENDEZVOUS, listener.name())
        .env(INTRODUCER, "1")
        .env(TRUST, "1");
    let mut attacker = Supervisor::spawn(&mut command).expect("a role process spawns");

    assert_eq!(
        introduce::exit_code(&mut attacker, "the claimant", Duration::from_secs(20)),
        REFUSED,
        "a pipe owned by this process is not the introducer the claimant named, and a policy the \
         claimant wrote for itself cannot vouch for a pipe no introducer opened"
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
