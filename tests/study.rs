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
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || stream::serve(listener.accept().unwrap().0).unwrap());
    let mut client = TcpStream::connect(address).unwrap();
    let payload = model::payload(7, 11, 4096);
    client.write_all(&11_u64.to_le_bytes()).unwrap();
    client
        .write_all(&(payload.len() as u32).to_le_bytes())
        .unwrap();
    client.write_all(&payload).unwrap();
    let mut header = [0; 12];
    client.read_exact(&mut header).unwrap();
    let mut response = vec![0; u32::from_le_bytes(header[8..].try_into().unwrap()) as usize];
    client.read_exact(&mut response).unwrap();
    assert_eq!(u64::from_le_bytes(header[..8].try_into().unwrap()), 11);
    assert!(model::valid_response(7, 11, 4096, &response));
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
        phase_ns: [1; 3],
        observed: Some(model::Observed {
            payload: 64,
            capacity: 8,
            in_flight: 3,
            batch: 3,
            topology: "1c1w".into(),
            transport: "shared-memory".into(),
            extent: Some(4096),
            allocator: Some("adaptive".into()),
            socket_send: None,
            socket_recv: None,
        }),
        status: model::Status::Ok,
        error: None,
    }
}

fn study(trials: Vec<model::Trial>) -> model::Study {
    let expected = trials.len();
    let mut study = model::Study {
        meta: model::Meta {
            format: 2,
            revision: "abc123".into(),
            dirty: false,
            diff: "clean".into(),
            target: "x86_64-test".into(),
            os: "test".into(),
            arch: "x86_64".into(),
            rustc: "rustc-test".into(),
            command: "study --seed 7".into(),
            started: "0".into(),
            mode: "test".into(),
            seed: 7,
            warmup: 1,
            blocks: 1,
            timeout_ms: 1000,
            schedule: 0,
            expected,
            host: "test-host".into(),
            spin: 0,
        },
        trials,
    };
    study.meta.schedule = model::trial_schedule_id(&study.trials);
    study
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
    trial.observed = Some(stream_observed());
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
    busy.observed = Some(stream_observed());
    let mut notified = busy.clone();
    notified.order = 1;
    notified.cell.candidate = "notified".into();
    let evidence = study(vec![busy, notified]);
    assert!(model::validate_study(&evidence).is_ok());
}

