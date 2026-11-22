//! The two shapes in which a shared word stops being reached by name, neither of which is a type
//! error: a field declared with a visibility its neighbours do not have, which is how a new shared
//! field could be read from another module without an accessor ever being written; and a signature
//! that hands out an atom, which is the exception the rule allows and the one thing that has to be
//! counted. `docs/AGENT.md` states the rule; `docs/architecture.md` states the width discipline a
//! new field owes it.
//!
//! `src/` is read as text for the same reason the manifest and the integration tree are in
//! `tests/verification_layout.rs`: the shape is invisible to the compiler, so a violation looks
//! exactly like a passing build. Matching is by spelling and deliberately narrow — a field line is
//! `name: Type,`, a signature is the text after `->` — so one added line fires the law without a
//! comment, an attribute or a `let` firing it.
//!
//! What is deliberately not claimed here. That every shared field has an accessor: a field read
//! only inside its own module needs none, and no spelling says whether another module reaches for
//! it. And the words a pool or the heap computes from a base address, handed out as locations by
//! signatures naming no atomic type, which the register below cannot see — `Class::control` is in it
//! because its return type says `AtomicU32`, while `Queue::header` and `talc.rs`'s pointer
//! arithmetic are not, and those are the seams the retirement work names. The floor in the first law
//! is the vacuity guard: a count pinned to today would fail the day a field is added for a good
//! reason, and a pattern that stopped matching would pass forever.

use std::{
    fs,
    path::{Path, PathBuf},
};

/// The type positions interior mutability is spelled with, including the array and option
/// wrappings a shared word is actually stored in.
fn interior(ty: &str) -> bool {
    ["Atomic", "UnsafeCell", "Cell<"]
        .iter()
        .any(|mark| ty.contains(mark))
}

/// The declared type of a field line, and everything spelled before the colon, which is where a
/// visibility would sit. A line that is not `name: Type,` is not a field: statements end in `;`,
/// attributes and comments are skipped by the caller, and a signature's return type follows `->`.
fn field(line: &str) -> Option<(&str, &str)> {
    let body = line.split("//").next().unwrap_or("").trim();
    let (spelled, ty) = body.strip_suffix(',')?.split_once(": ")?;
    let name = spelled.rsplit(' ').next().unwrap_or("");
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return None;
    }
    interior(ty).then_some((spelled, ty))
}

/// The name of the function a signature line declares, or `None` where the line hands out no atom.
fn hand_out(line: &str) -> Option<String> {
    let tail = line.split_once("fn ")?.1;
    let name = tail
        .split(['(', ' ', '\t', '<'])
        .next()
        .filter(|name| !name.is_empty())?;
    if !interior(tail.split_once("->")?.1) {
        return None;
    }
    Some(name.to_string())
}

/// Every `.rs` file under `src/`, named relative to the manifest and with forward slashes, so one
/// failure message and this file's register read the same on either host this suite runs on.
fn sources() -> Vec<(String, PathBuf)> {
    fn walk(manifest: &Path, directory: &Path, files: &mut Vec<(String, PathBuf)>) {
        for entry in fs::read_dir(directory).expect("the crate source tree") {
            let path = entry.expect("a readable entry").path();
            if path.is_dir() {
                walk(manifest, &path, files);
            } else if path.extension().is_some_and(|kind| kind == "rs") {
                let relative = path
                    .strip_prefix(manifest)
                    .expect("a file inside the source tree")
                    .to_string_lossy()
                    .replace('\\', "/");
                files.push((relative, path));
            }
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    walk(&manifest, &manifest.join("src"), &mut files);
    files.sort();
    files
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
fn no_shared_field_is_visible_outside_the_module_declaring_it() {
    let mut declared = 0;
    let mut published = Vec::new();
    for (relative, path) in sources() {
        for (number, line) in read(&path).lines().enumerate() {
            let line = line.trim();
            if line.starts_with("//") || line.starts_with('#') {
                continue;
            }
            if let Some((spelled, _)) = field(line) {
                declared += 1;
                if spelled.contains("pub") {
                    published.push(format!("{relative}:{}: {line}", number + 1));
                }
            }
        }
    }
    assert!(
        published.is_empty(),
        "a shared field is reachable as a field, which is what an accessor replaces: {published:?}"
    );
    assert!(
        declared >= 20,
        "the scan matched only {declared} interior-mutable fields, so it no longer sees `src/` as \
         the declarations this law is about"
    );
}

/// The atom hand-outs as they stand, one entry per declaration. Adding a hand-out is allowed and is
/// not free: it means editing this list, which is where the reason belongs. The pair is spelled
/// twice for `src/queue.rs` because a trait method and the mock that implements it are two
/// signatures reaching the same word.
const REGISTERED: [(&str, &str); 6] = [
    ("src/channel.rs", "lifecycle"),
    ("src/dir.rs", "next"),
    ("src/header.rs", "member"),
    ("src/pool.rs", "control"),
    ("src/queue.rs", "lifecycle"),
    ("src/queue.rs", "lifecycle"),
];

fn spelled(file: &str, name: &str) -> String {
    format!("{file} {name}")
}

#[test]
fn the_atom_hand_outs_are_exactly_the_ones_registered() {
    let mut found = Vec::new();
    for (relative, path) in sources() {
        for line in read(&path).lines() {
            let line = line.trim();
            if line.starts_with("//") || line.starts_with('#') {
                continue;
            }
            if let Some(name) = hand_out(line) {
                found.push(spelled(&relative, &name));
            }
        }
    }
    let mut registered: Vec<String> = REGISTERED
        .iter()
        .map(|(file, name)| spelled(file, name))
        .collect();
    found.sort();
    registered.sort();
    assert_eq!(
        found, registered,
        "the shared words handed out as atoms are not the ones registered; a new hand-out needs \
         its reason, and a moved or renamed one needs this list to say where it went"
    );
}
