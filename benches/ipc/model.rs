use std::collections::HashSet;

use super::drive::Path;

pub fn payload(seed: u64, operation: u64, len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| payload_byte(seed, operation, index))
        .collect()
}

pub fn valid_response(seed: u64, operation: u64, expected_len: usize, response: &[u8]) -> bool {
    response.len() == expected_len
        && response
            .iter()
            .enumerate()
            .all(|(index, byte)| *byte == payload_byte(seed, operation, index) ^ 0xa5)
}

fn payload_byte(seed: u64, operation: u64, index: usize) -> u8 {
    seed.wrapping_add(operation.rotate_left(17))
        .wrapping_add((index as u64).wrapping_mul(0x9e37_79b9))
        .to_le_bytes()[index & 7]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum Status {
    Ok,
    Unsupported,
    Invalid,
    SetupError,
    TimedError,
    DrainError,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Cell {
    pub arm: String,
    pub payload: u64,
    pub capacity: u64,
    pub in_flight: u64,
    pub memory: u64,
}

impl Cell {
    pub fn condition(&self) -> Condition {
        Condition {
            payload: self.payload,
            capacity: self.capacity,
            in_flight: self.in_flight,
            memory: self.memory,
        }
    }
}

#[derive(
    Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, serde::Serialize,
)]
pub struct Condition {
    pub payload: u64,
    pub capacity: u64,
    pub in_flight: u64,
    pub memory: u64,
}

impl Condition {
    pub fn cell(self, arm: &super::family::Arm) -> Cell {
        Cell {
            arm: arm.key.into(),
            payload: self.payload,
            capacity: self.capacity,
            in_flight: self.in_flight,
            memory: self.memory,
        }
    }
}

pub fn condition(payload: u64, capacity: u64, in_flight: u64) -> Condition {
    let working = payload.max(64) * capacity * 2;
    Condition {
        payload,
        capacity,
        in_flight,
        memory: (working + 4 * 1024 * 1024).next_multiple_of(4096),
    }
}

#[derive(Clone, Copy)]
pub struct Scheduled {
    pub block: u32,
    pub order: u32,
    pub condition: Condition,
    pub arm: &'static super::family::Arm,
}

impl Scheduled {
    pub fn cell(self) -> Cell {
        self.condition.cell(self.arm)
    }
}

