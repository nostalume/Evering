//! The child's side of the re-entry: the roles one claim defines, the one key that names which to run, and
//! the two words a role has about leaving.
//!
//! A multi-process case has no second executable, so its child is the same test binary started again with a
//! filter naming one item and an environment variable naming one role. Two things follow, and both are the
//! point of this module.
//!
//! - One key, [`ROLE`], named nowhere else in `tests/`. Each claim used to freeze its own
//!   (`EVERING_RECEIPT_ROLE`, `EVERING_IMPOSTOR_ROLE`, `EVERING_SEAT_ROLE`, `EVERING_PROCESS_ROLE`,
//!   `EVERING_RECOVERY_ROLE`), and one file's `CHILD` was the name of the key while another's was the value
//!   of a role. Role *values* stay each claim's own words: they say which arm of which case ran, and only
//!   that claim knows what they should read like.
//! - A role body is a function rather than a test. The filter can only select items the harness reports, so
//!   a body used to need a `#[test]` on it, which is how a target came to hold a `supervised_child` that was
//!   a child role and never an assertion. Each spawning target now declares exactly one `#[test]
//!   fn role_entry()`, whose whole body is a dispatch table, and every other item in the file is a driver:
//!   it never asks whether this process is the one it was waiting for, and a child cannot reach a driver by
//!   accident because no role name leads into one.
//!
//! What is deliberately not here: the verdict codes a role leaves with (`REPORTED`, `DIVERGED`, `ARRIVED`,
//! `PARKED`, and the region and size each claim runs at). After `dispatch` the role name the parent chose
//! already says which arm ran, so a table that also fixed the codes would let a driver assert a fact about
//! words it wrote itself. `src/tests/unix/recovery.rs` keeps its own role key for the same reason as
//! `cuts`: it is the library's test module, where this subtree cannot reach it.

use std::{env, process::Command};

/// The only environment key a re-entered test child reads.
pub const ROLE: &str = "EVERING_ROLE";

/// The code a child leaves with when it gave up for a reason it printed beside the code.
pub const FAILED: i32 = 1;

/// One role's whole program: what it does, and the code it leaves with.
pub type Body = fn() -> i32;

/// Runs the role the parent named and leaves with its code.
///
/// Returns when the key is unset, which is the parent's own run of the entry item: the entry then reports
/// one passing test and nothing else, so `role_entry` is honest as an item and inert as a driver. A role
/// named that the table does not hold is the claim's own mistake and is reported as one, rather than
/// falling through into whichever driver shares the binary.
pub fn dispatch(table: &[(&'static str, Body)]) {
    dispatch_all(&[table]);
}

/// [`dispatch`] over several tables at once, which is what a claim whose legs sit in separate files needs:
/// each leg keeps its roles beside its own bodies, and the root's entry says which legs this host compiles.
/// A role named by two of them is settled by order rather than ambiguity — the first table to match has the
/// child leave before the second is read — so a duplicate is a claim's own drafting mistake, visible in its
/// `--list`, and not something this function can detect.
pub fn dispatch_all(tables: &[&[(&'static str, Body)]]) {
    let Ok(role) = env::var(ROLE) else {
        return;
    };
    for table in tables {
        for (name, body) in *table {
            if *name == role {
                leave(body());
            }
        }
    }
    panic!("{role:?} is not a role this target defines");
}

/// The test binary, re-entering as one role: one item selects the table, one key selects the row.
///
/// The child's own harness output is sent to the void, because a case that reports a role's progress would
/// otherwise report it twice — once from the child's run and once from the assertion reading its exit code.
pub fn child(role: &str) -> Command {
    let mut command = Command::new(env::current_exe().expect("the test binary is its own parent"));
    command
        .args(["--exact", "role_entry"])
        .env(ROLE, role)
        .stdout(std::process::Stdio::null());
    command
}

/// Says, in the child's own words, why it is giving up, and returns the code for any other reason.
pub fn fail(reason: &str) -> i32 {
    eprintln!("{reason}");
    FAILED
}

/// Leaves without running destructors, so a member this process stood in stays stood in: a slot its member
/// left politely is what `Drop` is for, and only a slot whose process is gone is a reap's.
pub fn leave(code: i32) -> ! {
    std::process::exit(code)
}
