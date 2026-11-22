//! The verification surface's own bookkeeping, checked from inside the suite rather than by a shell step:
//! the claim files and their legs are what the manifest says they are, and a claim the manifest does not name
//! is built by nothing and reported by nobody. That is the failure shape the explicit `[[test]]` rows bring
//! with them — autodiscovery's failure was an extra target nobody declared, which a `cargo test` run would at
//! least show — and a dropped row looks exactly like a passing suite.
//!
//! Four laws, each the thing that can rot without a compile error: every integration file is a declared
//! target; every leg file is named by the root that owns it; a target has one role entry and dispatches every
//! role table its legs declare; and the re-entry key is spelled in the harness alone, which is what keeps the
//! five spellings of one handoff — the shape this suite held before the harness existed — from coming back.
//!
//! What this file deliberately does not check: that a declared path exists. Cargo refuses a `[[test]]` row
//! pointing at a missing file before any test runs, and a check that cannot fire is not a check. The narrow
//! build row in CI — `cargo check --tests --features os,tokio` — is the other half of the same guard, and
//! belongs there because it is a build, not an assertion.

use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    fs::read_to_string(root().join(relative)).unwrap_or_else(|error| panic!("{relative}: {error}"))
}

/// The `path` of every `[[test]]` table in the manifest, in the order they are written.
///
/// A table that names no file is a panic rather than a skip: a row this function failed to read would make
/// its file look undeclared, and the direction worth protecting is the one that stays silent.
fn declared_paths() -> Vec<String> {
    let mut paths = Vec::new();
    let mut row: Option<String> = None;
    let mut inside = false;
    for line in read("Cargo.toml").lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            if let Some(path) = row.take() {
                paths.push(path);
            }
            inside = line == "[[test]]";
        } else if inside && line.starts_with("path") {
            let value = line
                .split_once('=')
                .expect("a row's `path` is a key and a value")
                .1
                .trim()
                .trim_matches('"')
                .to_string();
            assert!(
                row.replace(value.clone()).is_none(),
                "a `[[test]]` row names two files, at {value:?}"
            );
        }
    }
    if let Some(path) = row.take() {
        paths.push(path);
    }
    paths
}

/// The `tests/*.rs` files a `[[test]]` row is expected to name: everything at the top of the tree except the
/// harness, which is imported by the claim files rather than built as a target of its own.
fn claim_files() -> BTreeSet<String> {
    files_in("tests")
        .into_iter()
        .filter(|file| file != "tests/support.rs")
        .collect()
}

/// Every directory under `tests/` holding a claim's legs, named by its root.
fn leg_directories() -> Vec<String> {
    let mut legs = Vec::new();
    for entry in fs::read_dir(root().join("tests")).expect("the integration tree") {
        let path = entry.expect("a readable entry").path();
        if path.is_dir() {
            let name = path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if name != "support" {
                legs.push(name);
            }
        }
    }
    legs.sort();
    legs
}

/// The `.rs` files directly inside one directory of the integration tree, as tree-relative paths.
fn files_in(directory: &str) -> Vec<String> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root().join(directory)).expect("a readable directory") {
        let path = entry.expect("a readable entry").path();
        if path.is_file() {
            let name = path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if name.ends_with(".rs") {
                files.push(format!("{directory}/{name}"));
            }
        }
    }
    files.sort();
    files
}

/// Every `.rs` file in the integration tree: the roots, the harness and its children, and the legs.
fn all_files() -> Vec<String> {
    let mut files = files_in("tests");
    for claim in leg_directories() {
        files.extend(files_in(&format!("tests/{claim}")));
    }
    files.extend(files_in("tests/support"));
    files
}

/// The `#[path = "…"]` values in a file, resolved against `tests/` so that a root reaching above the tree —
/// the way `example_wire.rs` compiles an example — lands in the same coordinates as its leg files.
fn named_paths(text: &str) -> BTreeSet<String> {
    let mut named = BTreeSet::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("#[path")
            && let Some((_, value)) = rest.split_once('=')
        {
            let value = value.trim().trim_end_matches(']').trim_matches('"');
            let joined = Path::new("tests").join(value);
            let mut resolved = PathBuf::new();
            for part in joined.components() {
                match part {
                    Component::ParentDir => {
                        resolved.pop();
                    }
                    Component::Normal(name) => resolved.push(name),
                    _ => {}
                }
            }
            named.insert(resolved.to_string_lossy().replace('\\', "/"));
        }
    }
    named
}

#[test]
fn every_integration_file_is_a_declared_target() {
    let declared: BTreeSet<String> = declared_paths()
        .into_iter()
        .filter(|path| path.starts_with("tests/") && path.ends_with(".rs"))
        .collect();
    let claims = claim_files();
    let undeclared: Vec<&String> = claims.difference(&declared).collect();
    assert!(
        undeclared.is_empty(),
        "files built by no `[[test]]` row: {undeclared:?}",
    );
    let stray: Vec<&String> = declared.difference(&claims).collect();
    assert!(
        stray.is_empty(),
        "`[[test]]` rows naming a file that is not a claim: {stray:?}",
    );
}

#[test]
fn every_leg_is_named_by_the_root_it_belongs_to() {
    for claim in leg_directories() {
        let named = named_paths(&read(&format!("tests/{claim}.rs")));
        for leg in files_in(&format!("tests/{claim}")) {
            assert!(
                named.contains(&leg),
                "`{leg}` is named by no `#[path]` in `tests/{claim}.rs`, so nothing compiles it",
            );
        }
    }
}

#[test]
fn one_role_entry_answers_for_a_target_and_its_tables() {
    for claim in leg_directories() {
        let legs = files_in(&format!("tests/{claim}"));
        let tables: Vec<String> = legs
            .iter()
            .filter(|leg| read(leg).contains("const ROLES"))
            .map(|leg| {
                Path::new(leg)
                    .file_stem()
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        for leg in &legs {
            assert!(
                !read(leg).contains("fn role_entry"),
                "`{leg}` holds a `role_entry`, so only one of the target's entries is ever reached",
            );
        }
        if tables.is_empty() {
            continue;
        }
        let root_text = read(&format!("tests/{claim}.rs"));
        assert_eq!(
            root_text.matches("fn role_entry").count(),
            1,
            "`tests/{claim}.rs` owns role tables but not the target's one role entry",
        );
        for table in tables {
            assert!(
                root_text.contains(&format!("{table}::ROLES")),
                "`tests/{claim}.rs` never dispatches the `{table}` leg's role table, so its roles answer to \
                 nothing",
            );
        }
    }
}

#[test]
fn the_reentry_key_is_spelled_in_the_harness_alone() {
    let spelled_line = read("tests/support/role.rs")
        .lines()
        .find(|line| line.trim().starts_with("pub const ROLE:"))
        .expect("the harness names the re-entry key it reads")
        .split_once('=')
        .expect("the re-entry key is a constant with a value")
        .1
        .trim()
        .trim_end_matches(';')
        .trim_matches('"')
        .to_string();
    let mut spelled = Vec::new();
    for file in all_files() {
        if file == "tests/support/role.rs" {
            continue;
        }
        if read(&file).contains(&spelled_line) {
            spelled.push(file);
        }
    }
    assert!(
        spelled.is_empty(),
        "the re-entry key is spelled outside `tests/support/role.rs`: {spelled:?}",
    );
}
