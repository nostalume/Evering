//! What a supervised child owes the process that spawned it: a wait that returns a terminal proof, a kill
//! that finishes with the same status it was waited for, a failed spawn that creates no supervisor at all,
//! and an `Exited` witness that reports a death before anybody reaps it — including once the owner that
//! made it has given the child up, which is the only arm of `Drop` this claim reaches.

use std::{env, process::Command, thread, time::Duration};

use evering::process::Supervisor;

mod support;
use crate::support::role;

/// The role a terminal proof is owed: it agrees by leaving, and leaves with success.
fn agreed() -> i32 {
    0
}

/// The role a kill is owed: it stays alive until the parent takes it off its hands.
fn stays() -> i32 {
    loop {
        thread::sleep(Duration::from_secs(1));
    }
}

/// The child's whole program: the roles this claim defines, and nothing else.
#[test]
fn role_entry() {
    role::dispatch(&[("agreed", agreed), ("stays", stays)]);
}

#[test]
fn wait_returns_terminal_proof_for_retained_child() {
    let mut supervisor = Supervisor::spawn(&mut role::child("agreed")).unwrap();
    let exit = supervisor.wait().unwrap();
    assert!(exit.success());
    assert!(exit.status().success());
}

#[test]
fn kill_always_finishes_with_waited_status() {
    let mut supervisor = Supervisor::spawn(&mut role::child("stays")).unwrap();
    assert!(supervisor.try_wait().unwrap().is_none());
    assert!(!supervisor.kill_wait().unwrap().success());
    assert!(supervisor.try_wait().unwrap().is_some());
}

#[test]
fn failed_spawn_creates_no_supervisor() {
    let mut command = Command::new(env::temp_dir().join("evering-executable-that-does-not-exist"));
    assert!(Supervisor::spawn(&mut command).is_err());
}

/// A witness reports a death and nothing about it. The cases below are the parts of that claim each of
/// which can fail alone: it does not fire while the child is live, it fires for a child that is gone but
/// still unreaped, it stays ready once the death is recorded, it holds no descriptor for a child this
/// process already waited on, and it is not stranded by the owner that made it.
#[cfg(all(feature = "tokio", target_os = "linux"))]
mod death_witness {
    use super::role;

    use std::time::Duration;

    use evering::{
        notify::Wait,
        process::{Exited, Supervisor},
    };
    use tokio::time::timeout;

    const PARKED: Duration = Duration::from_millis(50);
    const WAKE: Duration = Duration::from_secs(5);

    async fn wakes(witness: &Exited) {
        match timeout(WAKE, witness.wait()).await {
            Ok(ready) => {
                ready.expect("a child's death is reported by the host, not guessed at");
            }
            Err(_) => panic!("the witness stayed parked while its child was gone"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_child_that_exited_but_was_never_reaped_wakes_its_witness() {
        let mut supervisor = Supervisor::spawn(&mut role::child("agreed")).unwrap();
        let witness = supervisor.exited().unwrap();

        // The whole point of the witness: the death is observable before any reap turns it into a
        // status, and the task that learns it is not the one that has to take the child off its hands.
        wakes(&witness).await;

        let exit = supervisor
            .try_wait()
            .unwrap()
            .expect("the child the witness reported is still owed its reap");
        assert!(exit.success());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_live_child_keeps_its_witness_parked() {
        let mut supervisor = Supervisor::spawn(&mut role::child("stays")).unwrap();
        let witness = supervisor.exited().unwrap();

        assert!(
            timeout(PARKED, witness.wait()).await.is_err(),
            "a witness that reports readiness for a running child strands its caller on a reap of a \
             process that is still alive"
        );

        supervisor.kill_wait().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_witness_stays_ready_after_the_child_was_killed_and_reaped() {
        let mut supervisor = Supervisor::spawn(&mut role::child("stays")).unwrap();
        let witness = supervisor.exited().unwrap();

        supervisor.kill_wait().unwrap();

        // Readiness here is a fact about the child rather than a latch to drain, so a second waiter
        // learns the same death the first one did.
        wakes(&witness).await;
        wakes(&witness).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_witness_taken_after_a_wait_never_asks_the_host() {
        let mut supervisor = Supervisor::spawn(&mut role::child("agreed")).unwrap();
        supervisor.wait().unwrap();

        // The child is gone and already reaped, so a process number taken now could name somebody
        // else: what `exited` hands back is this process's own record, ready with no descriptor behind
        // it. The assertion that belongs to that is that nothing is asked of the host at all, which is
        // why the wait cannot be the one that parks.
        let witness = supervisor.exited().unwrap();
        wakes(&witness).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_witness_is_not_stranded_by_the_owner_that_gave_its_child_up() {
        let supervisor = Supervisor::spawn(&mut role::child("stays")).unwrap();
        let witness = supervisor.exited().unwrap();

        // `Drop` kills and reaps a child nobody waited on, and a witness holds no borrow of that owner,
        // so the death a caller was promised still arrives after the promise-maker is gone. This is
        // also the only place the suite reaches that arm of `Drop` at all.
        drop(supervisor);
        wakes(&witness).await;
    }
}
