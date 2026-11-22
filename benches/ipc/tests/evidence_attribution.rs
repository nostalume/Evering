//! Attribution evidence: the environment snapshot a figure is quoted in, the writers
//! a system run goes through, and the analysis that accepts only an exact path,
//! direction, intervention and provenance. `system_header` and `system_measure` here
//! are what the two boundary areas measure with.

use crate::{analysis, boundary, drive, environment, family, model, study, system};

#[test]
fn environment_snapshot_is_stable_complete_and_self_identifying() {
    let first = environment::capture().unwrap();
    let second = environment::capture().unwrap();
    assert_eq!(first, second);
    for field in [
        "os=",
        "arch=",
        "logical=",
        "affinity=",
        "page=",
        "power=",
        "kernel=",
        "native=",
        "physical=",
        "smt=",
        "thermal=",
        "background=",
    ] {
        assert!(first.text.split(';').any(|value| value.starts_with(field)));
    }
    assert_eq!(first.digest, environment::digest(first.text.as_bytes()));
    assert_eq!(environment::admit(&first.digest).unwrap(), first);
    assert!(environment::admit("0000000000000000").is_err());
}

pub(super) fn system_header(mode: &str) -> study::Header<system::Specification, system::Case> {
    let family = &family::CORE;
    let blocks = family.mode(mode).unwrap().1;
    let selected = model::schedule(&(family.members)(mode).unwrap(), blocks, 7);
    let mut cases = Vec::new();
    let mut schedule = Vec::new();
    for entry in selected {
        let case = system::Case {
            workload: entry.cell(),
            resources: entry.arm.resources(),
        };
        let index = cases
            .iter()
            .position(|known| known == &case)
            .unwrap_or_else(|| {
                cases.push(case);
                cases.len() - 1
            });
        schedule.push(study::Scheduled {
            unit: study::Unit {
                block: entry.block,
                order: entry.order,
            },
            case: index as u32,
        });
    }
    let contrasts = cases
        .iter()
        .filter(|case| case.workload.arm != family.baseline.key)
        .filter_map(|candidate| {
            let baseline = cases.iter().find(|case| {
                case.workload.condition() == candidate.workload.condition()
                    && case.workload.arm == family.baseline.key
            })?;
            Some(system::Contrast {
                candidate: study::case_id::<system::System>(candidate),
                baseline: study::case_id::<system::System>(baseline),
                delta: 0.05,
                role: system::Role::Primary,
            })
        })
        .collect();
    study::Header::new::<system::System>(
        study::Context {
            source: study::Source {
                revision: "revision".into(),
                dirty: false,
                diff: "diff".into(),
            },
            compiler: study::Compiler {
                target: "target".into(),
                rustc: "rustc".into(),
            },
            host: study::Host {
                os: "os".into(),
                arch: "arch".into(),
                description: "host".into(),
                environment: "environment".into(),
            },
        },
        system::Specification {
            family: family.key.into(),
            family_revision: family.revision,
            mode: mode.into(),
            blocks,
            spin: 0,
            timeout_ms: 5_000,
            alpha: 0.05,
            calibration: Some("calibration".into()),
            contrasts,
        },
        cases,
        study::Run {
            seed: 7,
            started: "started".into(),
            command: "command".into(),
            warmup: 8,
            budget_ms: family.mode(mode).unwrap().0.as_millis() as u64,
            schedule,
        },
    )
}

pub(super) fn system_measure(case: &system::Case, elapsed_ns: u64) -> system::Measure {
    let requested = 100;
    system::Measure {
        requested,
        accepted: requested,
        completed: requested,
        validated: requested,
        elapsed_ns,
        phase_ns: [1, elapsed_ns, 1],
        observed: model::Observed {
            payload: case.workload.payload,
            capacity: case.workload.capacity,
            in_flight: case.workload.in_flight,
            window: model::window(requested, case.workload.capacity, case.workload.in_flight),
            topology: case.resources.topology.clone(),
            transport: case.resources.transport.clone(),
            extent: case.resources.extent.then_some(case.workload.memory),
            allocator: case.resources.allocator.then(|| "pool".into()),
            socket_send: case.resources.socket_send.then_some(4096),
            socket_recv: case.resources.socket_recv.then_some(4096),
        },
        path: drive::PathCounts {
            send_attempts: requested,
            recv_attempts: requested,
            ..drive::PathCounts::default()
        },
        boundary: None,
        worker: None,
    }
}

