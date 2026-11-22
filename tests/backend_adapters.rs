//! A test name the two backend trees of the in-crate suite share is a claim that needs no kernel object —
//! that is the rule `src/tests.rs` states — and the rule's failure shape is a copy: a body in
//! `src/tests/mock` that spells again what `src/tests.rs` already holds keeps passing, logs what it likes
//! instead of what the body logs, and drifts from the reading under `src/tests/unix` with nothing noticing.
//! Neither the compiler nor a `cargo test` run can see that, so this file reads the trees as text, in the
//! same genre as `tests/shared_word_access.rs`.
//!
//! Matching is by spelling and deliberately narrow: a line written `#[test]`, the `fn` line after it, and the
//! body up to the first line closed at column zero, which is where `cargo fmt` leaves a top-level function.
//! A delegation is therefore a call spelled `tests::<name>(`, which is how both trees import the module the
//! bodies live in. The floors in the first law are the vacuity guard: a scan that stopped matching these two
//! trees would pass forever, and a count pinned to today would fail the day a case is added for a good
//! reason.
//!
//! What is deliberately not claimed here. That a body is not a copy of another body under a different name:
//! no spelling says two claims are the same claim, which is why the register below lists names and not
//! bodies. And that a shared body needs no kernel object — the reason a name joins the register belongs at
//! the body's declaration, where a reader can weigh it, and this file cannot read it.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// A file's path relative to the manifest, with forward slashes, so one failure message and the register
/// below read the same on either host this suite runs on.
fn spelled(path: &Path) -> String {
    path.strip_prefix(root())
        .expect("a file inside the crate")
        .to_string_lossy()
        .replace('\\', "/")
}

/// The `.rs` files of one backend: the file that names it and the directory of the same name, so a case
/// added under `src/tests/mock/` is in the same scan as one added to `src/tests/mock.rs`.
fn backend(tree: &str) -> Vec<(String, PathBuf)> {
    fn walk(directory: &Path, files: &mut Vec<(String, PathBuf)>) {
        for entry in fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("{}: {error}", directory.display()))
        {
            let path = entry.expect("a readable entry").path();
            if path.is_dir() {
                walk(&path, files);
            } else if path.extension().is_some_and(|kind| kind == "rs") {
                files.push((spelled(&path), path));
            }
        }
    }
    let tests = root().join("src").join("tests");
    let mut files = Vec::new();
    let single = tests.join(format!("{tree}.rs"));
    if single.is_file() {
        files.push((spelled(&single), single));
    }
    walk(&tests.join(tree), &mut files);
    files.sort();
    files
}

/// The `#[test]` functions in those files: where they are spelled, their names, and the text from the
/// signature line to the line that closes them, which is the body the law reads.
fn declared(files: &[(String, PathBuf)]) -> Vec<(String, String, String)> {
    let mut tests = Vec::new();
    for (relative, path) in files {
        let text = read(path);
        let lines: Vec<&str> = text.lines().collect();
        let mut number = 0;
        while number < lines.len() {
            if lines[number].trim() != "#[test]" {
                number += 1;
                continue;
            }
            let mut signature = number + 1;
            while signature < lines.len()
                && !lines[signature].trim_start().starts_with("fn ")
                && (lines[signature].trim().is_empty()
                    || lines[signature].trim_start().starts_with(['#', '/', ' ']))
            {
                signature += 1;
            }
            assert!(
                signature < lines.len() && lines[signature].trim_start().starts_with("fn "),
                "`{relative}` has a `#[test]` line with no signature after it, so this scan no longer reads \
                 the backend trees as functions"
            );
            let name = lines[signature]
                .trim_start()
                .strip_prefix("fn ")
                .unwrap()
                .split(['(', ' ', '\t', '<'])
                .next()
                .unwrap_or("")
                .to_string();
            let closing = (signature + 1..lines.len())
                .find(|next| lines[*next] == "}")
                .unwrap_or(lines.len() - 1);
            tests.push((
                relative.clone(),
                name,
                lines[signature..=closing].join("\n"),
            ));
            number = closing + 1;
        }
    }
    tests
}

fn names(tests: &[(String, String, String)]) -> BTreeSet<String> {
    tests.iter().map(|(_, name, _)| name.clone()).collect()
}

/// The claims both trees spell, in name order.
fn shared(mock: &[(String, String, String)], unix: &[(String, String, String)]) -> Vec<String> {
    names(mock).intersection(&names(unix)).cloned().collect()
}

#[test]
fn a_claim_both_backends_make_is_delegated_in_both() {
    let mock = declared(&backend("mock"));
    let unix = declared(&backend("unix"));
    assert!(
        mock.len() >= 60 && unix.len() >= 10,
        "the scan matched {} tests in `src/tests/mock` and {} in `src/tests/unix`, so it no longer sees the \
         two backend trees as the suite this law is about",
        mock.len(),
        unix.len()
    );
    for name in shared(&mock, &unix) {
        for (file, test, body) in mock
            .iter()
            .chain(&unix)
            .filter(|(_, test, _)| **test == name)
        {
            assert!(
                body.contains(&format!("tests::{name}(")),
                "`{file}` spells `{test}`, a name both backend trees share, as a body of its own instead of a \
                 call into `src/tests.rs`: a claim that asks nothing of the host reserves is written once and \
                 handed a view by each tree, so that the two readings cannot drift while both pass",
            );
        }
    }
}

/// The claims the two trees share, as they stand, one entry per name. Sharing a claim is allowed and is not
/// free: it means editing this list, which is where the reason for the second reading belongs. The pair is
/// written once rather than per file because a shared claim is one claim, whichever tree added it first.
const REGISTERED: [&str; 1] = ["area_init"];

#[test]
fn the_claims_both_backends_make_are_exactly_the_ones_registered() {
    let found = shared(&declared(&backend("mock")), &declared(&backend("unix")));
    let registered: Vec<String> = REGISTERED.iter().map(|name| (*name).to_string()).collect();
    assert_eq!(
        found, registered,
        "the claims the two backend trees share are not the ones registered; a new shared name needs its \
         reason, and one that moved or changed spelling needs this list to say where it went"
    );
}
