#[allow(dead_code)]
#[path = "../benches/ipc/model.rs"]
mod model;
#[allow(dead_code)]
#[path = "../benches/ipc/stream.rs"]
mod stream;

#[test]
fn deterministic_validation_checks_every_response_byte() {
    let mut response = model::payload(7, 11, 4096);
    response.iter_mut().for_each(|byte| *byte ^= 0xa5);
    assert!(model::valid_response(7, 11, 4096, &response));
    response[2047] ^= 1;
    assert!(!model::valid_response(7, 11, 4096, &response));
    let mut truncated = model::payload(7, 11, 2048);
    truncated.iter_mut().for_each(|byte| *byte ^= 0xa5);
    assert!(!model::valid_response(7, 11, 4096, &truncated));
}

#[test]
fn framed_stream_round_trip_validates_complete_payload() {
    use std::net::{Shutdown, TcpListener, TcpStream};

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || stream::serve(listener.accept().unwrap().0).unwrap());
    let mut client = TcpStream::connect(address).unwrap();
    assert!(stream::round_trip(&mut client, 7, 11, 4096).unwrap());
    client.shutdown(Shutdown::Write).unwrap();
    worker.join().unwrap();
}

fn successful_trial() -> model::Trial {
    model::Trial {
        block: 0,
        order: 0,
        cell: model::Cell {
            implementation: "evering".into(),
            policy: "busy".into(),
            candidate: "busy".into(),
            payload: 64,
            capacity: 8,
            in_flight: 3,
            memory: 4096,
        },
        requested: 7,
        accepted: 7,
        completed: 7,
        validated: 7,
        elapsed_ns: Some(1),
        status: model::Status::Ok,
        error: None,
    }
}

fn study(trials: Vec<model::Trial>) -> model::Study {
    model::Study {
        meta: model::Meta {
            format: 1,
            revision: "abc123".into(),
            dirty: false,
            target: "x86_64-test".into(),
            os: "test".into(),
            arch: "x86_64".into(),
            rustc: "rustc-test".into(),
            command: "study --seed 7".into(),
            started: "0".into(),
            seed: 7,
            warmup: 1,
            blocks: 1,
            timeout_ms: 1000,
        },
        trials,
    }
}

fn contrast(policy: model::Policy) -> model::Contrast {
    model::Contrast {
        key: model::ContrastKey {
            payload: 64,
            capacity: 8,
            in_flight: 3,
            memory: 4096,
        },
        candidate: policy,
    }
}

#[test]
fn schedule_keeps_exact_candidate_baseline_pairs() {
    use model::{Arm, Policy};

    let contrasts = [contrast(Policy::Busy), contrast(Policy::Notified)];
    let scheduled = model::schedule(&contrasts, 3, 7);
    for block in 0..3 {
        for contrast in &contrasts {
            let arms: Vec<_> = scheduled
                .iter()
                .filter(|trial| trial.block == block && trial.contrast == *contrast)
                .map(|trial| trial.arm)
                .collect();
            assert_eq!(arms.len(), 2);
            assert!(arms.contains(&Arm::Evering(contrast.candidate)));
            assert!(arms.contains(&Arm::Stream));
        }
    }
}

#[test]
fn schedule_randomizes_arm_order_without_changing_membership() {
    let contrasts = [contrast(model::Policy::Adaptive)];
    let first = model::schedule(&contrasts, 8, 7);
    let second = model::schedule(&contrasts, 8, 8);
    assert_eq!(first.len(), second.len());
    assert_ne!(
        first.iter().map(|entry| entry.arm).collect::<Vec<_>>(),
        second.iter().map(|entry| entry.arm).collect::<Vec<_>>()
    );
}

#[test]
fn stream_window_obeys_capacity_and_in_flight() {
    assert_eq!(model::window(100, 1, 64), 1);
    assert_eq!(model::window(100, 8, 64), 8);
    assert_eq!(model::window(3, 8, 64), 3);
}

#[test]
fn baseline_cannot_claim_an_evering_policy() {
    let mut trial = successful_trial();
    trial.cell.implementation = "os-stream".into();
    trial.cell.policy = "notified".into();
    assert_eq!(
        model::validate_trial(&trial).unwrap_err(),
        model::TrialError::InvalidCell
    );
}

#[test]
fn baseline_rows_keep_the_candidate_contrast_identity() {
    let mut busy = successful_trial();
    busy.cell.implementation = "os-stream".into();
    busy.cell.policy = "blocking".into();
    let mut notified = busy.clone();
    notified.order = 1;
    notified.cell.candidate = "notified".into();
    let evidence = study(vec![busy, notified]);
    assert!(model::validate_study(&evidence).is_ok());
    assert_eq!(
        model::decode(&model::encode(&evidence).unwrap()).unwrap(),
        evidence
    );
}