fn random(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut value = *state;
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

pub fn schedule(
    members: &[(Condition, &'static super::family::Arm)],
    blocks: u32,
    seed: u64,
) -> Vec<Scheduled> {
    let mut state = seed;
    let mut result = Vec::with_capacity(members.len() * blocks as usize);
    for block in 0..blocks {
        let mut shuffled = members.to_vec();
        for index in (1..shuffled.len()).rev() {
            let selected = random(&mut state) as usize % (index + 1);
            shuffled.swap(index, selected);
        }
        for (order, (condition, arm)) in shuffled.into_iter().enumerate() {
            result.push(Scheduled {
                block,
                order: order as u32,
                condition,
                arm,
            });
        }
    }
    result
}

pub fn schedule_id(entries: &[Scheduled]) -> u64 {
    entries.iter().fold(OFFSET, |hash, entry| {
        identity(
            hash,
            entry.block,
            entry.order,
            entry.condition,
            entry.arm.key,
        )
    })
}

const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
fn identity(mut hash: u64, block: u32, order: u32, cell: Condition, arm: &str) -> u64 {
    hash = [
        block as u64,
        order as u64,
        cell.payload,
        cell.capacity,
        cell.in_flight,
        cell.memory,
    ]
    .into_iter()
    .fold(hash, |hash, value| {
        (hash ^ value).wrapping_mul(0x100_0000_01b3)
    });
    arm.as_bytes().iter().fold(hash, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    })
}

pub fn trial_schedule_id(trials: &[Trial]) -> u64 {
    trials.iter().fold(OFFSET, |hash, trial| {
        identity(
            hash,
            trial.block,
            trial.order,
            trial.cell.condition(),
            &trial.cell.arm,
        )
    })
}

pub fn window(remaining: u64, capacity: u64, in_flight: u64) -> u64 {
    remaining.min(capacity).min(in_flight)
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Observed {
    pub payload: u64,
    pub capacity: u64,
    pub in_flight: u64,
    pub window: u64,
    pub topology: String,
    pub transport: String,
    pub extent: Option<u64>,
    pub allocator: Option<String>,
    pub socket_send: Option<u64>,
    pub socket_recv: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Trial {
    pub block: u32,
    pub order: u32,
    pub cell: Cell,
    pub requested: u64,
    pub accepted: u64,
    pub completed: u64,
    pub validated: u64,
    pub elapsed_ns: Option<u64>,
    pub phase_ns: [u64; 3],
    pub observed: Option<Observed>,
    pub path: Path,
    pub status: Status,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Meta {
    pub format: u32,
    pub family: String,
    pub family_revision: u32,
    pub revision: String,
    pub dirty: bool,
    pub diff: String,
    pub target: String,
    pub os: String,
    pub arch: String,
    pub rustc: String,
    pub command: String,
    pub started: String,
    pub mode: String,
    pub seed: u64,
    pub warmup: u64,
    pub blocks: u32,
    pub timeout_ms: u64,
    pub schedule: u64,
    pub expected: usize,
    pub host: String,
    pub spin: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Study {
    pub meta: Meta,
    pub trials: Vec<Trial>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrialError {
    InvalidCell,
    CountOrder,
    CountMismatch,
    MissingElapsed,
    UnexpectedElapsed,
    ZeroElapsed,
    MissingObserved,
    MissingError,
    UnexpectedError,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudyError {
    Trial(TrialError),
    Format,
    Family,
    Metadata,
    OutOfBlock,
    DuplicateCell,
    DuplicateOrder,
    IncompleteBlock,
    Incomplete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecError {
    Study(StudyError),
    Syntax,
}

pub fn validate_trial(trial: &Trial) -> Result<(), TrialError> {
    if trial.cell.capacity == 0
        || trial.cell.in_flight == 0
        || trial.cell.memory == 0
        || trial.cell.arm.is_empty()
        || trial.cell.arm.contains(['\t', '\n', '\r'])
        || trial
            .error
            .as_deref()
            .is_some_and(|error| error.contains(['\t', '\n', '\r']))
    {
        return Err(TrialError::InvalidCell);
    }
    if trial.validated > trial.completed
        || trial.completed > trial.accepted
        || trial.accepted > trial.requested
    {
        return Err(TrialError::CountOrder);
    }
    if let Some(observed) = &trial.observed {
        let text = [
            observed.topology.as_str(),
            observed.transport.as_str(),
            observed.allocator.as_deref().unwrap_or(""),
        ];
        if observed.payload != trial.cell.payload
            || observed.capacity != trial.cell.capacity
            || observed.in_flight != trial.cell.in_flight
            || observed.window != window(trial.requested, trial.cell.capacity, trial.cell.in_flight)
            || text[..2]
                .iter()
                .any(|value| value.is_empty() || value.contains(['\t', '\n', '\r']))
            || text[2..]
                .iter()
                .any(|value| value.contains(['\t', '\n', '\r']))
        {
            return Err(TrialError::InvalidCell);
        }
    }
    match trial.status {
        Status::Ok => {
            if trial.observed.is_none() {
                return Err(TrialError::MissingObserved);
            }
            if trial.accepted != trial.requested
                || trial.completed != trial.requested
                || trial.validated != trial.requested
            {
                return Err(TrialError::CountMismatch);
            }
            match trial.elapsed_ns {
                None => return Err(TrialError::MissingElapsed),
                Some(0) => return Err(TrialError::ZeroElapsed),
                Some(_) => {}
            }
            if trial.error.is_some() {
                return Err(TrialError::UnexpectedError);
            }
            if trial.phase_ns.contains(&0) {
                return Err(TrialError::InvalidCell);
            }
        }
        Status::Unsupported => {
            if trial.accepted != 0 || trial.completed != 0 || trial.validated != 0 {
                return Err(TrialError::CountMismatch);
            }
        }
        Status::TimedError | Status::DrainError if trial.observed.is_none() => {
            return Err(TrialError::MissingObserved);
        }
        Status::Invalid | Status::SetupError | Status::TimedError | Status::DrainError => {}
    }
    if trial.status != Status::Ok && trial.elapsed_ns.is_some() {
        return Err(TrialError::UnexpectedElapsed);
    }
    if trial.status != Status::Ok && trial.error.as_deref().is_none_or(str::is_empty) {
        return Err(TrialError::MissingError);
    }
    Ok(())
}

type Blocks = (Vec<HashSet<Cell>>, Vec<HashSet<u32>>);

fn admit_prefix(study: &Study) -> Result<Blocks, StudyError> {
    let meta = &study.meta;
    let fields = [
        &meta.family,
        &meta.revision,
        &meta.diff,
        &meta.target,
        &meta.os,
        &meta.arch,
        &meta.rustc,
        &meta.command,
        &meta.started,
        &meta.mode,
        &meta.host,
    ];
    if meta.format != 5 {
        return Err(StudyError::Format);
    }
    let family = super::family::find(&meta.family)
        .filter(|family| family.revision == meta.family_revision)
        .ok_or(StudyError::Family)?;
    if meta.blocks == 0
        || meta.timeout_ms == 0
        || meta.expected == 0
        || study.trials.len() > meta.expected
        || fields
            .into_iter()
            .any(|field| field.is_empty() || field.contains(['\t', '\n', '\r']))
    {
        return Err(StudyError::Metadata);
    }
    let mut cells = vec![HashSet::new(); meta.blocks as usize];
    let mut orders = vec![HashSet::new(); meta.blocks as usize];
    for trial in &study.trials {
        validate_trial(trial).map_err(StudyError::Trial)?;
        let arm = family.arm(&trial.cell.arm).ok_or(StudyError::Family)?;
        if trial
            .observed
            .as_ref()
            .is_some_and(|observed| !arm.admits(observed))
        {
            return Err(StudyError::Trial(TrialError::InvalidCell));
        }
        if (trial.path.wait_returned && !trial.path.wait_entered)
            || (trial.path.stale_wake && !trial.path.wait_returned)
        {
            return Err(StudyError::Format);
        }
        let Some(block_cells) = cells.get_mut(trial.block as usize) else {
            return Err(StudyError::OutOfBlock);
        };
        if !block_cells.insert(trial.cell.clone()) {
            return Err(StudyError::DuplicateCell);
        }
        if !orders[trial.block as usize].insert(trial.order) {
            return Err(StudyError::DuplicateOrder);
        }
    }
    Ok((cells, orders))
}

pub fn validate_prefix(study: &Study) -> Result<(), StudyError> {
    admit_prefix(study).map(drop)
}

pub fn validate_study(study: &Study) -> Result<(), StudyError> {
    let (cells, orders) = admit_prefix(study)?;
    if study.trials.len() != study.meta.expected
        || trial_schedule_id(&study.trials) != study.meta.schedule
    {
        return Err(StudyError::Incomplete);
    }
    let expected = cells.first().ok_or(StudyError::IncompleteBlock)?;
    if expected.is_empty()
        || cells.iter().any(|block| block != expected)
        || orders.iter().any(|block| {
            block.len() != expected.len()
                || !(0..expected.len() as u32).all(|order| block.contains(&order))
        })
    {
        return Err(StudyError::IncompleteBlock);
    }
    Ok(())
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
enum Line {
    Header(Meta),
    Trial(Trial),
    End(End),
}

#[derive(serde::Deserialize, serde::Serialize)]
struct End {
    rows: usize,
    schedule: u64,
    digest: String,
}

pub fn decode(input: &str) -> Result<Study, CodecError> {
    let (study, complete) = parse(input)?;
    complete
        .then_some(study)
        .ok_or(CodecError::Study(StudyError::Incomplete))
}

pub fn decode_prefix(input: &str) -> Result<Study, CodecError> {
    parse(input).map(|(study, _)| study)
}

fn parse(input: &str) -> Result<(Study, bool), CodecError> {
    if !input.ends_with('\n') {
        return Err(CodecError::Syntax);
    }
    let mut meta = None;
    let mut trials = Vec::new();
    let mut end = None;
    let mut digest = blake3::Hasher::new();
    for raw in input.split_inclusive('\n') {
        let line: Line = serde_json::from_str(raw.strip_suffix('\n').unwrap())
            .map_err(|_| CodecError::Syntax)?;
        match line {
            Line::Header(value) if meta.is_none() && trials.is_empty() => {
                meta = Some(value);
                digest.update(raw.as_bytes());
            }
            Line::Trial(value) if meta.is_some() && end.is_none() => {
                trials.push(value);
                digest.update(raw.as_bytes());
            }
            Line::End(value) if meta.is_some() && end.is_none() => end = Some(value),
            _ => return Err(CodecError::Syntax),
        }
    }
    let study = Study {
        meta: meta.ok_or(CodecError::Syntax)?,
        trials,
    };
    validate_prefix(&study).map_err(CodecError::Study)?;
    if let Some(end) = end {
        if end.rows != study.trials.len()
            || end.schedule != study.meta.schedule
            || end.digest != digest.finalize().to_hex().as_str()
        {
            return Err(CodecError::Study(StudyError::Incomplete));
        }
        validate_study(&study).map_err(CodecError::Study)?;
        Ok((study, true))
    } else {
        Ok((study, false))
    }
}

pub struct Loaded {
    pub study: Study,
    pub complete: bool,
}

pub fn load(path: &std::path::Path) -> Result<Loaded, String> {
    let input = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let (study, complete) = match decode(&input) {
        Ok(study) => (study, true),
        Err(CodecError::Study(StudyError::Incomplete)) => (
            decode_prefix(&input).map_err(|error| format!("{error:?}"))?,
            false,
        ),
        Err(error) => return Err(format!("{error:?}")),
    };
    Ok(Loaded { study, complete })
}

pub(super) struct Journal {
    file: std::fs::File,
    digest: blake3::Hasher,
}

impl Journal {
    pub(super) fn create(path: &std::path::Path) -> Result<Self, String> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            file,
            digest: blake3::Hasher::new(),
        })
    }

    pub(super) fn append(&mut self, value: &impl serde::Serialize) -> Result<(), String> {
        use std::io::Write;
        let mut line = serde_json::to_vec(value).map_err(|error| error.to_string())?;
        line.push(b'\n');
        self.file
            .write_all(&line)
            .and_then(|()| self.file.sync_data())
            .map_err(|error| error.to_string())?;
        self.digest.update(&line);
        Ok(())
    }

    pub(super) fn digest(&self) -> String {
        self.digest.clone().finalize().to_hex().to_string()
    }

    pub(super) fn seal(mut self, value: &impl serde::Serialize) -> Result<(), String> {
        use std::io::Write;
        serde_json::to_writer(&mut self.file, value).map_err(|error| error.to_string())?;
        self.file
            .write_all(b"\n")
            .and_then(|()| self.file.flush())
            .and_then(|()| self.file.sync_all())
            .map_err(|error| error.to_string())
    }
}

pub fn record(
    path: &std::path::Path,
    meta: Meta,
    trials: impl IntoIterator<Item = Result<Trial, String>>,
) -> Result<(), String> {
    if meta.format != 5 {
        return Err("recorder only writes evidence format 5".into());
    }
    let mut study = Study {
        meta,
        trials: Vec::new(),
    };
    validate_prefix(&study).map_err(|error| format!("{error:?}"))?;
    let mut journal = Journal::create(path)?;
    journal.append(&Line::Header(study.meta.clone()))?;
    for trial in trials {
        study.trials.push(trial?);
        if let Err(error) = validate_prefix(&study) {
            study.trials.pop();
            return Err(format!("{error:?}"));
        }
        let trial = study.trials.last().unwrap();
        journal.append(&Line::Trial(trial.clone()))?;
        if trial.status != Status::Ok {
            return Err("mandatory trial failed; inspect the unsealed evidence record".into());
        }
    }
    validate_study(&study).map_err(|error| format!("{error:?}"))?;
    let end = End {
        rows: study.trials.len(),
        schedule: study.meta.schedule,
        digest: journal.digest(),
    };
    journal.seal(&Line::End(end))
}