pub(super) fn record_system_evidence(
    path: &std::path::Path,
    mode: &str,
    candidate_ratio: f64,
    revision: &str,
) {
    record_system_evidence_with(path, mode, candidate_ratio, revision, |_, _| None);
}

/// The same run with a boundary figure attached to the rows a caller selects. The
/// figure is handed the case and the phase width so it can stay inside the admission
/// rule that bounds attributed run time by the window containing it.
pub(super) fn record_system_evidence_with(
    path: &std::path::Path,
    mode: &str,
    candidate_ratio: f64,
    revision: &str,
    cost: impl Fn(&system::Case, u64) -> Option<boundary::Accounting>,
) {
    let mut header = system_header(mode);
    header.context.source.revision = revision.into();
    let execution = header.run.schedule.clone();
    let cases = header.cases.clone();
    let baseline = family::CORE.baseline.key;
    let mut recorder = study::Recorder::<system::System>::create(path, header).unwrap();
    for expected in execution {
        let case = &cases[expected.case as usize];
        let elapsed = if case.workload.arm == baseline {
            1_000_000
        } else {
            (1_000_000.0 / candidate_ratio) as u64
        };
        let mut measure = system_measure(case, elapsed);
        measure.boundary = cost(case, elapsed);
        recorder
            .observe(study::Observation {
                unit: expected.unit,
                case: expected.case,
                measure,
            })
            .unwrap();
    }
    recorder.complete().unwrap();
}

