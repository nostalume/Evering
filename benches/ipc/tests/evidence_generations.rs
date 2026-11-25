//! Evidence generations: the schedule definitions a recorded artifact was written
//! against, and the retained revision-2 `core-ipc` smoke run that has to stay
//! checkable. The retired table has no producer any more, so the artifact pins it: an
//! edit that drifts the generation away from the schedule it names fails here rather
//! than silently re-explaining a measurement nobody can rerun.

use crate::{analysis, family, model, study, system};

/// The retained `core-ipc` revision-2 artifact: one block, five cases, and no boundary
/// or worker figure anywhere in it, because it was written before stage 0.2 counted
/// anything.
const RETIRED_SMOKE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/benches/evidence/core-ipc-windows-smoke-20260802.jsonl"
);

fn cases(family: &family::Family, mode: &str) -> Vec<system::Case> {
    let members = (family.members)(mode).unwrap();
    let blocks = family.mode(mode).unwrap().1;
    model::schedule(&members, blocks, 7)
        .into_iter()
        .map(|entry| system::Case {
            workload: entry.cell(),
            resources: entry.arm.resources(),
        })
        .collect()
}

#[test]
fn each_generation_resolves_to_the_definition_that_records_it() {
    assert!(std::ptr::eq(
        family::find_at("core-ipc", family::CORE.revision).unwrap(),
        &family::CORE
    ));
    let retired = family::find_at("core-ipc", 2).unwrap();
    assert_eq!(retired.revision, 2);
    // The revision was raised by a retune that did not reach this family's schedule.
    for mode in ["smoke", "pilot", "screening", "focused"] {
        assert_eq!(retired.mode(mode), family::CORE.mode(mode));
    }
    // `pilot` calibrates a deadline and has no case matrix to compare.
    for mode in ["smoke", "screening", "focused"] {
        assert_eq!(cases(retired, mode), cases(&family::CORE, mode));
    }
    // A generation nobody retained is absent, not close enough.
    assert!(family::find_at("core-ipc", 1).is_none());
    assert!(family::find_at("core-ipc", 4).is_none());
    assert!(family::find_at("local-ipc-unix", 2).is_none());
    assert!(family::find_at("unknown", 3).is_none());
}

#[test]
fn evidence_written_before_the_counters_still_loads_and_validates() {
    let encoded = std::fs::read_to_string(RETIRED_SMOKE).unwrap();
    let evidence = study::load_str::<system::System>(&encoded).unwrap();
    assert_eq!(evidence.header.specification.family, "core-ipc");
    assert_eq!(evidence.header.specification.family_revision, 2);
    assert_eq!(evidence.complete.rows, 5);
    assert_eq!(evidence.observations.len(), 5);
    assert!(
        evidence
            .observations
            .iter()
            .all(|row| row.measure.boundary.is_none() && row.measure.worker.is_none())
    );
    // The digest covers the header and every row, so these bytes are the artifact the
    // claim was made about and not a reconstruction of it.
    assert_eq!(
        evidence.complete.content,
        "3d5f30189e421cf987c37c1f768cb8932e4fad73c8ecc176be0b22e2010905ea"
    );
}

#[test]
fn a_retired_generation_analyzes_under_its_name_without_counters() {
    let report = analysis::command(&[RETIRED_SMOKE.to_string()]).unwrap();
    assert!(report.contains("## core-ipc/v2 — smoke"));
    assert!(report.contains("evering/notified"));
    assert!(!report.contains("Boundary cost per operation"));
}
