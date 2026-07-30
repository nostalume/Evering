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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Unsupported,
    Invalid,
    SetupError,
    TimedError,
    DrainError,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct Cell {
    pub implementation: String,
    pub policy: String,
    pub candidate: String,
    pub payload: u64,
    pub capacity: u64,
    pub in_flight: u64,
    pub memory: u64,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum Policy {
    Busy,
    Adaptive,
    Notified,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum Arm {
    Evering(Policy),
    Stream,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct ContrastKey {
    pub payload: u64,
    pub capacity: u64,
    pub in_flight: u64,
    pub memory: u64,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct Contrast {
    pub key: ContrastKey,
    pub candidate: Policy,
}

impl Contrast {
    pub fn cell(self, arm: Arm) -> Cell {
        let (implementation, policy) = match arm {
            Arm::Evering(Policy::Busy) => ("evering", "busy"),
            Arm::Evering(Policy::Adaptive) => ("evering", "adaptive"),
            Arm::Evering(Policy::Notified) => ("evering", "notified"),
            Arm::Stream => ("os-stream", "blocking"),
        };
        Cell {
            implementation: implementation.into(),
            policy: policy.into(),
            candidate: match self.candidate {
                Policy::Busy => "busy",
                Policy::Adaptive => "adaptive",
                Policy::Notified => "notified",
            }
            .into(),
            payload: self.key.payload,
            capacity: self.key.capacity,
            in_flight: self.key.in_flight,
            memory: self.key.memory,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scheduled {
    pub block: u32,
    pub order: u32,
    pub contrast: Contrast,
    pub arm: Arm,
}

fn random(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut value = *state;
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

pub fn schedule(contrasts: &[Contrast], blocks: u32, seed: u64) -> Vec<Scheduled> {
    let mut state = seed;
    let mut result = Vec::with_capacity(contrasts.len() * blocks as usize * 2);
    for block in 0..blocks {
        let mut shuffled = contrasts.to_vec();
        for index in (1..shuffled.len()).rev() {
            let selected = random(&mut state) as usize % (index + 1);
            shuffled.swap(index, selected);
        }
        let mut order = 0;
        for contrast in shuffled {
            let mut arms = [Arm::Evering(contrast.candidate), Arm::Stream];
            if random(&mut state) & 1 == 1 {
                arms.reverse();
            }
            for arm in arms {
                result.push(Scheduled {
                    block,
                    order,
                    contrast,
                    arm,
                });
                order += 1;
            }
        }
    }
    result
}

pub fn window(remaining: u64, capacity: u64, in_flight: u64) -> u64 {
    remaining.min(capacity).min(in_flight)
}

pub fn mandatory_success(trials: &[Trial]) -> bool {
    trials.iter().all(|trial| trial.status == Status::Ok)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trial {
    pub block: u32,
    pub order: u32,
    pub cell: Cell,
    pub requested: u64,
    pub accepted: u64,
    pub completed: u64,
    pub validated: u64,
    pub elapsed_ns: Option<u64>,
    pub status: Status,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    pub format: u32,
    pub revision: String,
    pub dirty: bool,
    pub target: String,
    pub os: String,
    pub arch: String,
    pub rustc: String,
    pub command: String,
    pub started: String,
    pub seed: u64,
    pub warmup: u64,
    pub blocks: u32,
    pub timeout_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
    MissingError,
    UnexpectedError,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudyError {
    Trial(TrialError),
    Format,
    Metadata,
    OutOfBlock,
    DuplicateCell,
    DuplicateOrder,
    IncompleteBlock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecError {
    Study(StudyError),
    Syntax,
}

pub fn validate_trial(trial: &Trial) -> Result<(), TrialError> {
    let candidate = trial.cell.candidate.as_str();
    let valid_candidate = matches!(candidate, "busy" | "adaptive" | "notified");
    let valid_arm = match (
        trial.cell.implementation.as_str(),
        trial.cell.policy.as_str(),
    ) {
        ("evering", policy) => policy == candidate,
        ("os-stream", "blocking") => true,
        _ => false,
    };
    if trial.cell.capacity == 0
        || trial.cell.in_flight == 0
        || trial.cell.memory == 0
        || !valid_candidate
        || !valid_arm
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
    match trial.status {
        Status::Ok => {
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
        }
        Status::Unsupported => {
            if trial.accepted != 0 || trial.completed != 0 || trial.validated != 0 {
                return Err(TrialError::CountMismatch);
            }
            if trial.elapsed_ns.is_some() {
                return Err(TrialError::UnexpectedElapsed);
            }
            if trial.error.as_deref().is_none_or(str::is_empty) {
                return Err(TrialError::MissingError);
            }
        }
        Status::Invalid | Status::SetupError | Status::TimedError | Status::DrainError => {
            if trial.elapsed_ns.is_some() {
                return Err(TrialError::UnexpectedElapsed);
            }
            if trial.error.as_deref().is_none_or(str::is_empty) {
                return Err(TrialError::MissingError);
            }
        }
    }
    Ok(())
}

pub fn validate_study(study: &Study) -> Result<(), StudyError> {
    use std::collections::HashSet;

    let meta = &study.meta;
    let fields = [
        &meta.revision,
        &meta.target,
        &meta.os,
        &meta.arch,
        &meta.rustc,
        &meta.command,
        &meta.started,
    ];
    if meta.format != 1 {
        return Err(StudyError::Format);
    }
    if meta.blocks == 0
        || meta.timeout_ms == 0
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
    let Some(expected) = cells.first() else {
        return Err(StudyError::IncompleteBlock);
    };
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

pub fn encode(study: &Study) -> Result<String, CodecError> {
    use std::fmt::Write;

    validate_study(study).map_err(CodecError::Study)?;
    let meta = &study.meta;
    let mut output = String::new();
    writeln!(
        output,
        "META\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        meta.format,
        meta.revision,
        u8::from(meta.dirty),
        meta.target,
        meta.os,
        meta.arch,
        meta.rustc,
        meta.command,
        meta.started,
        meta.seed,
        meta.warmup,
        meta.blocks,
        meta.timeout_ms,
    )
    .unwrap();
    for trial in &study.trials {
        writeln!(
            output,
            "TRIAL\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            trial.block,
            trial.order,
            trial.cell.implementation,
            trial.cell.policy,
            trial.cell.candidate,
            trial.cell.payload,
            trial.cell.capacity,
            trial.cell.in_flight,
            trial.cell.memory,
            trial.requested,
            trial.accepted,
            trial.completed,
            trial.validated,
            trial
                .elapsed_ns
                .map_or(String::new(), |value| value.to_string()),
            status_name(trial.status),
            trial.error.as_deref().unwrap_or(""),
        )
        .unwrap();
    }
    Ok(output)
}

pub fn decode(input: &str) -> Result<Study, CodecError> {
    let mut lines = input.lines();
    let meta = lines.next().ok_or(CodecError::Syntax)?;
    let fields: Vec<_> = meta.split('\t').collect();
    if fields.len() != 14 || fields[0] != "META" {
        return Err(CodecError::Syntax);
    }
    let meta = Meta {
        format: number(fields[1])?,
        revision: fields[2].into(),
        dirty: match fields[3] {
            "0" => false,
            "1" => true,
            _ => return Err(CodecError::Syntax),
        },
        target: fields[4].into(),
        os: fields[5].into(),
        arch: fields[6].into(),
        rustc: fields[7].into(),
        command: fields[8].into(),
        started: fields[9].into(),
        seed: number(fields[10])?,
        warmup: number(fields[11])?,
        blocks: number(fields[12])?,
        timeout_ms: number(fields[13])?,
    };
    let mut trials = Vec::new();
    for line in lines {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 17 || fields[0] != "TRIAL" {
            return Err(CodecError::Syntax);
        }
        trials.push(Trial {
            block: number(fields[1])?,
            order: number(fields[2])?,
            cell: Cell {
                implementation: fields[3].into(),
                policy: fields[4].into(),
                candidate: fields[5].into(),
                payload: number(fields[6])?,
                capacity: number(fields[7])?,
                in_flight: number(fields[8])?,
                memory: number(fields[9])?,
            },
            requested: number(fields[10])?,
            accepted: number(fields[11])?,
            completed: number(fields[12])?,
            validated: number(fields[13])?,
            elapsed_ns: if fields[14].is_empty() {
                None
            } else {
                Some(number(fields[14])?)
            },
            status: parse_status(fields[15])?,
            error: (!fields[16].is_empty()).then(|| fields[16].into()),
        });
    }
    let study = Study { meta, trials };
    validate_study(&study).map_err(CodecError::Study)?;
    Ok(study)
}

fn number<T: core::str::FromStr>(value: &str) -> Result<T, CodecError> {
    value.parse().map_err(|_| CodecError::Syntax)
}

fn status_name(status: Status) -> &'static str {
    match status {
        Status::Ok => "ok",
        Status::Unsupported => "unsupported",
        Status::Invalid => "invalid",
        Status::SetupError => "setup-error",
        Status::TimedError => "timed-error",
        Status::DrainError => "drain-error",
    }
}

fn parse_status(value: &str) -> Result<Status, CodecError> {
    match value {
        "ok" => Ok(Status::Ok),
        "unsupported" => Ok(Status::Unsupported),
        "invalid" => Ok(Status::Invalid),
        "setup-error" => Ok(Status::SetupError),
        "timed-error" => Ok(Status::TimedError),
        "drain-error" => Ok(Status::DrainError),
        _ => Err(CodecError::Syntax),
    }
}
