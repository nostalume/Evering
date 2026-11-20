#![cfg(all(feature = "process", windows))]

//! The Windows half of P0. The parent already refused a client that was not the child it retained
//! (`Listener::accept`), so what was missing was the other direction: a process that reaches a pipe
//! it does not own is now told so by the kernel before it is allowed to read the offer.

use std::{
    env,
    process::Command,
    thread,
    time::{Duration, Instant},
};

use evering::{
    mapping::Access,
    os::{Handoff, Shared, event, windows::process::Listener},
    process::{Bootstrap, Supervisor},
};

const RENDEZVOUS: &str = "EVERING_RENDEZVOUS";
const INTRODUCER: &str = "EVERING_INTRODUCER";
const ROLE: &str = "EVERING_IMPOSTOR_ROLE";
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
/// that refusal by its reason: any other error would mean it never reached a pipe at all.
fn attacker() -> i32 {
    match Handoff::receive() {
        Ok((peer, _, _report)) => {
            eprintln!(
                "attacker was handed the meeting by a peer at {}",
                peer.pid()
            );
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
        Ok((peer, resources, _report)) => {
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

fn spawn(command: &mut Command, role: &str, test: &str) -> Supervisor {
    command.args(["--exact", test]).env(ROLE, role);
    Supervisor::spawn(command).expect("a role process spawns")
}

#[test]
fn a_granted_process_names_the_peer_it_met() {
    const TEST: &str = "a_granted_process_names_the_peer_it_met";
    if env::var(ROLE).ok().as_deref() == Some(CHILD) {
        std::process::exit(child());
    }

    let (handoff, grant) = Handoff::bind().unwrap();
    let mapping: Shared =
        evering::os::shared("attestation", SIZE, Access::READ | Access::WRITE).unwrap();
    let (_notify, child_wait) = event().unwrap();
    let (child_ring, _event) = event().unwrap();
    let bootstrap = Bootstrap::new(b"selected-protocol-id").unwrap();

    let mut command = Command::new(env::current_exe().unwrap());
    command.env(EXPECT_PEER, std::process::id().to_string());
    grant.admit(&mut command).unwrap();
    let mut granted = spawn(&mut command, CHILD, TEST);

    handoff
        .send(
            &granted,
            Instant::now() + Duration::from_secs(10),
            &bootstrap,
            &mapping,
            &child_wait,
            &child_ring,
        )
        .unwrap();

    assert_eq!(
        wait_for(&mut granted, "the granted child", Duration::from_secs(20)),
        ATTESTED,
        "the process the grant was admitted to meets and names its introducer"
    );
}

#[test]
fn a_process_that_claims_another_introducer_is_refused_before_it_reads() {
    const TEST: &str = "a_process_that_claims_another_introducer_is_refused_before_it_reads";
    if env::var(ROLE).ok().as_deref() == Some(ATTACKER) {
        std::process::exit(attacker());
    }

    let listener = Listener::bind().unwrap();
    let mut command = Command::new(env::current_exe().unwrap());
    command
        .env(RENDEZVOUS, listener.name())
        .env(INTRODUCER, "1");
    let mut attacker = spawn(&mut command, ATTACKER, TEST);

    assert_eq!(
        wait_for(&mut attacker, "the claimant", Duration::from_secs(20)),
        REFUSED,
        "a pipe owned by this process is not the introducer the claimant named"
    );
}