pub(super) fn system_evidence_with(
    mode: &str,
    candidate_ratio: f64,
    cost: impl Fn(&system::Case, u64) -> Option<boundary::Accounting>,
) -> system::Evidence {
    let path = std::env::temp_dir().join(format!("evering-system-{}", fastrand::u64(..)));
    record_system_evidence_with(&path, mode, candidate_ratio, "revision", cost);
    let evidence = system::load(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    evidence
}

fn system_evidence_at(mode: &str, candidate_ratio: f64, revision: &str) -> system::Evidence {
    let path = std::env::temp_dir().join(format!("evering-system-{}", fastrand::u64(..)));
    record_system_evidence(&path, mode, candidate_ratio, revision);
    let evidence = system::load(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    evidence
}

pub(super) fn system_evidence(mode: &str, candidate_ratio: f64) -> system::Evidence {
    system_evidence_at(mode, candidate_ratio, "revision")
}

fn mechanism_analysis(system: &system::Evidence) -> analysis::MechanismAnalysis {
    let target = system.header.specification.contrasts[0].candidate.clone();
    analysis::MechanismAnalysis {
        schema: "mechanism.queue.reserve-publish".into(),
        evidence: "mechanism-evidence".into(),
        context: system.header.context.clone(),
        delta_ns: 1.0,
        system_delta: 0.05,
        estimates: vec![analysis::MechanismEstimate {
            target,
            target_admitted: Some(true),
            case: r#"{"payload":0}"#.into(),
            paired_ns: vec![10.0; 9],
            gross_ns: vec![12.0; 9],
            control_ns: vec![2.0; 9],
            iqr: [10.0, 10.0],
            interval: analysis::Interval {
                point: 10.0,
                low: 10.0,
                high: 10.0,
            },
            decision: analysis::Decision::Slower,
        }],
    }
}

#[test]
fn attribution_requires_exact_path_direction_intervention_and_provenance() {
    let before = system_evidence_at("focused", 0.8, "before");
    let after = system_evidence_at("focused", 0.95, "after");
    let mechanism = mechanism_analysis(&before);

    assert_eq!(
        analysis::attribute(&before, None, None).unwrap().authority,
        analysis::Authority::SystemEffect
    );
    assert_eq!(
        analysis::attribute(&before, Some(&mechanism), Some(&after))
            .unwrap()
            .authority,
        analysis::Authority::Attributed
    );
    let mut conflict = mechanism_analysis(&before);
    conflict.estimates[0].target_admitted = Some(false);
    assert_eq!(
        analysis::attribute(&before, Some(&conflict), Some(&after))
            .unwrap()
            .authority,
        analysis::Authority::Inconclusive
    );
    let mut conflict = mechanism_analysis(&before);
    conflict.estimates[0].target = "f".repeat(64);
    assert_eq!(
        analysis::attribute(&before, Some(&conflict), Some(&after))
            .unwrap()
            .authority,
        analysis::Authority::Inconclusive
    );
    let mut conflict = mechanism_analysis(&before);
    conflict.estimates[0].decision = analysis::Decision::Faster;
    assert_eq!(
        analysis::attribute(&before, Some(&conflict), Some(&after))
            .unwrap()
            .authority,
        analysis::Authority::Inconclusive
    );
    let mut conflict = mechanism_analysis(&before);
    conflict.schema = "mechanism.talc.allocate-release".into();
    assert_eq!(
        analysis::attribute(&before, Some(&conflict), Some(&after))
            .unwrap()
            .authority,
        analysis::Authority::Inconclusive
    );
    let mut conflict = mechanism_analysis(&before);
    conflict.context.source.revision = "foreign".into();
    assert_eq!(
        analysis::attribute(&before, Some(&conflict), Some(&after))
            .unwrap()
            .authority,
        analysis::Authority::Inconclusive
    );
    let insufficient = system_evidence_at("focused", 0.82, "insufficient");
    assert_eq!(
        analysis::attribute(&before, Some(&mechanism), Some(&insufficient))
            .unwrap()
            .authority,
        analysis::Authority::Inconclusive
    );
}

#[test]
fn system_schema_registers_resources_contrasts_and_exact_schedule() {
    let evidence = system_evidence("focused", 1.2);
    let analysis = analysis::analyze(&evidence).unwrap();
    assert_eq!(analysis.estimates.len(), 5);
    assert!(
        analysis
            .estimates
            .iter()
            .all(|estimate| estimate.effect > 1.19)
    );
    assert_eq!(analysis.authority, analysis::Authority::SystemEffect);
    let markdown = analysis.markdown();
    assert!(markdown.contains("candidate op/s | baseline op/s"));
    assert!(markdown.contains("candidate MiB/s | baseline MiB/s"));
    assert!(
        analysis
            .decisions
            .iter()
            .all(|decision| *decision == analysis::Decision::Faster)
    );
    assert_eq!(
        evidence.header.run.schedule.len(),
        evidence.observations.len()
    );
    assert!(
        evidence
            .header
            .specification
            .contrasts
            .iter()
            .all(|contrast| contrast.delta == 0.05)
    );
}

#[test]
fn system_schema_rejects_wrong_resources_counts_and_duplicate_cases() {
    let path = std::env::temp_dir().join(format!("evering-system-invalid-{}", fastrand::u64(..)));
    let header = system_header("focused");
    let execution = header.run.schedule[0];
    let case = header.cases[execution.case as usize].clone();
    let mut invalid = system_measure(&case, 1);
    invalid.accepted -= 1;
    let mut recorder = study::Recorder::<system::System>::create(&path, header).unwrap();
    assert_eq!(
        recorder.observe(study::Observation {
            unit: execution.unit,
            case: execution.case,
            measure: invalid,
        }),
        Err(study::Error::Invalid)
    );
    let mut invalid = system_measure(&case, 1);
    invalid.observed.transport = "wrong".into();
    assert_eq!(
        recorder.observe(study::Observation {
            unit: execution.unit,
            case: execution.case,
            measure: invalid,
        }),
        Err(study::Error::Invalid)
    );
    recorder.abort(Some(execution.unit), "invalid").unwrap();
    assert!(system::load(&path).is_err());
    std::fs::remove_file(&path).unwrap();

    let mut duplicate = system_header("focused");
    duplicate.cases.push(duplicate.cases[0].clone());
    assert_eq!(
        study::Recorder::<system::System>::create(&path, duplicate).err(),
        Some(study::Error::Invalid)
    );
    let mut duplicate = system_header("focused");
    let contrast = duplicate.specification.contrasts[0].clone();
    duplicate.specification.contrasts.push(contrast);
    assert_eq!(
        study::Recorder::<system::System>::create(&path, duplicate).err(),
        Some(study::Error::Invalid)
    );
}
