#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkError {
    NoWorkers,
}

pub fn distribute(total: u64, workers: usize) -> Result<Vec<u64>, WorkError> {
    if workers == 0 {
        return Err(WorkError::NoWorkers);
    }
    let workers = workers as u64;
    let quotient = total / workers;
    let remainder = total % workers;
    Ok((0..workers)
        .map(|worker| quotient + u64::from(worker < remainder))
        .collect())
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
    pub payload: u64,
    pub capacity: u64,
    pub in_flight: u64,
    pub memory: u64,
}

pub fn schedule(cells: &[Cell], blocks: u32, seed: u64) -> Vec<(u32, u32, Cell)> {
    let mut state = seed;
    let mut result = Vec::with_capacity(cells.len() * blocks as usize);
    for block in 0..blocks {
        let mut shuffled = cells.to_vec();
        for index in (1..shuffled.len()).rev() {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut random = state;
            random = (random ^ (random >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            random = (random ^ (random >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            random ^= random >> 31;
            shuffled.swap(index, random as usize % (index + 1));
        }
        result.extend(
            shuffled
                .into_iter()
                .enumerate()
                .map(|(order, cell)| (block, order as u32, cell)),
        );
    }
    result
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
    if trial.cell.capacity == 0
        || trial.cell.in_flight == 0
        || trial.cell.memory == 0
        || trial.cell.implementation.is_empty()
        || trial.cell.policy.is_empty()
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
            "TRIAL\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            trial.block,
            trial.order,
            trial.cell.implementation,
            trial.cell.policy,
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
        if fields.len() != 16 || fields[0] != "TRIAL" {
            return Err(CodecError::Syntax);
        }
        trials.push(Trial {
            block: number(fields[1])?,
            order: number(fields[2])?,
            cell: Cell {
                implementation: fields[3].into(),
                policy: fields[4].into(),
                payload: number(fields[5])?,
                capacity: number(fields[6])?,
                in_flight: number(fields[7])?,
                memory: number(fields[8])?,
            },
            requested: number(fields[9])?,
            accepted: number(fields[10])?,
            completed: number(fields[11])?,
            validated: number(fields[12])?,
            elapsed_ns: if fields[13].is_empty() {
                None
            } else {
                Some(number(fields[13])?)
            },
            status: parse_status(fields[14])?,
            error: (!fields[15].is_empty()).then(|| fields[15].into()),
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
