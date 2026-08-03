use super::{
    analysis, drive, environment, evering, family, fixture, geometry, mechanism, model, pilot,
    stream, study, system,
};

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
struct ExampleSpec;

impl study::Identity for ExampleSpec {
    fn identity(&self, _: &mut blake3::Hasher) {}
}

struct ExampleSchema;

impl study::Schema for ExampleSchema {
    const NAME: &'static str = "example";
    const REVISION: u32 = 1;
    type Specification = ExampleSpec;
    type Case = u64;
    type Measure = u64;

    fn validate(
        header: &study::Header<Self::Specification, Self::Case>,
        observation: &study::Observation<Self::Measure>,
    ) -> Result<(), String> {
        (header.cases.get(observation.case as usize) == Some(&observation.measure))
            .then_some(())
            .ok_or_else(|| "measure does not match case".into())
    }
}

struct ForeignSchema;

impl study::Schema for ForeignSchema {
    const NAME: &'static str = "foreign";
    const REVISION: u32 = 1;
    type Specification = ExampleSpec;
    type Case = u64;
    type Measure = u64;

    fn validate(
        _: &study::Header<Self::Specification, Self::Case>,
        _: &study::Observation<Self::Measure>,
    ) -> Result<(), String> {
        Ok(())
    }
}

struct ExampleFixture(u64);

impl mechanism::Fixture for ExampleFixture {
    type Parameters = u64;
    type Case = u64;

    fn prepare(_: &u64, _: &u64) -> Result<Self, String> {
        Ok(Self(0))
    }

    fn state(&self) -> Result<String, String> {
        Ok(self.0.to_string())
    }

    fn limit(&self) -> std::num::NonZeroU64 {
        std::num::NonZeroU64::new(1).unwrap()
    }

    fn setup(&mut self, _: u64, _: mechanism::Body) -> Result<(), String> {
        Ok(())
    }

    fn gross(&mut self, operations: u64) -> Result<(), String> {
        self.0 = operations;
        Ok(())
    }

    fn control(&mut self, operations: u64) -> Result<(), String> {
        self.0 = operations;
        Ok(())
    }

    fn reset(&mut self) -> Result<(), String> {
        self.0 = 0;
        Ok(())
    }
}

struct ExampleMechanism;

impl mechanism::FixtureSchema for ExampleMechanism {
    const KEY: &'static str = "mechanism.example";
    type Parameters = u64;
    type Case = u64;
    type Fixture = ExampleFixture;

    fn matches(_: &u64, _: &u64, _: &system::Case) -> bool {
        true
    }
}

fn mechanism_header(seed: u64) -> study::Header<mechanism::Specification<u64>, u64> {
    let (schedule, orders) = mechanism::schedule(seed, 2, 3);
    study::Header::new::<mechanism::Mechanism<ExampleMechanism>>(
        study::Context::default(),
        mechanism::Specification {
            targets: vec!["a".repeat(64), "b".repeat(64)],
            parameters: 8,
            policy: mechanism::Policy {
                min_ns: 1,
                max_ns: u64::MAX,
                pairs: 3,
                calibration_attempts: 1,
            },
            alpha: 0.05,
            delta_ns: 1.0,
            system_delta: 0.05,
            orders,
        },
        vec![1, 2],
        study::Run {
            seed,
            budget_ms: 1_000,
            schedule,
            ..study::Run::default()
        },
    )
}

