//! The number a parent wants back from a child that dies in the middle of a transaction, and the one
//! line that hands it over.
//!
//! A crash cut is the verdict a claim cannot report through a return value: the process stops where the
//! transaction stands and the only thing the parent reads is the exit number. Both legs used to hold that
//! number — one to die with, one to assert — so the pair stayed honest only while somebody remembered to
//! edit both. The parent passes it now and the child leaves with whatever it was handed.
//!
//! What deliberately stays per claim is the table the parent reads. `recovery_conserves`'s `at_every_cut` leg
//! names four cuts at 81 to 84, its `unix` and `windows` legs distinguish a doomed member (77) from a foreign
//! owner (73) and a pool filler (74), and `src/tests/unix/recovery.rs` — a library unit test, unreachable from
//! this subtree — keeps deriving `80 + cut`. Merging those would assert more than any claim does: that two
//! files' crash numbers may never collide.

use std::{env, process::Command};

/// The key a parent writes its verdict number under.
pub const CODE: &str = "EVERING_CRASH_CODE";

/// Tells `command` which number it is to die with.
pub fn hand(command: &mut Command, code: i32) {
    command.env(CODE, code.to_string());
}

/// The number this process was handed, read by the role about to leave without unwinding.
pub fn code() -> i32 {
    env::var(CODE)
        .expect("a crashing child is told which number its death is reported by")
        .parse()
        .expect("a numeric crash code")
}