#[test]
fn mandatory_failure_is_not_a_successful_run() {
    let success = successful_trial();
    assert!(model::mandatory_success(core::slice::from_ref(&success)));
    let mut failed = success;
    failed.status = model::Status::Unsupported;
    failed.accepted = 0;
    failed.completed = 0;
    failed.validated = 0;
    failed.elapsed_ns = None;
    failed.error = Some("not implemented".into());
    assert!(!model::mandatory_success(&[failed]));
}

#[test]
fn seeded_blocks_are_reproducible_but_not_fixed_order() {
    let contrasts = [
        contrast(model::Policy::Busy),
        contrast(model::Policy::Adaptive),
        contrast(model::Policy::Notified),
    ];
    let first = model::schedule(&contrasts, 2, 7);
    assert_eq!(first, model::schedule(&contrasts, 2, 7));
    assert_ne!(first, model::schedule(&contrasts, 2, 8));
}

#[test]
fn study_rejects_duplicate_block_cell_rows() {
    let trial = successful_trial();
    assert_eq!(
        model::validate_study(&study(vec![trial.clone(), trial])).unwrap_err(),
        model::StudyError::DuplicateCell
    );
}

#[test]
fn raw_schema_round_trips_without_losing_environment_or_trials() {
    let study = study(vec![successful_trial()]);
    let encoded = model::encode(&study).unwrap();
    assert_eq!(model::decode(&encoded).unwrap(), study);
}

#[test]
fn raw_schema_rejects_truncation_unknown_versions_and_control_bytes() {
    let study = study(vec![successful_trial()]);
    let mut encoded = model::encode(&study).unwrap();
    encoded.truncate(encoded.find("\nTRIAL").unwrap());
    assert_eq!(
        model::decode(&encoded).unwrap_err(),
        model::CodecError::Study(model::StudyError::IncompleteBlock)
    );

    let encoded = model::encode(&study)
        .unwrap()
        .replacen("META\t1\t", "META\t2\t", 1);
    assert_eq!(
        model::decode(&encoded).unwrap_err(),
        model::CodecError::Study(model::StudyError::Format)
    );

    let mut study = study;
    study.trials[0].error = Some("line\nbreak".into());
    assert_eq!(
        model::encode(&study).unwrap_err(),
        model::CodecError::Study(model::StudyError::Trial(model::TrialError::InvalidCell))
    );
}

#[test]
fn study_requires_complete_blocks_and_environment_identity() {
    let mut first = successful_trial();
    let mut second = successful_trial();
    second.block = 1;
    let mut study = study(vec![first.clone(), second]);
    study.meta.blocks = 2;
    assert!(model::validate_study(&study).is_ok());

    first.cell.capacity = 16;
    first.order = 1;
    study.trials.push(first);
    assert_eq!(
        model::validate_study(&study).unwrap_err(),
        model::StudyError::IncompleteBlock
    );

    study.trials.pop();
    study.meta.revision.clear();
    assert_eq!(
        model::validate_study(&study).unwrap_err(),
        model::StudyError::Metadata
    );
}

#[test]
fn successful_trial_requires_exact_conserved_work() {
    let mut trial = successful_trial();
    trial.accepted -= 1;
    trial.completed -= 1;
    trial.validated -= 1;
    assert_eq!(
        model::validate_trial(&trial).unwrap_err(),
        model::TrialError::CountMismatch
    );
}

#[test]
fn trial_status_cannot_disguise_failure_as_timing() {
    let mut trial = successful_trial();
    trial.status = model::Status::TimedError;
    trial.error = Some("worker exited".into());
    assert_eq!(
        model::validate_trial(&trial).unwrap_err(),
        model::TrialError::UnexpectedElapsed
    );

    trial.elapsed_ns = None;
    assert!(model::validate_trial(&trial).is_ok());
}

#[test]
fn unsupported_cell_has_reason_and_no_work_or_timing() {
    let mut trial = successful_trial();
    trial.status = model::Status::Unsupported;
    trial.accepted = 0;
    trial.completed = 0;
    trial.validated = 0;
    trial.elapsed_ns = None;
    trial.error = Some("policy unavailable".into());
    assert!(model::validate_trial(&trial).is_ok());

    trial.error = None;
    assert_eq!(
        model::validate_trial(&trial).unwrap_err(),
        model::TrialError::MissingError
    );
}

#[test]
fn every_failure_phase_has_no_performance_value() {
    for status in [
        model::Status::Invalid,
        model::Status::SetupError,
        model::Status::TimedError,
        model::Status::DrainError,
    ] {
        let mut trial = successful_trial();
        trial.status = status;
        trial.elapsed_ns = None;
        trial.error = Some("phase failed".into());
        assert!(model::validate_trial(&trial).is_ok());
    }
}