#[test]
fn mechanism_schema_batches_pairs_and_returns_to_initial_state() {
    let root = std::env::temp_dir().join(format!("evering-mechanism-{}", fastrand::u64(..)));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("evidence.jsonl");
    mechanism::record::<ExampleMechanism>(&path, mechanism_header(7)).unwrap();
    let evidence = study::load::<mechanism::Mechanism<ExampleMechanism>>(&path).unwrap();

    assert_eq!(evidence.observations.len(), 6);
    assert!(evidence.observations.iter().all(|row| {
        row.measure.operations == 1
            && row.measure.before == "0"
            && row.measure.after == row.measure.before
    }));
    let repeated = mechanism_header(7);
    assert_eq!(evidence.header.run.schedule, repeated.run.schedule);
    assert_eq!(
        evidence.header.specification.orders,
        repeated.specification.orders
    );
    assert!(
        evidence
            .header
            .specification
            .orders
            .contains(&mechanism::Order::GrossControl)
    );
    assert!(
        evidence
            .header
            .specification
            .orders
            .contains(&mechanism::Order::ControlGross)
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn mechanism_registration_is_exact_closed_and_typed() {
    assert_eq!(
        fixture::KEYS,
        [
            "mechanism.queue.reserve-publish",
            "mechanism.queue.claim-recycle",
            "mechanism.pool.allocate-release",
            "mechanism.talc.allocate-release",
            "mechanism.notify",
            "mechanism.signal-wait",
        ]
    );
}

#[test]
fn registered_mechanisms_admit_typed_specs_and_seal_real_fixtures() {
    let root = std::env::temp_dir().join(format!("evering-fixtures-{}", fastrand::u64(..)));
    std::fs::create_dir(&root).unwrap();
    let common = serde_json::json!({
        "targets": ["a".repeat(64)],
        "policy": { "min_ns": 1, "max_ns": u64::MAX, "pairs": 1, "calibration_attempts": 1 },
        "seed": 7,
        "budget_ms": 5_000,
        "alpha": 0.05,
        "delta_ns": 1.0,
        "system_delta": 0.05,
    });
    for (index, (key, parameters, cases)) in [
        (
            fixture::KEYS[0],
            serde_json::json!({ "extent": 1 << 20, "storage": 1 << 16, "capacity": 8 }),
            serde_json::json!([{ "payload": 0 }]),
        ),
        (
            fixture::KEYS[1],
            serde_json::json!({ "extent": 1 << 20, "storage": 1 << 16, "capacity": 8 }),
            serde_json::json!([{ "payload": 0 }]),
        ),
        (
            fixture::KEYS[2],
            serde_json::json!({ "extent": 1 << 20, "storage": 1 << 16, "placement": "shared-mapping" }),
            serde_json::json!([{ "bytes": 64, "alignment": 1, "occupancy": 0, "touch": true }]),
        ),
        (
            fixture::KEYS[3],
            serde_json::json!({ "extent": 1 << 20, "storage": 1 << 16, "placement": "shared-mapping" }),
            serde_json::json!([{ "bytes": 64, "alignment": 1, "occupancy": 0, "touch": true }]),
        ),
        (
            fixture::KEYS[4],
            serde_json::json!({ "limit": 1 }),
            serde_json::json!([{ "sticky": true }]),
        ),
        (
            fixture::KEYS[5],
            serde_json::json!({ "limit": 1 }),
            serde_json::json!([{ "sticky": true }]),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut specification = common.clone();
        specification["fixture"] = key.into();
        specification["parameters"] = parameters;
        specification["cases"] = cases;
        let input = root.join(format!("{index}.json"));
        let output = root.join(format!("{index}.jsonl"));
        std::fs::write(&input, serde_json::to_vec(&specification).unwrap()).unwrap();
        fixture::record(&input, &output).unwrap();
        assert!(
            std::fs::read_to_string(output)
                .unwrap()
                .contains("\"complete\"")
        );
        let encoded = std::fs::read_to_string(root.join(format!("{index}.jsonl"))).unwrap();
        let schema = study::schema_str(&encoded).unwrap();
        let analysis = fixture::analyze_str(&schema, &encoded, &[]).unwrap();
        assert_eq!(analysis.estimates.len(), 1);
    }
    std::fs::remove_dir_all(root).unwrap();
}

fn example_header() -> study::Header<ExampleSpec, u64> {
    let schedule = [0, 1]
        .map(|order| study::Scheduled {
            unit: study::Unit { block: 0, order },
            case: order,
        })
        .into();
    study::Header::new::<ExampleSchema>(
        study::Context::default(),
        ExampleSpec,
        vec![11, 22],
        study::Run {
            schedule,
            ..study::Run::default()
        },
    )
}

fn example_artifact(path: &std::path::Path) -> String {
    let mut recorder = study::Recorder::<ExampleSchema>::create(path, example_header()).unwrap();
    for (order, measure) in [11, 22].into_iter().enumerate() {
        recorder
            .observe(study::Observation {
                unit: study::Unit {
                    block: 0,
                    order: order as u32,
                },
                case: order as u32,
                measure,
            })
            .unwrap();
    }
    recorder.complete().unwrap()
}

#[test]
fn typed_study_round_trips_and_schema_selects_the_decoder() {
    let root = std::env::temp_dir().join(format!("evering-typed-study-{}", fastrand::u64(..)));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("evidence.jsonl");
    let digest = example_artifact(&path);
    let loaded = study::load::<ExampleSchema>(&path).unwrap();

    assert_eq!(loaded.header, example_header());
    assert_eq!(loaded.observations.len(), 2);
    assert_eq!(loaded.complete.content, digest);
    assert_eq!(
        study::load::<ForeignSchema>(&path).err(),
        Some(study::Error::Schema)
    );
    let mut another_run = example_header();
    another_run.run.seed = 99;
    assert_eq!(
        study::study_id::<ExampleSchema>(&loaded.header),
        study::study_id::<ExampleSchema>(&another_run)
    );
    another_run.context.source.revision = "different source".into();
    assert_ne!(
        study::study_id::<ExampleSchema>(&loaded.header),
        study::study_id::<ExampleSchema>(&another_run)
    );
    assert_ne!(
        study::case_id::<ExampleSchema>(&11),
        study::case_id::<ExampleSchema>(&22)
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn typed_study_rejects_every_unsealed_prefix_and_abort() {
    let root = std::env::temp_dir().join(format!("evering-typed-cuts-{}", fastrand::u64(..)));
    std::fs::create_dir(&root).unwrap();
    let complete = root.join("complete.jsonl");
    example_artifact(&complete);
    let bytes = std::fs::read(&complete).unwrap();
    for cut in 0..bytes.len() {
        let path = root.join(format!("cut-{cut}"));
        std::fs::write(&path, &bytes[..cut]).unwrap();
        assert!(
            study::load::<ExampleSchema>(&path).is_err(),
            "accepted cut {cut}"
        );
    }
    let aborted = root.join("aborted.jsonl");
    let recorder = study::Recorder::<ExampleSchema>::create(&aborted, example_header()).unwrap();
    recorder.abort(None, "stopped\nnow").unwrap();
    assert_eq!(
        study::load::<ExampleSchema>(&aborted).err(),
        Some(study::Error::Aborted)
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn typed_study_rejects_corruption_duplicate_rows_and_post_terminal_data() {
    let root = std::env::temp_dir().join(format!("evering-typed-corrupt-{}", fastrand::u64(..)));
    std::fs::create_dir(&root).unwrap();
    let complete = root.join("complete.jsonl");
    example_artifact(&complete);
    let text = std::fs::read_to_string(&complete).unwrap();
    let lines: Vec<_> = text.lines().collect();

    let corrupt = root.join("corrupt.jsonl");
    std::fs::write(
        &corrupt,
        text.replacen("\"measure\":11", "\"measure\":12", 1),
    )
    .unwrap();
    assert!(study::load::<ExampleSchema>(&corrupt).is_err());

    let revision = root.join("revision.jsonl");
    std::fs::write(
        &revision,
        text.replacen("\"revision\":1", "\"revision\":2", 1),
    )
    .unwrap();
    assert_eq!(
        study::load::<ExampleSchema>(&revision).err(),
        Some(study::Error::Schema)
    );

    let duplicate = root.join("duplicate.jsonl");
    std::fs::write(
        &duplicate,
        format!("{}\n{}\n{}\n{}\n", lines[0], lines[1], lines[1], lines[3]),
    )
    .unwrap();
    assert!(study::load::<ExampleSchema>(&duplicate).is_err());

    let trailing = root.join("trailing.jsonl");
    std::fs::write(&trailing, format!("{text}{}\n", lines[1])).unwrap();
    assert_eq!(
        study::load::<ExampleSchema>(&trailing).err(),
        Some(study::Error::Syntax)
    );

    let aborted = root.join("aborted.jsonl");
    study::Recorder::<ExampleSchema>::create(&aborted, example_header())
        .unwrap()
        .abort(None, "stopped")
        .unwrap();
    let forged = root.join("forged-complete.jsonl");
    std::fs::write(
        &forged,
        std::fs::read_to_string(aborted)
            .unwrap()
            .replace("\"kind\":\"abort\"", "\"kind\":\"complete\""),
    )
    .unwrap();
    assert!(study::load::<ExampleSchema>(&forged).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn typed_study_rejects_reorder_invalid_measure_and_overwrite() {
    let root = std::env::temp_dir().join(format!("evering-typed-order-{}", fastrand::u64(..)));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("evidence.jsonl");
    let mut recorder = study::Recorder::<ExampleSchema>::create(&path, example_header()).unwrap();
    assert_eq!(
        recorder.observe(study::Observation {
            unit: study::Unit { block: 0, order: 1 },
            case: 1,
            measure: 22,
        }),
        Err(study::Error::Schedule)
    );
    assert_eq!(
        recorder.observe(study::Observation {
            unit: study::Unit { block: 0, order: 0 },
            case: 0,
            measure: 99,
        }),
        Err(study::Error::Invalid)
    );
    assert!(study::Recorder::<ExampleSchema>::create(&path, example_header()).is_err());
    recorder.abort(None, "test complete").unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn path_counts_preserve_frequency_cause_and_overflow() {
    let mut paths = drive::PathCounts::default();
    paths.send_full().unwrap();
    paths.send_full().unwrap();
    paths.send_busy().unwrap();
    paths.recv_empty().unwrap();

    assert_eq!(
        (
            paths.send_full,
            paths.send_busy,
            paths.recv_empty,
            paths.send_attempts
        ),
        (2, 1, 1, 0)
    );
    paths.send_full = u64::MAX;
    assert_eq!(paths.send_full(), Err(drive::CountOverflow));
}

#[test]
fn geometry_replay_keeps_causal_metrics_separate() {
    use geometry::{Event::*, Geometry, Rejection, Trace};

    let geometry = Geometry::from_classes(256, &[(64, 1), (128, 1)]).unwrap();
    let trace = Trace::synthetic([
        Reserve {
            id: 1,
            owner: 7,
            bytes: 64,
        },
        Reserve {
            id: 2,
            owner: 7,
            bytes: 64,
        },
        Reserve {
            id: 3,
            owner: 7,
            bytes: 64,
        },
        Release { id: 1 },
        Transfer { id: 2, to: 9 },
        Crash { owner: 9 },
        Reap { owner: 9 },
    ]);
    let first = geometry.replay(&trace);

    assert_eq!(first, geometry.replay(&trace));
    assert_eq!((first.accepted, first.rejected), (2, 1));
    assert_eq!(first.rejections, [(2, Rejection::Unavailable)]);
    assert_eq!((first.requested_bytes, first.peak_live_bytes), (192, 128));
    assert_eq!(
        (first.peak_physical_bytes, first.peak_fragmentation),
        (192, 64)
    );
    assert_eq!(
        (first.spills, first.recovery_work, first.reclaimed_bytes),
        (1, 2, 128)
    );
}

#[test]
fn replacement_geometry_repairs_static_history_without_a_buddy_model() {
    use geometry::{Event::*, Geometry, Outcome, Rejection, Trace};

    let trace = Trace::synthetic([
        Reserve {
            id: 1,
            owner: 1,
            bytes: 64,
        },
        Reserve {
            id: 2,
            owner: 1,
            bytes: 64,
        },
        Reserve {
            id: 3,
            owner: 1,
            bytes: 64,
        },
        Reserve {
            id: 4,
            owner: 1,
            bytes: 64,
        },
        Release { id: 1 },
        Release { id: 2 },
        Release { id: 3 },
        Reserve {
            id: 5,
            owner: 1,
            bytes: 256,
        },
    ]);
    let original = Geometry::from_classes(581, &[(64, 1), (128, 2), (256, 1)]).unwrap();
    let comparison = original.compare_compacting(&trace);
    let divergence = comparison.first_divergence.unwrap();
    assert_eq!(divergence.event, 7);
    assert_eq!(
        divergence.static_model,
        Outcome::Rejected(Rejection::Unavailable)
    );
    assert_eq!(divergence.compact_bound, Outcome::Reserved);
    assert_eq!(comparison.static_model.largest[6], 128);

    let replacement = Geometry::from_classes(581, &[(64, 4), (256, 1)]).unwrap();
    assert_eq!(replacement.replay(&trace).rejections, []);
}

#[test]
fn balanced_geometry_is_integer_deterministic_and_extent_bounded() {
    let first = geometry::Geometry::balanced(4 * 1024 * 1024, 64, 64 * 1024).unwrap();
    assert_eq!(
        first,
        geometry::Geometry::balanced(4 * 1024 * 1024, 64, 64 * 1024).unwrap()
    );
    assert!(geometry::Geometry::balanced(63, 64, 64).is_err());
    assert!(geometry::Geometry::balanced(u64::MAX, 64, u64::MAX).is_err());
}

#[test]
fn projected_benchmark_conditions_never_authorize_buddy() {
    let evidence = system_evidence("focused", 1.0);
    let report = geometry::evidence_report(core::slice::from_ref(&evidence)).unwrap();
    assert!(report.starts_with("geometry-v1\nsource\t"));
    assert!(report.lines().nth(2).unwrap().ends_with("\tfalse"));
    assert_eq!(
        report,
        geometry::evidence_report(core::slice::from_ref(&evidence)).unwrap()
    );
}

#[derive(Default)]
struct TraceEndpoint {
    staged: Option<u64>,
    staged_digest: u64,
    payloads: Vec<Vec<u8>>,
    replies: std::collections::VecDeque<(u64, u64, usize)>,
    trace: Vec<(char, u64)>,
    corrupt: bool,
    fail_before_send: bool,
    fail_after_send: bool,
    fail_after_recv: bool,
    delay_ready: bool,
    fail_abort: bool,
    aborted: bool,
    quiet: bool,
}

#[test]
fn family_identity_is_explicit_unique_and_closed() {
    let core = family::find("core-ipc").unwrap();
    assert_eq!((core.key, core.revision), ("core-ipc", 3));
    assert_eq!(
        core.mode("screening"),
        Some((std::time::Duration::from_secs(180), 3))
    );
    assert_eq!(core.mode("unknown"), None);
    assert!(family::find("unknown").is_none());
}

#[cfg(unix)]
#[test]
fn local_family_owns_its_exact_two_arm_matrix() {
    let local = family::find("local-ipc-unix").unwrap();
    assert_eq!((local.key, local.revision), ("local-ipc-unix", 3));
    assert_eq!(local.baseline.key, "uds/readiness");
    assert_eq!(
        local.mode("pilot"),
        Some((std::time::Duration::from_secs(45), 1))
    );
    assert_eq!(
        local.mode("screening"),
        Some((std::time::Duration::from_secs(60), 3))
    );
    assert_eq!(
        local.mode("focused"),
        Some((std::time::Duration::from_secs(90), 15))
    );
    for mode in ["screening", "focused"] {
        let members = (local.members)(mode).unwrap();
        assert_eq!(members.len(), 6);
        for payload in [0, 1024, 64 * 1024] {
            let pair: Vec<_> = members
                .iter()
                .filter(|(cell, _)| cell.payload == payload)
                .collect();
            assert_eq!(pair.len(), 2);
            assert!(
                pair.iter()
                    .all(|(cell, _)| (cell.capacity, cell.in_flight) == (8, 8))
            );
            assert!(pair.iter().all(|(cell, _)| cell.memory == model::MEMORY));
        }
    }
}

#[cfg(unix)]
#[test]
fn registered_evering_cells_fit_the_frozen_pool() {
    use ::evering::{
        Session,
        mapping::{Access, Request},
        os::unix::UnixFd,
    };

    let source = UnixFd::memfd("evering-study-pool", model::MEMORY as usize, false).unwrap();
    let session = Session::create(
        source.borrow(),
        Request::new(model::MEMORY as usize, Access::READ | Access::WRITE),
        evering::REGION,
    )
    .unwrap();
    let pool = session
        .create_pool(evering::POOL_EXTENT, Some(evering::pool_range()))
        .unwrap();
    assert_eq!(pool.range(), evering::pool_range());

    for (condition, _) in (family::CORE.members)("screening")
        .unwrap()
        .into_iter()
        .chain((family::LOCAL.members)("screening").unwrap())
        .filter(|(_, arm)| arm.key.starts_with("evering/"))
    {
        let bytes = vec![0_u8; condition.payload as usize];
        let live: Vec<_> = (0..condition.capacity.min(condition.in_flight))
            .map(|_| pool.as_ref().copy(&bytes).unwrap())
            .collect();
        assert_eq!(
            live.len() as u64,
            condition.capacity.min(condition.in_flight)
        );
    }
}

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
fn pilot_calibration_freezes_from_one_measurable_ramp_without_a_probe() {
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
fn pilot_calibration_rejects_a_zero_duration_observation() {
    let cell = cell(&family::BUSY);
    assert!(pilot::calibrate(cell, 8, |_| Ok(0)).is_err());
}

#[test]
fn pilot_calibration_bounds_attempts_overflow_timeout_and_identity() {
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
fn pilot_admission_rejects_foreign_missing_duplicate_and_unverified_counts() {
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

impl drive::Endpoint for TraceEndpoint {
    type Error = &'static str;

    fn stage(&mut self, operation: u64, payload: &[u8]) -> Result<(), Self::Error> {
        if self.staged.replace(operation).is_some() {
            return Err("double stage");
        }
        self.staged_digest = model::digest(payload);
        self.payloads.push(payload.to_vec());
        Ok(())
    }

    fn try_send(
        &mut self,
        _: &mut drive::PathCounts,
    ) -> Result<drive::Step<(), Self::Error>, Self::Error> {
        if self.fail_before_send {
            return Err("pre-commit send failure");
        }
        let operation = self.staged.take().ok_or("send without stage")?;
        if !self.quiet {
            self.trace.push(('s', operation));
        }
        self.replies.push_back((
            operation,
            self.staged_digest,
            self.payloads.last().map_or(0, Vec::len),
        ));
        let _advisory_signal_failed = self.fail_after_send;
        Ok(drive::Step::Committed(Ok(())))
    }

    fn try_recv(
        &mut self,
        _: &mut drive::PathCounts,
        expected: drive::Expected,
    ) -> Result<drive::Step<bool, Self::Error>, Self::Error> {
        let Some((operation, digest, payload_len)) = self.replies.pop_front() else {
            return Ok(drive::Step::Pending);
        };
        if self.delay_ready && operation == u64::MAX {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !self.quiet {
            self.trace.push(('r', operation));
        }
        if self.fail_after_recv {
            return Ok(drive::Step::Committed(Err(
                "post-commit reconstruction failure",
            )));
        }
        let digest = digest ^ u64::from(self.corrupt);
        Ok(drive::Step::Committed(Ok(expected.matches_digest(
            operation,
            digest,
            payload_len,
        ))))
    }

    fn wait(
        &mut self,
        _: drive::Interest,
        _: drive::Deadline,
        _: &mut drive::PathCounts,
    ) -> Result<(), Self::Error> {
        Err("unexpected wait")
    }

    fn abort(&mut self) -> Result<(), Self::Error> {
        self.aborted = true;
        self.staged = None;
        if self.fail_abort {
            Err("cleanup failure")
        } else {
            Ok(())
        }
    }
}

fn trace_transfer(
    endpoint: &mut TraceEndpoint,
    count: u64,
    window: u64,
) -> Result<drive::Counts, drive::Error<&'static str>> {
    trace_transfer_with_paths(endpoint, count, window).map(|(counts, _)| counts)
}

fn trace_transfer_with_paths(
    endpoint: &mut TraceEndpoint,
    count: u64,
    window: u64,
) -> Result<(drive::Counts, drive::PathCounts), drive::Error<&'static str>> {
    let mut paths = drive::PathCounts::default();
    drive::transfer(
        endpoint,
        drive::Work {
            start: 0,
            count,
            window,
            payload: 1,
            seed: 7,
        },
        drive::Deadline::after(std::time::Instant::now(), std::time::Duration::from_secs(1))
            .unwrap(),
        &mut paths,
    )
    .map(|counts| (counts, paths))
}

#[test]
fn shared_driver_uses_a_sliding_window_and_shared_validation() {
    let mut endpoint = TraceEndpoint::default();
    let (counts, paths) = trace_transfer_with_paths(&mut endpoint, 4, 2).unwrap();
    assert_eq!(
        (counts.accepted, counts.completed, counts.validated),
        (4, 4, 4)
    );
    assert_eq!((paths.send_attempts, paths.recv_attempts), (4, 4));
    assert_eq!(
        endpoint.trace,
        [
            ('s', 0),
            ('s', 1),
            ('r', 0),
            ('s', 2),
            ('r', 1),
            ('s', 3),
            ('r', 2),
            ('r', 3),
        ]
    );
    assert!(endpoint.payloads.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn shared_driver_aborts_and_retains_counts_after_invalid_response() {
    let mut endpoint = TraceEndpoint {
        corrupt: true,
        ..TraceEndpoint::default()
    };
    let error = trace_transfer(&mut endpoint, 2, 1).unwrap_err();
    assert_eq!(error.kind, drive::Kind::InvalidResponse(0));
    assert_eq!(
        (
            error.counts.accepted,
            error.counts.completed,
            error.counts.validated
        ),
        (1, 1, 0)
    );
    assert!(endpoint.aborted);
}

#[test]
fn shared_driver_ignores_advisory_failure_after_send_commit() {
    let mut endpoint = TraceEndpoint {
        fail_after_send: true,
        ..TraceEndpoint::default()
    };
    let counts = trace_transfer(&mut endpoint, 1, 1).unwrap();
    assert_eq!(
        counts,
        drive::Counts {
            accepted: 1,
            completed: 1,
            validated: 1,
        }
    );
    assert!(!endpoint.aborted);
}

#[test]
fn shared_driver_counts_only_committed_work_before_errors() {
    let mut before = TraceEndpoint {
        fail_before_send: true,
        fail_abort: true,
        ..TraceEndpoint::default()
    };
    let error = trace_transfer(&mut before, 1, 1).unwrap_err();
    assert_eq!(error.kind, drive::Kind::Endpoint("pre-commit send failure"));
    assert_eq!(error.counts, drive::Counts::default());
    assert!(before.aborted);

    let mut after = TraceEndpoint {
        fail_after_recv: true,
        ..TraceEndpoint::default()
    };
    let error = trace_transfer(&mut after, 1, 1).unwrap_err();
    assert_eq!(
        error.kind,
        drive::Kind::Endpoint("post-commit reconstruction failure")
    );
    assert_eq!(
        error.counts,
        drive::Counts {
            accepted: 1,
            completed: 1,
            validated: 0,
        }
    );
    assert!(after.aborted);
}

#[cfg(feature = "process")]
#[test]
fn evering_bootstrap_binds_region_channel_and_extent() {
    use ::evering::{ChannelId as Id, Port};

    let id = Id::new(evering::REGION, 2, 3, 4, 5);
    let port = Port::from_parts(id, 1, 8).unwrap();
    let pool = ::evering::PoolId::new(evering::REGION, 5, 6, 7);
    let encoded = evering::bootstrap(&port, pool, 4096).unwrap();
    let (decoded, decoded_pool, decoded_extent) = evering::parse(encoded.as_ref()).unwrap();
    assert_eq!(decoded.id(), id);
    assert_eq!((decoded.role(), decoded.generation()), (1, 8));
    assert_eq!((decoded_pool, decoded_extent), (pool, 4096));
    let mut invalid = encoded.into_boxed();
    invalid[0] ^= 1;
    assert!(evering::parse(&invalid).is_err());
    assert!(
        evering::parse(
            evering::bootstrap(
                &Port::from_parts(Id::new(evering::REGION, 2, 3, 4, 0), 1, 8).unwrap(),
                pool,
                4096,
            )
            .unwrap()
            .as_ref()
        )
        .is_err()
    );
    assert!(evering::parse(evering::bootstrap(&port, pool, 0).unwrap().as_ref()).is_err());
}

#[test]
fn delivery_digest_reads_the_complete_payload() {
    let payload = model::payload(7, 11, 4096);
    let digest = model::digest(&payload);
    let mut changed = payload.clone();
    changed[2047] ^= 1;
    assert_ne!(digest, model::digest(&changed));
}

#[test]
fn framed_stream_returns_only_the_delivery_digest() {
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
    assert_eq!(response, model::digest(&payload).to_le_bytes());
    client.shutdown(Shutdown::Write).unwrap();
    worker.join().unwrap();
}

#[cfg(unix)]
#[test]
fn framing_is_transport_independent() {
    use std::{
        io::{Read, Write},
        net::Shutdown,
        os::unix::net::UnixStream,
    };

    let (mut client, server) = UnixStream::pair().unwrap();
    let worker = std::thread::spawn(move || stream::serve(server).unwrap());
    client.write_all(&7_u64.to_le_bytes()).unwrap();
    client.write_all(&3_u32.to_le_bytes()).unwrap();
    client.write_all(&[1, 2, 3]).unwrap();
    let mut response = [0; 20];
    client.read_exact(&mut response).unwrap();
    assert_eq!(&response[..12], &[7, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0, 0]);
    assert_eq!(&response[12..], &model::digest(&[1, 2, 3]).to_le_bytes());
    client.shutdown(Shutdown::Write).unwrap();
    worker.join().unwrap();
}

#[test]
fn framed_stream_ready_is_a_validated_round_trip() {
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::time::Duration;

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || stream::serve(listener.accept().unwrap().0).unwrap());
    let mut client = TcpStream::connect(address).unwrap();
    client
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    client.write_all(&u64::MAX.to_le_bytes()).unwrap();
    client.write_all(&0_u32.to_le_bytes()).unwrap();
    let mut response = [0; 12];
    client.read_exact(&mut response).unwrap();
    assert_eq!(
        u64::from_le_bytes(response[..8].try_into().unwrap()),
        u64::MAX
    );
    assert_eq!(u32::from_le_bytes(response[8..].try_into().unwrap()), 0);
    client.shutdown(Shutdown::Write).unwrap();
    worker.join().unwrap();
}

#[test]
fn readiness_stream_progresses_with_constrained_duplex_buffers() {
    use drive::Endpoint as _;
    use std::net::{TcpListener, TcpStream};
    use std::time::{Duration, Instant};

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        let stream = listener.accept().unwrap().0;
        let socket = socket2::SockRef::from(&stream);
        socket.set_send_buffer_size(4096).unwrap();
        socket.set_recv_buffer_size(4096).unwrap();
        stream::serve(stream).unwrap();
    });
    let client = TcpStream::connect(address).unwrap();
    let socket = socket2::SockRef::from(&client);
    socket.set_send_buffer_size(4096).unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    let mut client = stream::Client::<tokio::net::TcpStream>::new(client).unwrap();
    let deadline = drive::Deadline::after(Instant::now(), Duration::from_secs(5)).unwrap();
    drive::transfer(
        &mut client,
        drive::Work {
            start: u64::MAX,
            count: 1,
            window: 1,
            payload: 0,
            seed: 7,
        },
        deadline,
        &mut drive::PathCounts::default(),
    )
    .unwrap();
    let mut path = drive::PathCounts::default();
    let counts = drive::transfer(
        &mut client,
        drive::Work {
            start: 0,
            count: 32,
            window: 8,
            payload: 64 * 1024,
            seed: 7,
        },
        deadline,
        &mut path,
    )
    .unwrap();
    assert_eq!(
        (counts.accepted, counts.completed, counts.validated),
        (32, 32, 32)
    );
    client.abort().unwrap();
    worker.join().unwrap();
}

fn condition() -> model::Condition {
    model::Condition {
        payload: 64,
        capacity: 8,
        in_flight: 3,
        memory: model::MEMORY,
    }
}

fn cell(arm: &'static family::Arm) -> model::Cell {
    condition().cell(arm)
}

fn contrast(arm: &'static family::Arm) -> (model::Condition, &'static family::Arm) {
    (condition(), arm)
}

#[test]
fn schedule_shares_one_baseline_across_evering_policies() {
    let members = [
        contrast(&family::BUSY),
        contrast(&family::NOTIFIED),
        (condition(), &family::STREAM),
    ];
    let scheduled = model::schedule(&members, 3, 7);
    for block in 0..3 {
        let arms: Vec<_> = scheduled
            .iter()
            .filter(|trial| trial.block == block)
            .map(|trial| trial.arm.key)
            .collect();
        assert_eq!(arms.len(), 3);
        assert_eq!(
            arms.iter()
                .filter(|arm| **arm == family::STREAM.key)
                .count(),
            1
        );
        assert!(arms.contains(&family::BUSY.key));
        assert!(arms.contains(&family::NOTIFIED.key));
    }
}

#[test]
fn registered_families_have_exact_bounded_membership() {
    let screening = (family::CORE.members)("screening").unwrap();
    let focused = (family::CORE.members)("focused").unwrap();
    assert_eq!((screening.len(), focused.len()), (36, 10));
    assert_eq!(model::schedule(&screening, 3, 7).len(), 108);
    assert_eq!(model::schedule(&focused, 15, 7).len(), 150);
    assert!(
        screening
            .iter()
            .chain(&focused)
            .all(|(cell, _)| cell.memory == model::MEMORY)
    );
}

#[test]
fn schedule_randomizes_arm_order_without_changing_membership() {
    let members = [contrast(&family::ADAPTIVE), (condition(), &family::STREAM)];
    let first = model::schedule(&members, 8, 7);
    let second = model::schedule(&members, 8, 8);
    assert_eq!(first.len(), second.len());
    assert_ne!(
        first.iter().map(|entry| entry.arm.key).collect::<Vec<_>>(),
        second.iter().map(|entry| entry.arm.key).collect::<Vec<_>>()
    );
}

#[test]
fn stream_window_obeys_capacity_and_in_flight() {
    assert_eq!(model::window(100, 1, 64), 1);
    assert_eq!(model::window(100, 8, 64), 8);
    assert_eq!(model::window(3, 8, 64), 3);
}

fn system_header(mode: &str) -> study::Header<system::Specification, system::Case> {
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

fn system_measure(case: &system::Case, elapsed_ns: u64) -> system::Measure {
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
    }
}

fn record_system_evidence(
    path: &std::path::Path,
    mode: &str,
    candidate_ratio: f64,
    revision: &str,
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
        recorder
            .observe(study::Observation {
                unit: expected.unit,
                case: expected.case,
                measure: system_measure(case, elapsed),
            })
            .unwrap();
    }
    recorder.complete().unwrap();
}

fn system_evidence_at(mode: &str, candidate_ratio: f64, revision: &str) -> system::Evidence {
    let path = std::env::temp_dir().join(format!("evering-system-{}", fastrand::u64(..)));
    record_system_evidence(&path, mode, candidate_ratio, revision);
    let evidence = system::load(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    evidence
}

fn system_evidence(mode: &str, candidate_ratio: f64) -> system::Evidence {
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

#[test]
fn screening_has_estimates_but_no_decision_authority() {
    let evidence = system_evidence("screening", 1.1);
    let analysis = analysis::analyze(&evidence).unwrap();
    assert!(!analysis.estimates.is_empty());
    assert_eq!(analysis.authority, analysis::Authority::Descriptive);
}

#[test]
fn smoke_is_admitted_only_as_descriptive_evidence() {
    let evidence = system_evidence("smoke", 1.2);
    let analysis = analysis::analyze(&evidence).unwrap();
    assert!(!analysis.estimates.is_empty());
    assert_eq!(analysis.authority, analysis::Authority::Descriptive);
}

#[test]
fn report_is_input_order_invariant_and_binds_exact_source_bytes() {
    let root = std::env::temp_dir().join(format!("evering-report-{}", fastrand::u64(..)));
    std::fs::create_dir(&root).unwrap();
    let first = root.join("first.jsonl");
    let second = root.join("second.jsonl");
    record_system_evidence(&first, "focused", 1.2, "first");
    record_system_evidence(&second, "focused", 1.1, "second");
    let paths = [first, second].map(|path| path.to_string_lossy().into_owned());

    let forward = analysis::command(&paths).unwrap();
    let reverse = analysis::command(&[paths[1].clone(), paths[0].clone()]).unwrap();
    assert_eq!(forward, reverse);
    for path in &paths {
        let digest = blake3::hash(&std::fs::read(path).unwrap()).to_hex();
        assert!(forward.contains(digest.as_str()));
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(feature = "plot")]
fn publication_is_complete_digest_bound_and_non_overwriting() {
    let root = std::env::temp_dir().join(format!("evering-publish-{}", fastrand::u64(..)));
    std::fs::create_dir(&root).unwrap();
    let system = root.join("system.jsonl");
    record_system_evidence(&system, "focused", 1.2, "publish");
    let specification = root.join("mechanism.json");
    let mechanism = root.join("mechanism.jsonl");
    std::fs::write(
        &specification,
        serde_json::to_vec(&serde_json::json!({
            "fixture": fixture::KEYS[0],
            "targets": ["a".repeat(64)],
            "parameters": { "extent": 1 << 20, "storage": 1 << 16, "capacity": 8 },
            "cases": [{ "payload": 0 }],
            "policy": { "min_ns": 1, "max_ns": u64::MAX, "pairs": 1, "calibration_attempts": 1 },
            "seed": 7,
            "budget_ms": 5_000,
            "alpha": 0.05,
            "delta_ns": 1.0,
            "system_delta": 0.05,
        }))
        .unwrap(),
    )
    .unwrap();
    fixture::record(&specification, &mechanism).unwrap();
    let sources = [&system, &mechanism].map(|path| path.to_string_lossy().into_owned());
    let output = root.join("published");

    let files = super::plot::command(&output, &sources).unwrap();
    assert_eq!(files.len(), 4);
    assert!(files.iter().any(|path| path.ends_with("report.md")));
    assert!(files.iter().any(|path| path.ends_with("DIGESTS.blake3")));
    let svgs = files
        .iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "svg"))
        .map(|path| std::fs::read_to_string(path).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(svgs.len(), 2);
    assert!(svgs.iter().any(|svg| svg.contains("practical band")));
    assert!(svgs.iter().any(|svg| svg.contains("paired difference")));
    let manifest = std::fs::read_to_string(output.join("DIGESTS.blake3")).unwrap();
    for path in files
        .iter()
        .filter(|path| !path.ends_with("DIGESTS.blake3"))
    {
        let digest = blake3::hash(&std::fs::read(path).unwrap()).to_hex();
        assert!(manifest.contains(&format!(
            "{}  {}",
            digest,
            path.file_name().unwrap().to_string_lossy()
        )));
    }
    let repeated = super::plot::command(&root.join("repeated"), &sources).unwrap();
    assert!(files.iter().zip(&repeated).all(|(left, right)| {
        left.file_name() == right.file_name()
            && std::fs::read(left).unwrap() == std::fs::read(right).unwrap()
    }));
    let before = files
        .iter()
        .map(|path| (path.clone(), std::fs::read(path).unwrap()))
        .collect::<Vec<_>>();
    assert!(super::plot::command(&output, &sources).is_err());
    assert!(
        before
            .iter()
            .all(|(path, bytes)| std::fs::read(path).unwrap() == *bytes)
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(feature = "plot")]
fn publication_rejects_source_over_the_retention_limit_before_decode() {
    let root = std::env::temp_dir().join(format!("evering-retention-{}", fastrand::u64(..)));
    std::fs::create_dir(&root).unwrap();
    let oversized = root.join("oversized.jsonl");
    std::fs::File::create(&oversized)
        .unwrap()
        .set_len(5 * 1024 * 1024 + 1)
        .unwrap();
    let error = super::plot::command(
        &root.join("published"),
        &[oversized.to_string_lossy().into_owned()],
    )
    .unwrap_err();
    assert!(error.contains("retention limit"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg_attr(debug_assertions, ignore = "optimized evidence gate")]
fn path_counter_overhead_stays_inside_one_percent() {
    use std::time::{Duration, Instant};

    fn timed(instrumented: bool) -> f64 {
        let mut endpoint = TraceEndpoint {
            replies: std::collections::VecDeque::with_capacity(8),
            quiet: true,
            ..TraceEndpoint::default()
        };
        let work = drive::Work {
            start: 0,
            count: 100_000,
            window: 8,
            payload: 0,
            seed: 7,
        };
        let began = Instant::now();
        let deadline = drive::Deadline::after(began, Duration::from_secs(10)).unwrap();
        let counts = if instrumented {
            drive::transfer(
                &mut endpoint,
                work,
                deadline,
                &mut drive::PathCounts::default(),
            )
        } else {
            drive::transfer_control(&mut endpoint, work, deadline)
        }
        .unwrap();
        assert_eq!(counts.validated, work.count);
        began.elapsed().as_secs_f64()
    }

    for _ in 0..3 {
        std::hint::black_box((timed(false), timed(true)));
    }
    let ratios = (0..101)
        .map(|pair| {
            let (control, instrumented) = if pair % 2 == 0 {
                let first = timed(false);
                let inner = timed(true) * timed(true);
                (first * timed(false), inner)
            } else {
                let first = timed(true);
                let inner = timed(false) * timed(false);
                (inner, first * timed(true))
            };
            (instrumented / control).sqrt()
        })
        .collect::<Vec<_>>();
    let mut state = 7_u64;
    let mut interval = (0..10_000)
        .map(|_| {
            let mut sample = (0..ratios.len())
                .map(|_| {
                    state = state
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1);
                    ratios[state as usize % ratios.len()]
                })
                .collect::<Vec<_>>();
            sample.sort_unstable_by(f64::total_cmp);
            sample[sample.len() / 2]
        })
        .collect::<Vec<_>>();
    interval.sort_unstable_by(f64::total_cmp);
    let low = interval[250];
    let high = interval[9_749];
    assert!(
        low >= 0.99 && high <= 1.01,
        "instrumentation interval [{:.4}, {:.4}]",
        low,
        high
    );
}

#[test]
fn warmup_consumption_cannot_reset_the_measured_deadline() {
    let began = std::time::Instant::now();
    let error = drive::measure(
        &mut TraceEndpoint {
            delay_ready: true,
            ..TraceEndpoint::default()
        },
        drive::Work {
            start: 0,
            count: 1,
            window: 1,
            payload: 1,
            seed: 7,
        },
        1,
        began,
        drive::Deadline::after(began, std::time::Duration::from_millis(2)).unwrap(),
    )
    .err()
    .unwrap();
    assert!(error.timed);
    assert_eq!(error.error.kind, drive::Kind::Deadline);
}
