#![cfg(feature = "process")]

use std::{env, process::Command, thread, time::Duration};

use evering::process::Supervisor;

const CHILD: &str = "EVERING_SUPERVISOR_CHILD";

#[test]
fn supervised_child() {
    match env::var(CHILD).as_deref() {
        Ok("exit") => {}
        Ok("sleep") => loop {
            thread::sleep(Duration::from_secs(1));
        },
        _ => {}
    }
}

fn child(mode: &str) -> Command {
    let mut command = Command::new(env::current_exe().unwrap());
    command
        .args(["--exact", "supervised_child"])
        .env(CHILD, mode);
    command
}

#[test]
fn wait_returns_terminal_proof_for_retained_child() {
    let mut supervisor = Supervisor::spawn(&mut child("exit")).unwrap();
    let exit = supervisor.wait().unwrap();
    assert!(exit.success());
    assert!(exit.status().success());
}

#[test]
fn kill_always_finishes_with_waited_status() {
    let mut supervisor = Supervisor::spawn(&mut child("sleep")).unwrap();
    assert!(supervisor.try_wait().unwrap().is_none());
    assert!(!supervisor.kill_wait().unwrap().success());
    assert!(supervisor.try_wait().unwrap().is_some());
}

#[test]
fn failed_spawn_creates_no_supervisor() {
    let mut command = Command::new(env::temp_dir().join("evering-executable-that-does-not-exist"));
    assert!(Supervisor::spawn(&mut command).is_err());
}
