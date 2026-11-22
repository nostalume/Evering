//! What a mechanism case has to look like: the spec, schema and fixture types a study
//! seals, and the registrations the frozen tables admit. The example types declared
//! here are the ones `record_round_trips` round-trips.

use crate::{fixture, mechanism, study, system};

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(super) struct ExampleSpec;

impl study::Identity for ExampleSpec {
    fn identity(&self, _: &mut blake3::Hasher) {}
}

pub(super) struct ExampleSchema;

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

pub(super) struct ForeignSchema;

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
