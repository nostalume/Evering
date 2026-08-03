use super::{
    model::{Cell, Scheduled},
    study::{
        self, Compiler, Context, Header, Host, Identity as StudyIdentity,
        Observation as EvidenceRow, Recorder, Run, Scheduled as StudyScheduled, Schema, Source,
        Unit,
    },
};

pub fn progress(kind: &str, done: usize, total: usize, cell: &Cell) -> String {
    format!(
        "{kind} {done}/{total}: {} payload={} capacity={} in-flight={}",
        cell.arm, cell.payload, cell.capacity, cell.in_flight
    )
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Identity {
    pub algorithm: u32,
    pub family: String,
    pub family_revision: u32,
    pub revision: String,
    pub dirty: bool,
    pub diff: String,
    pub target: String,
    pub os: String,
    pub arch: String,
    pub rustc: String,
    pub host: String,
    pub environment: String,
    pub command: String,
    pub started: String,
    pub seed: u64,
    pub warmup: u64,
    pub timeout_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Observation {
    pub count: u64,
    pub elapsed_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Row {
    pub cell: Cell,
    pub count: u64,
    pub observations: Vec<Observation>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Specification {
    algorithm: u32,
    family: String,
    family_revision: u32,
}

impl StudyIdentity for Specification {
    fn identity(&self, hash: &mut blake3::Hasher) {
        self.algorithm.identity(hash);
        self.family.identity(hash);
        self.family_revision.identity(hash);
    }
}

impl StudyIdentity for Cell {
    fn identity(&self, hash: &mut blake3::Hasher) {
        self.arm.identity(hash);
        self.payload.identity(hash);
        self.capacity.identity(hash);
        self.in_flight.identity(hash);
        self.memory.identity(hash);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Measure {
    count: u64,
    observations: Vec<Observation>,
}

pub struct Calibration;

pub type Evidence = study::Study<Calibration>;

impl Schema for Calibration {
    const NAME: &'static str = "calibration";
    const REVISION: u32 = 2;
    type Specification = Specification;
    type Case = Cell;
    type Measure = Measure;

    fn validate_header(header: &Header<Specification, Cell>) -> Result<(), String> {
        let mut unique = std::collections::HashSet::with_capacity(header.cases.len());
        if header.cases.iter().any(|cell| !unique.insert(cell)) {
            return Err("duplicate calibration case".into());
        }
        (header.run.schedule.len() == header.cases.len())
            .then_some(())
            .ok_or_else(|| "incomplete calibration schedule".into())
    }

    fn validate(
        header: &Header<Specification, Cell>,
        observation: &EvidenceRow<Measure>,
    ) -> Result<(), String> {
        let cell = header
            .cases
            .get(observation.case as usize)
            .ok_or("foreign calibration case")?;
        validate(
            &Row {
                cell: cell.clone(),
                count: observation.measure.count,
                observations: observation.measure.observations.clone(),
            },
            header.run.warmup,
        )
    }
}

fn header(identity: &Identity, cases: Vec<Cell>) -> Result<Header<Specification, Cell>, String> {
    let schedule = (0..cases.len())
        .map(|order| {
            let order = u32::try_from(order).map_err(|_| "too many calibration cases")?;
            Ok(StudyScheduled {
                unit: Unit { block: 0, order },
                case: order,
            })
        })
        .collect::<Result<_, &str>>()?;
    Ok(Header::new::<Calibration>(
        Context {
            source: Source {
                revision: identity.revision.clone(),
                dirty: identity.dirty,
                diff: identity.diff.clone(),
            },
            compiler: Compiler {
                target: identity.target.clone(),
                rustc: identity.rustc.clone(),
            },
            host: Host {
                os: identity.os.clone(),
                arch: identity.arch.clone(),
                description: identity.host.clone(),
                environment: identity.environment.clone(),
            },
        },
        Specification {
            algorithm: identity.algorithm,
            family: identity.family.clone(),
            family_revision: identity.family_revision,
        },
        cases,
        Run {
            seed: identity.seed,
            started: identity.started.clone(),
            command: identity.command.clone(),
            warmup: identity.warmup,
            budget_ms: identity.timeout_ms,
            schedule,
        },
    ))
}

pub fn identity(evidence: &Evidence) -> Identity {
    let header = &evidence.header;
    Identity {
        algorithm: header.specification.algorithm,
        family: header.specification.family.clone(),
        family_revision: header.specification.family_revision,
        revision: header.context.source.revision.clone(),
        dirty: header.context.source.dirty,
        diff: header.context.source.diff.clone(),
        target: header.context.compiler.target.clone(),
        os: header.context.host.os.clone(),
        arch: header.context.host.arch.clone(),
        rustc: header.context.compiler.rustc.clone(),
        host: header.context.host.description.clone(),
        environment: header.context.host.environment.clone(),
        command: header.run.command.clone(),
        started: header.run.started.clone(),
        seed: header.run.seed,
        warmup: header.run.warmup,
        timeout_ms: header.run.budget_ms,
    }
}

pub fn record(
    path: &std::path::Path,
    identity: Identity,
    cases: Vec<Cell>,
    rows: impl IntoIterator<Item = Result<Row, String>>,
) -> Result<String, String> {
    let expected = cases.len();
    let mut recorder = Recorder::<Calibration>::create(path, header(&identity, cases.clone())?)
        .map_err(|error| format!("{error:?}"))?;
    let mut recorded = 0;
    for (order, result) in rows.into_iter().enumerate() {
        let unit = Unit {
            block: 0,
            order: u32::try_from(order).map_err(|_| "too many calibration rows")?,
        };
        let row = match result {
            Ok(row) => row,
            Err(error) => {
                recorder
                    .abort(Some(unit), &error)
                    .map_err(|error| format!("{error:?}"))?;
                return Err(error);
            }
        };
        if cases.get(order) != Some(&row.cell) {
            recorder
                .abort(Some(unit), "calibration row does not match case")
                .map_err(|error| format!("{error:?}"))?;
            return Err("calibration row does not match case".into());
        }
        recorder
            .observe(EvidenceRow {
                unit,
                case: unit.order,
                measure: Measure {
                    count: row.count,
                    observations: row.observations,
                },
            })
            .map_err(|error| format!("{error:?}"))?;
        recorded += 1;
    }
    if expected == 0 || recorded != expected {
        recorder
            .abort(None, "incomplete calibration")
            .map_err(|error| format!("{error:?}"))?;
        return Err("incomplete calibration".into());
    }
    recorder.complete().map_err(|error| format!("{error:?}"))
}

pub fn load(path: &std::path::Path) -> Result<(Evidence, String), String> {
    let evidence = study::load::<Calibration>(path).map_err(|error| format!("{error:?}"))?;
    let digest = evidence.complete.content.clone();
    Ok((evidence, digest))
}

pub fn admit(
    evidence: &Evidence,
    identity: &Identity,
    scheduled: &[Scheduled],
) -> Result<Vec<u64>, String> {
    let actual = self::identity(evidence);
    let mut expected = identity.clone();
    expected.command.clone_from(&actual.command);
    expected.started.clone_from(&actual.started);
    if identity.algorithm != 4 || actual != expected {
        return Err("foreign pilot identity".into());
    }
    let expected: std::collections::HashSet<_> =
        scheduled.iter().map(|entry| entry.cell()).collect();
    let actual: std::collections::HashSet<_> = evidence.header.cases.iter().cloned().collect();
    if evidence.header.cases.len() != evidence.observations.len()
        || actual.len() != evidence.header.cases.len()
        || actual != expected
    {
        return Err("incomplete pilot manifest".into());
    }
    scheduled
        .iter()
        .map(|entry| {
            let cell = entry.cell();
            evidence
                .header
                .cases
                .iter()
                .position(|value| value == &cell)
                .and_then(|index| evidence.observations.get(index))
                .map(|row| row.measure.count)
                .ok_or_else(|| "missing pilot row".into())
        })
        .collect()
}

fn rounded(value: u64, window: u64) -> Result<u64, String> {
    value
        .checked_add(window - 1)
        .map(|value| value / window * window)
        .ok_or_else(|| "pilot count overflow".into())
}

fn scaled(count: u64, elapsed_ns: u64, target_ns: u64, window: u64) -> Result<u64, String> {
    let target = u128::from(count)
        .checked_mul(u128::from(target_ns))
        .ok_or("pilot scale overflow")?
        .div_ceil(u128::from(elapsed_ns));
    let target = u64::try_from(target).map_err(|_| "pilot count overflow")?;
    rounded(target, window)
        .and_then(|value| Ok(value.max(window.checked_mul(32).ok_or("pilot count overflow")?)))
}

fn validate(row: &Row, warmup: u64) -> Result<(), String> {
    let window = row.cell.capacity.min(row.cell.in_flight);
    let minimum = window.checked_mul(32).ok_or("pilot count overflow")?;
    if window == 0
        || row.count < minimum
        || !row.count.is_multiple_of(window)
        || row
            .count
            .checked_add(warmup)
            .is_none_or(|last| last == u64::MAX)
    {
        return Err("invalid pilot count".into());
    }
    if !(1..=8).contains(&row.observations.len()) {
        return Err("invalid pilot ramp".into());
    }
    let mut count = rounded(64_u64.max(minimum), window)?;
    for (index, value) in row.observations.iter().enumerate() {
        if value.count != count || value.elapsed_ns == 0 {
            return Err("invalid pilot ramp".into());
        }
        if index + 1 < row.observations.len() {
            if value.elapsed_ns >= 50_000_000 {
                return Err("continued measurable pilot ramp".into());
            }
            count = scaled(count, value.elapsed_ns, 50_000_000, window)?;
        } else if value.elapsed_ns < 50_000_000 {
            return Err("pilot ramp remained unmeasurable".into());
        }
    }
    let last = row.observations.last().unwrap();
    (row.count == scaled(last.count, last.elapsed_ns, 250_000_000, window)?)
        .then_some(())
        .ok_or_else(|| "invalid frozen pilot count".into())
}

pub fn calibrate(
    cell: Cell,
    warmup: u64,
    mut run: impl FnMut(u64) -> Result<u64, String>,
) -> Result<Row, String> {
    let window = cell.capacity.min(cell.in_flight);
    if window == 0 {
        return Err("zero pilot window".into());
    }
    let mut count = rounded(
        64_u64.max(window.checked_mul(32).ok_or("pilot count overflow")?),
        window,
    )?;
    let mut observations = Vec::new();
    for _ in 0..8 {
        let elapsed_ns = run(count)?;
        if elapsed_ns == 0 {
            return Err("zero pilot duration".into());
        }
        observations.push(Observation { count, elapsed_ns });
        if elapsed_ns >= 50_000_000 {
            count = scaled(count, elapsed_ns, 250_000_000, window)?;
            if count
                .checked_add(warmup)
                .is_some_and(|last| last < u64::MAX)
            {
                return Ok(Row {
                    cell,
                    count,
                    observations,
                });
            }
            return Err("pilot operation identity overflow".into());
        }
        count = scaled(count, elapsed_ns, 50_000_000, window)?;
    }
    Err("pilot never became measurable".into())
}