fn stream_observed() -> model::Observed {
    model::Observed {
        payload: 64,
        capacity: 8,
        in_flight: 3,
        batch: 3,
        topology: "1c1w".into(),
        transport: "ipv4-loopback".into(),
        extent: None,
        allocator: None,
        socket_send: Some(4096),
        socket_recv: Some(4096),
    }
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
fn recorder_persists_each_row_before_final_publication() {
    use std::fs;

    let root = std::env::temp_dir().join(format!(
        "evering-study-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("row")
    ));
    fs::create_dir_all(&root).unwrap();
    let final_path = root.join("evidence.tsv");
    let partial_path = root.join("evidence.tsv.partial");
    let evidence = study(vec![successful_trial()]);

    let mut recorder = model::Recorder::create(&final_path, evidence.meta.clone()).unwrap();
    recorder.append(evidence.trials[0].clone()).unwrap();
    assert!(!final_path.exists());
    assert_eq!(
        model::decode_prefix(&fs::read_to_string(&partial_path).unwrap()).unwrap(),
        evidence
    );
    recorder.finish().unwrap();
    assert!(!partial_path.exists());
    let complete = fs::read_to_string(&final_path).unwrap();
    assert_eq!(model::decode(&complete).unwrap(), evidence);
    let footer = format!("END\t{}\t", evidence.meta.schedule);
    let mismatched = complete.replacen(
        &footer,
        &format!("END\t{}\t", evidence.meta.schedule.wrapping_add(1)),
        1,
    );
    assert_eq!(
        model::decode(&mismatched).unwrap_err(),
        model::CodecError::Study(model::StudyError::Incomplete)
    );
    assert!(model::Recorder::create(&final_path, evidence.meta).is_err());

    fs::remove_file(final_path).unwrap();
    fs::remove_dir(root).unwrap();
}

#[test]
fn dropped_recorder_leaves_a_valid_incomplete_prefix() {
    use std::{fs, io::Write};

    let root = std::env::temp_dir().join(format!("evering-study-drop-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let final_path = root.join("evidence.tsv");
    let partial_path = root.join("evidence.tsv.partial");
    let evidence = study(vec![successful_trial()]);
    let mut recorder = model::Recorder::create(&final_path, evidence.meta).unwrap();
    recorder.append(evidence.trials[0].clone()).unwrap();
    drop(recorder);

    fs::OpenOptions::new()
        .append(true)
        .open(&partial_path)
        .unwrap()
        .write_all(b"TRIA")
        .unwrap();
    let prefix = fs::read_to_string(&partial_path).unwrap();
    assert!(model::decode_prefix(&prefix).is_ok());
    assert_eq!(
        model::decode(&prefix).unwrap_err(),
        model::CodecError::Study(model::StudyError::Incomplete)
    );
    assert!(!final_path.exists());
    assert!(model::Recorder::create(&final_path, study(Vec::new()).meta).is_err());

    fs::remove_file(partial_path).unwrap();
    fs::remove_dir(root).unwrap();
}

#[test]
fn partial_path_never_authorizes_complete_evidence_at_any_persistence_cut() {
    use std::{fs, io::Write};

    let root = std::env::temp_dir().join(format!("evering-study-cuts-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let final_path = root.join("evidence.tsv");
    let partial_path = root.join("evidence.tsv.partial");
    let mut second = successful_trial();
    second.order = 1;
    second.cell.candidate = "adaptive".into();
    second.cell.policy = "adaptive".into();
    let evidence = study(vec![successful_trial(), second]);
    let mut recorder = model::Recorder::create(&final_path, evidence.meta.clone()).unwrap();
    assert_eq!(model::load(&partial_path).unwrap().study.trials.len(), 0);
    for (index, trial) in evidence.trials.iter().cloned().enumerate() {
        recorder.append(trial).unwrap();
        let loaded = model::load(&partial_path).unwrap();
        assert!(!loaded.complete);
        assert_eq!(loaded.study.trials.len(), index + 1);
    }
    drop(recorder);
    writeln!(
        fs::OpenOptions::new()
            .append(true)
            .open(&partial_path)
            .unwrap(),
        "END\t{}\t{}",
        evidence.meta.schedule,
        evidence.meta.expected
    )
    .unwrap();
    let loaded = model::load(&partial_path).unwrap();
    assert!(!loaded.complete);
    assert_eq!(loaded.study, evidence);

    fs::remove_file(partial_path).unwrap();
    fs::remove_dir(root).unwrap();
}

#[test]
fn actual_configuration_is_required_after_setup() {
    let mut trial = successful_trial();
    trial.observed = None;
    assert_eq!(
        model::validate_trial(&trial).unwrap_err(),
        model::TrialError::MissingObserved
    );
    trial.status = model::Status::TimedError;
    trial.elapsed_ns = None;
    trial.error = Some("timed out".into());
    assert_eq!(
        model::validate_trial(&trial).unwrap_err(),
        model::TrialError::MissingObserved
    );
}

#[test]
fn v2_rejects_corrupt_metadata_schedule_footer_and_elapsed_value() {
    assert_eq!(
        model::decode_prefix("META\t2").unwrap_err(),
        model::CodecError::Syntax
    );
    let mut zero = successful_trial();
    zero.elapsed_ns = Some(0);
    assert_eq!(
        model::validate_trial(&zero).unwrap_err(),
        model::TrialError::ZeroElapsed
    );

    let first = successful_trial();
    let mut second = first.clone();
    second.order = 1;
    second.cell.candidate = "adaptive".into();
    second.cell.policy = "adaptive".into();
    let mut missing = study(vec![first, second]);
    missing.trials.pop();
    assert_eq!(
        model::validate_study(&missing).unwrap_err(),
        model::StudyError::Incomplete
    );

    let root = std::env::temp_dir().join(format!("evering-study-footer-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let final_path = root.join("evidence.tsv");
    let evidence = study(vec![successful_trial()]);
    let mut recorder = model::Recorder::create(&final_path, evidence.meta).unwrap();
    recorder.append(evidence.trials[0].clone()).unwrap();
    recorder.finish().unwrap();
    let mut complete = std::fs::read_to_string(&final_path).unwrap();
    complete.truncate(complete.len() - 2);
    assert!(model::decode(&complete).is_err());
    std::fs::remove_file(final_path).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[test]
fn stalled_peer_exhausts_one_absolute_deadline() {
    use std::{
        io::ErrorKind,
        net::{TcpListener, TcpStream},
        sync::mpsc,
        time::{Duration, Instant},
    };

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let peer = std::thread::spawn(move || {
        let _stream = listener.accept().unwrap().0;
        ready_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    let stream = TcpStream::connect(address).unwrap();
    ready_rx.recv().unwrap();
    stream.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_millis(10);
    let error = stream::read_until(&stream, &mut [0], deadline).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert!(Instant::now() < deadline + Duration::from_millis(100));
    release_tx.send(()).unwrap();
    peer.join().unwrap();
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
    study.meta.expected = 3;
    study.meta.schedule = model::trial_schedule_id(&study.trials);
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
