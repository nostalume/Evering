use std::{
    marker::PhantomData,
    num::NonZeroU64,
    path::Path,
    time::{Duration, Instant},
};

use serde::{Serialize, de::DeserializeOwned};

use super::study::{self, Header, Identity, Observation, Recorder, Scheduled, Schema, Unit};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum Order {
    GrossControl,
    ControlGross,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Body {
    Gross,
    Control,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Policy {
    pub min_ns: u64,
    pub max_ns: u64,
    pub pairs: u32,
    pub calibration_attempts: u32,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            min_ns: 10_000_000,
            max_ns: 50_000_000,
            pairs: 9,
            calibration_attempts: 8,
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Specification<P> {
    pub targets: Vec<String>,
    pub parameters: P,
    pub policy: Policy,
    pub alpha: f64,
    pub delta_ns: f64,
    pub system_delta: f64,
    pub orders: Vec<Order>,
}

impl<P: Identity> Identity for Specification<P> {
    fn identity(&self, hash: &mut blake3::Hasher) {
        for target in &self.targets {
            target.identity(hash);
        }
        self.parameters.identity(hash);
        self.policy.min_ns.identity(hash);
        self.policy.max_ns.identity(hash);
        self.policy.pairs.identity(hash);
        self.policy.calibration_attempts.identity(hash);
        hash.update(&self.alpha.to_bits().to_le_bytes());
        hash.update(&self.delta_ns.to_bits().to_le_bytes());
        hash.update(&self.system_delta.to_bits().to_le_bytes());
        for order in &self.orders {
            hash.update(&[*order as u8]);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Measure {
    pub order: Order,
    pub operations: u64,
    pub gross_ns: u64,
    pub control_ns: u64,
    pub before: String,
    pub after: String,
}

pub trait Fixture: Sized {
    type Parameters;
    type Case;

    fn prepare(parameters: &Self::Parameters, case: &Self::Case) -> Result<Self, String>;
    fn state(&self) -> Result<String, String>;
    fn limit(&self) -> NonZeroU64;
    fn setup(&mut self, operations: u64, body: Body) -> Result<(), String>;
    fn gross(&mut self, operations: u64) -> Result<(), String>;
    fn control(&mut self, operations: u64) -> Result<(), String>;
    fn reset(&mut self) -> Result<(), String>;
}

pub trait FixtureSchema: 'static {
    const KEY: &'static str;
    type Parameters: Clone + Identity + Serialize + DeserializeOwned;
    type Case: Clone + Eq + Identity + Serialize + DeserializeOwned;
    type Fixture: Fixture<Parameters = Self::Parameters, Case = Self::Case>;

    fn matches(
        parameters: &Self::Parameters,
        case: &Self::Case,
        target: &super::system::Case,
    ) -> bool;
}

pub struct Mechanism<F>(PhantomData<F>);

impl<F: FixtureSchema> Schema for Mechanism<F> {
    const NAME: &'static str = F::KEY;
    const REVISION: u32 = 1;
    type Specification = Specification<F::Parameters>;
    type Case = F::Case;
    type Measure = Measure;

    fn validate_header(header: &Header<Self::Specification, Self::Case>) -> Result<(), String> {
        let policy = &header.specification.policy;
        let target =
            |value: &str| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
        if header.specification.targets.is_empty()
            || header
                .specification
                .targets
                .iter()
                .any(|value| !target(value))
            || header.cases.is_empty()
            || header.specification.targets.len() != header.cases.len()
            || !header.specification.alpha.is_finite()
            || !(0.0..1.0).contains(&header.specification.alpha)
            || !header.specification.delta_ns.is_finite()
            || header.specification.delta_ns < 0.0
            || !header.specification.system_delta.is_finite()
            || header.specification.system_delta == 0.0
            || header.specification.system_delta.abs() >= 1.0
            || policy.min_ns == 0
            || policy.min_ns > policy.max_ns
            || policy.pairs == 0
            || policy.calibration_attempts == 0
            || header.run.budget_ms == 0
            || header.run.schedule.len() != header.cases.len() * policy.pairs as usize
            || header.specification.orders.len() != header.run.schedule.len()
        {
            return Err("invalid mechanism specification".into());
        }
        Ok(())
    }

    fn validate(
        header: &Header<Self::Specification, Self::Case>,
        observation: &Observation<Self::Measure>,
    ) -> Result<(), String> {
        let index = header
            .run
            .schedule
            .iter()
            .position(|entry| entry.unit == observation.unit)
            .ok_or("foreign mechanism unit")?;
        let measure = &observation.measure;
        if measure.operations == 0
            || measure.gross_ns == 0
            || measure.control_ns == 0
            || measure.before != measure.after
            || header.specification.orders[index] != measure.order
        {
            return Err("invalid mechanism measure".into());
        }
        Ok(())
    }
}

pub fn schedule(seed: u64, cases: usize, pairs: u32) -> (Vec<Scheduled>, Vec<Order>) {
    let mut state = seed;
    let starts_gross = study::random(&mut state) & 1 == 0;
    let mut schedule = Vec::with_capacity(cases * pairs as usize);
    let mut orders = Vec::with_capacity(schedule.capacity());
    for block in 0..pairs {
        let mut case = (0..cases as u32).collect::<Vec<_>>();
        for index in (1..case.len()).rev() {
            case.swap(index, study::random(&mut state) as usize % (index + 1));
        }
        for case in case {
            schedule.push(Scheduled {
                unit: Unit {
                    block,
                    order: schedule.len() as u32,
                },
                case,
            });
            orders.push(if block.is_multiple_of(2) == starts_gross {
                Order::GrossControl
            } else {
                Order::ControlGross
            });
        }
    }
    (schedule, orders)
}

fn elapsed(body: impl FnOnce() -> Result<(), String>) -> Result<u64, String> {
    let began = Instant::now();
    body()?;
    u64::try_from(began.elapsed().as_nanos().max(1)).map_err(|_| "batch time overflow".into())
}

fn calibrate<F: Fixture>(
    fixture: &mut F,
    policy: &Policy,
    deadline: Instant,
) -> Result<u64, String> {
    let initial = fixture.state()?;
    let target = policy.min_ns + (policy.max_ns - policy.min_ns) / 2;
    let mut operations = 1_u64;
    for _ in 0..policy.calibration_attempts {
        if Instant::now() >= deadline {
            return Err("mechanism budget exhausted during calibration".into());
        }
        fixture.setup(operations, Body::Gross)?;
        let measured = elapsed(|| fixture.gross(operations))?;
        fixture.reset()?;
        if fixture.state()? != initial {
            return Err("mechanism calibration did not reset".into());
        }
        if Instant::now() >= deadline {
            return Err("mechanism budget exhausted during calibration".into());
        }
        if (policy.min_ns..=policy.max_ns).contains(&measured) {
            return Ok(operations);
        }
        if measured > policy.max_ns {
            return Err("one mechanism operation exceeds the calibration window".into());
        }
        let next = operations
            .checked_mul(target)
            .and_then(|value| value.checked_div(measured))
            .unwrap_or(u64::MAX)
            .max(operations.saturating_add(1))
            .min(operations.saturating_mul(1024))
            .min(fixture.limit().get());
        if next == operations {
            return Err("mechanism geometry cannot reach the calibration window".into());
        }
        operations = next;
    }
    Err("mechanism calibration did not converge".into())
}

fn pair<F: Fixture>(fixture: &mut F, operations: u64, order: Order) -> Result<Measure, String> {
    let before = fixture.state()?;
    let mut run = |gross| {
        fixture.setup(operations, if gross { Body::Gross } else { Body::Control })?;
        let measured = if gross {
            elapsed(|| fixture.gross(operations))
        } else {
            elapsed(|| fixture.control(operations))
        }?;
        fixture.reset()?;
        Ok::<_, String>(measured)
    };
    let (gross_ns, control_ns) = match order {
        Order::GrossControl => (run(true)?, run(false)?),
        Order::ControlGross => {
            let control = run(false)?;
            (run(true)?, control)
        }
    };
    Ok(Measure {
        order,
        operations,
        gross_ns,
        control_ns,
        before,
        after: fixture.state()?,
    })
}

pub fn record<F: FixtureSchema>(
    path: &Path,
    header: Header<Specification<F::Parameters>, F::Case>,
) -> Result<String, String> {
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(header.run.budget_ms))
        .ok_or("mechanism budget overflow")?;
    let schedule = header.run.schedule.clone();
    let parameters = header.specification.parameters.clone();
    let policy = header.specification.policy.clone();
    let orders = header.specification.orders.clone();
    let cases = header.cases.clone();
    let mut calibrated = vec![None; header.cases.len()];
    let mut recorder =
        Recorder::<Mechanism<F>>::create(path, header).map_err(|error| format!("{error:?}"))?;
    for (index, scheduled) in schedule.into_iter().enumerate() {
        let result = (|| -> Result<Measure, String> {
            if Instant::now() >= deadline {
                return Err("mechanism budget exhausted".into());
            }
            let mut fixture = F::Fixture::prepare(&parameters, &cases[scheduled.case as usize])?;
            let operations = match calibrated[scheduled.case as usize] {
                Some(value) => value,
                None => {
                    let value = calibrate(&mut fixture, &policy, deadline)?;
                    calibrated[scheduled.case as usize] = Some(value);
                    value
                }
            };
            let measure = pair(&mut fixture, operations, orders[index])?;
            if Instant::now() >= deadline {
                return Err("mechanism budget exhausted".into());
            }
            Ok(measure)
        })();
        let measure = match result {
            Ok(measure) => measure,
            Err(error) => {
                recorder
                    .abort(Some(scheduled.unit), &error)
                    .map_err(|failure| format!("{failure:?}"))?;
                return Err(error);
            }
        };
        recorder
            .observe(Observation {
                unit: scheduled.unit,
                case: scheduled.case,
                measure,
            })
            .map_err(|error| format!("{error:?}"))?;
    }
    recorder.complete().map_err(|error| format!("{error:?}"))
}
