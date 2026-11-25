//! The pilot run: the calibration one measurable ramp has to yield before a deadline
//! is frozen, the counts admission will not accept, the manifest a rerun is bound to by
//! digest, and the partial file a killed pilot leaves behind. Nested deadlines are here
//! because they are the same command-bound arithmetic.

use crate::tests::membership_schedule::{cell, condition, contrast};
use crate::{analysis, drive, family, model, pilot};

fn pilot_identity() -> pilot::Identity {
    pilot::Identity {
        algorithm: 4,
        family: "core-ipc".into(),
        family_revision: family::CORE.revision,
        revision: "revision".into(),
        dirty: false,
        diff: "diff".into(),
        target: "target".into(),
        os: "os".into(),
        arch: "arch".into(),
        rustc: "rustc".into(),
        host: "host".into(),
        environment: "environment".into(),
        command: "command".into(),
        started: "started".into(),
        seed: 7,
        warmup: 8,
        timeout_ms: 5_000,
    }
}

#[test]
fn one_measurable_ramp_freezes_the_calibration_without_a_probe() {
    let cell = cell(&family::BUSY);
    let mut calls = 0;
    let row = pilot::calibrate(cell, 8, |_| {
        calls += 1;
        Ok(if calls == 1 { 49_000_000 } else { 110_000_000 })
    })
    .unwrap();
    assert_eq!((row.count, row.observations.len(), calls), (225, 2, 2));
}

#[test]
fn a_zero_duration_observation_is_refused() {
    let cell = cell(&family::BUSY);
    assert!(pilot::calibrate(cell, 8, |_| Ok(0)).is_err());
}

#[test]
fn attempts_overflow_timeout_and_identity_are_bounded() {
    let mut attempts = 0;
    assert!(
        pilot::calibrate(cell(&family::BUSY), 8, |_| {
            attempts += 1;
            Ok(49_999_999)
        })
        .is_err()
    );
    assert_eq!(attempts, 8);

    let mut geometry = cell(&family::BUSY);
    geometry.capacity = u64::MAX;
    geometry.in_flight = u64::MAX;
    assert!(pilot::calibrate(geometry, 8, |_| unreachable!()).is_err());
    assert_eq!(
        pilot::calibrate(cell(&family::BUSY), 8, |_| Err("timeout".into())).unwrap_err(),
        "timeout"
    );
    assert!(pilot::calibrate(cell(&family::BUSY), u64::MAX, |_| Ok(50_000_000)).is_err());
}

fn calibration_evidence(identity: pilot::Identity, rows: Vec<pilot::Row>) -> pilot::Evidence {
    let path = std::env::temp_dir().join(format!("evering-calibration-{}", fastrand::u64(..)));
    let cases = rows.iter().map(|row| row.cell.clone()).collect();
    pilot::record(&path, identity, cases, rows.into_iter().map(Ok)).unwrap();
    let evidence = pilot::load(&path).unwrap().0;
    std::fs::remove_file(path).unwrap();
    evidence
}

