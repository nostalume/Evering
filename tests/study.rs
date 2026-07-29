#[path = "../benches/ipc/model.rs"]
mod model;
#[path = "../benches/ipc/stream.rs"]
mod stream;

#[test]
fn producer_shares_conserve_exact_requested_work() {
    let shares = model::distribute(7, 3).unwrap();
    assert_eq!(shares, [3, 2, 2]);
    assert_eq!(shares.iter().sum::<u64>(), 7);
}

#[test]
fn deterministic_validation_checks_every_response_byte() {
    let mut response = model::response(model::payload(7, 11, 4096));
    assert!(model::valid_response(7, 11, 4096, &response));
    response[2047] ^= 1;
    assert!(!model::valid_response(7, 11, 4096, &response));
    let truncated = model::response(model::payload(7, 11, 2048));
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

fn cells() -> Vec<model::Cell> {
    (1..=5)
        .map(|capacity| model::Cell {
            implementation: "evering".into(),
            policy: "busy".into(),
            payload: 64,
            capacity,
            in_flight: 1,
            memory: 4096,
        })
        .collect()
}

#[test]
fn seeded_blocks_are_reproducible_but_not_fixed_order() {
    let cells = cells();
    let first = model::schedule(&cells, 2, 7);
    assert_eq!(first, model::schedule(&cells, 2, 7));
    assert_ne!(first, model::schedule(&cells, 2, 8));
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

#[test]
fn zero_workers_is_rejected_and_small_work_is_not_truncated() {
    assert_eq!(
        model::distribute(1, 0).unwrap_err(),
        model::WorkError::NoWorkers
    );
    assert_eq!(model::distribute(2, 4).unwrap(), [1, 1, 0, 0]);
    assert_eq!(model::distribute(0, 3).unwrap(), [0, 0, 0]);
}