#[test]
fn admission_refuses_foreign_missing_duplicate_and_unverified_counts() {
    let scheduled = model::schedule(
        &[contrast(&family::BUSY), (condition(), &family::STREAM)],
        2,
        7,
    );
    let cells: Vec<_> = scheduled[..2].iter().map(|entry| entry.cell()).collect();
    let verified = |cell| pilot::calibrate(cell, 8, |_| Ok(500_000_000)).unwrap();
    let evidence =
        calibration_evidence(pilot_identity(), cells.into_iter().map(verified).collect());
    assert_eq!(
        pilot::admit(&evidence, &pilot_identity(), &scheduled).unwrap(),
        vec![96; 4]
    );
    let mut reuse = pilot_identity();
    reuse.command = "screening invocation".into();
    reuse.started = "later".into();
    assert_eq!(
        pilot::admit(&evidence, &reuse, &scheduled).unwrap(),
        vec![96; 4]
    );
    let mut foreign = pilot_identity();
    foreign.target = "foreign".into();
    assert!(pilot::admit(&evidence, &foreign, &scheduled).is_err());
    foreign = pilot_identity();
    foreign.family_revision += 1;
    assert!(pilot::admit(&evidence, &foreign, &scheduled).is_err());
    let mut unsupported = pilot_identity();
    unsupported.algorithm = 2;
    let unsupported_evidence = calibration_evidence(
        unsupported.clone(),
        scheduled[..2]
            .iter()
            .map(|entry| verified(entry.cell()))
            .collect(),
    );
    assert!(pilot::admit(&unsupported_evidence, &unsupported, &scheduled).is_err());
    assert!(pilot::admit(&evidence, &pilot_identity(), &scheduled[..1]).is_err());

    let duplicate = scheduled[0].cell();
    let path = std::env::temp_dir().join(format!("evering-duplicate-{}", fastrand::u64(..)));
    assert!(
        pilot::record(
            &path,
            pilot_identity(),
            vec![duplicate.clone(), duplicate.clone()],
            [Ok(verified(duplicate.clone())), Ok(verified(duplicate))],
        )
        .is_err()
    );
    assert!(!path.exists());

    let path = std::env::temp_dir().join(format!("evering-mismatch-{}", fastrand::u64(..)));
    assert!(
        pilot::record(
            &path,
            pilot_identity(),
            vec![scheduled[0].cell()],
            [Ok(verified(scheduled[1].cell()))],
        )
        .is_err()
    );
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("row does not match case")
    );
    std::fs::remove_file(path).unwrap();

    let mut invalid = verified(scheduled[0].cell());
    invalid.count = 99;
    let path = std::env::temp_dir().join(format!("evering-invalid-{}", fastrand::u64(..)));
    assert!(
        pilot::record(
            &path,
            pilot_identity(),
            vec![invalid.cell.clone()],
            [Ok(invalid)],
        )
        .is_err()
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn pilot_manifest_round_trip_is_digest_bound_and_non_overwriting() {
    let scheduled = model::schedule(
        &[contrast(&family::BUSY), (condition(), &family::STREAM)],
        1,
        7,
    );
    let identity = pilot_identity();
    let rows: Vec<_> = scheduled
        .iter()
        .map(|entry| {
            let cell = entry.cell();
            pilot::calibrate(cell, 8, |count| Ok(count * 10_000_000)).unwrap()
        })
        .collect();
    let path = std::env::temp_dir().join(format!("evering-pilot-{}", std::process::id()));
    let digest = pilot::record(
        &path,
        identity.clone(),
        rows.iter().map(|row| row.cell.clone()).collect(),
        rows.iter().cloned().map(Ok),
    )
    .unwrap();
    let (evidence, loaded_digest) = pilot::load(&path).unwrap();
    assert_eq!(
        (pilot::identity(&evidence), loaded_digest),
        (identity.clone(), digest)
    );
    assert_eq!(evidence.observations.len(), rows.len());
    let report = analysis::command(&[path.to_string_lossy().into_owned()]).unwrap();
    assert!(report.contains("Calibration evidence"));
    assert!(
        pilot::record(
            &path,
            identity,
            rows.iter().map(|row| row.cell.clone()).collect(),
            std::iter::empty::<Result<pilot::Row, String>>(),
        )
        .is_err()
    );
    let mut corrupt = std::fs::read_to_string(&path).unwrap();
    corrupt = corrupt.replacen("revision", "foreign", 1);
    std::fs::write(&path, corrupt).unwrap();
    assert!(pilot::load(&path).is_err());
    std::fs::remove_file(path).unwrap();
}

#[test]
fn pilot_partial_is_durable_non_admissible_and_sanitizes_abort() {
    let scheduled = model::schedule(
        &[contrast(&family::BUSY), (condition(), &family::STREAM)],
        1,
        7,
    );
    let cell = scheduled[0].cell();
    let path = std::env::temp_dir().join(format!("evering-pilot-cut-{}", std::process::id()));
    let row = pilot::calibrate(cell.clone(), 8, |count| Ok(count * 10_000_000)).unwrap();
    assert!(
        pilot::record(
            &path,
            pilot_identity(),
            vec![cell.clone(), scheduled[1].cell()],
            [Ok(row), Err("bad\tline\nreason".into())],
        )
        .is_err()
    );
    assert!(pilot::load(&path).is_err());
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"kind\":\"abort\""));
    assert!(text.contains("bad line reason"));
    assert!(!text.contains("bad\tline\n"));
    std::fs::remove_file(path).unwrap();
}

#[test]
fn nested_deadlines_never_reset_or_exceed_the_command() {
    use std::time::{Duration, Instant};

    let now = Instant::now();
    let at = |seconds| now + Duration::from_secs(seconds);
    let command = drive::Deadline::after(now, Duration::from_secs(10)).unwrap();
    let early = command.within(at(2), Duration::from_secs(3)).unwrap();
    let capped = command.within(at(8), Duration::from_secs(5)).unwrap();
    assert_eq!(early.remaining(at(2)), Ok(Duration::from_secs(3)));
    assert_eq!(capped.remaining(at(8)), Ok(Duration::from_secs(2)));
    assert!(capped.remaining(at(10)).is_err());
    assert!(command.within(at(10), Duration::from_secs(1)).is_err());
    assert!(drive::Deadline::after(now, Duration::MAX).is_err());
    let cell = cell(&family::BUSY);
    assert_eq!(
        pilot::progress("pilot-start", 1, 2, &cell),
        "pilot-start 1/2: evering/busy payload=64 capacity=8 in-flight=3"
    );
}
