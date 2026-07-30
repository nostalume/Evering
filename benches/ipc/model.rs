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

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
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

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContrastKey {
    pub payload: u64,
    pub capacity: u64,
    pub in_flight: u64,
    pub memory: u64,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
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

pub fn schedule_id(entries: &[Scheduled]) -> u64 {
    entries.iter().fold(0xcbf2_9ce4_8422_2325, |hash, entry| {
        schedule_hash(
            hash,
            [
                entry.block as u64,
                entry.order as u64,
                entry.contrast.key.payload,
                entry.contrast.key.capacity,
                entry.contrast.key.in_flight,
                entry.contrast.key.memory,
                match entry.contrast.candidate {
                    Policy::Busy => 0,
                    Policy::Adaptive => 1,
                    Policy::Notified => 2,
                },
                u64::from(matches!(entry.arm, Arm::Stream)),
            ],
        )
    })
}

fn schedule_hash(hash: u64, values: [u64; 8]) -> u64 {
    values.into_iter().fold(hash, |hash, value| {
        (hash ^ value).wrapping_mul(0x100_0000_01b3)
    })
}

pub fn trial_schedule_id(trials: &[Trial]) -> u64 {
    trials.iter().fold(0xcbf2_9ce4_8422_2325, |hash, trial| {
        schedule_hash(
            hash,
            [
                trial.block as u64,
                trial.order as u64,
                trial.cell.payload,
                trial.cell.capacity,
                trial.cell.in_flight,
                trial.cell.memory,
                match trial.cell.candidate.as_str() {
                    "busy" => 0,
                    "adaptive" => 1,
                    _ => 2,
                },
                u64::from(trial.cell.implementation == "os-stream"),
            ],
        )
    })
}

pub fn window(remaining: u64, capacity: u64, in_flight: u64) -> u64 {
    remaining.min(capacity).min(in_flight)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observed {
    pub payload: u64,
    pub capacity: u64,
    pub in_flight: u64,
    pub batch: u64,
    pub topology: String,
    pub transport: String,
    pub extent: Option<u64>,
    pub allocator: Option<String>,
    pub socket_send: Option<u64>,
    pub socket_recv: Option<u64>,
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
    pub phase_ns: [u64; 3],
    pub observed: Option<Observed>,
    pub status: Status,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    pub format: u32,
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
    MissingObserved,
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
    Incomplete,
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
    if let Some(observed) = &trial.observed {
        let text = [
            observed.topology.as_str(),
            observed.transport.as_str(),
            observed.allocator.as_deref().unwrap_or(""),
        ];
        let valid_resource = if trial.cell.implementation == "os-stream" {
            observed.extent.is_none()
                && observed.allocator.is_none()
                && observed.socket_send.is_some_and(|value| value > 0)
                && observed.socket_recv.is_some_and(|value| value > 0)
        } else {
            observed.extent.is_some_and(|value| value > 0)
                && observed
                    .allocator
                    .as_deref()
                    .is_some_and(|value| !value.is_empty())
                && observed.socket_send.is_none()
                && observed.socket_recv.is_none()
        };
        if observed.capacity == 0
            || observed.in_flight == 0
            || observed.batch == 0
            || observed.batch > observed.capacity.min(observed.in_flight)
            || text[..2]
                .iter()
                .any(|value| value.is_empty() || value.contains(['\t', '\n', '\r']))
            || text[2..]
                .iter()
                .any(|value| value.contains(['\t', '\n', '\r']))
            || !valid_resource
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

pub fn validate_prefix(study: &Study) -> Result<(), StudyError> {
    use std::collections::HashSet;

    let meta = &study.meta;
    let fields = [
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
    if meta.format != 2 {
        return Err(StudyError::Format);
    }
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
    Ok(())
}

pub fn validate_study(study: &Study) -> Result<(), StudyError> {
    use std::collections::HashSet;

    validate_prefix(study)?;
    if study.trials.len() != study.meta.expected {
        return Err(StudyError::Incomplete);
    }
    if trial_schedule_id(&study.trials) != study.meta.schedule {
        return Err(StudyError::Incomplete);
    }
    let mut cells = vec![HashSet::new(); study.meta.blocks as usize];
    let mut orders = vec![HashSet::new(); study.meta.blocks as usize];
    for trial in &study.trials {
        cells[trial.block as usize].insert(trial.cell.clone());
        orders[trial.block as usize].insert(trial.order);
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

fn meta_line(meta: &Meta) -> String {
    format!(
        "META\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
        meta.format,
        meta.revision,
        u8::from(meta.dirty),
        meta.diff,
        meta.target,
        meta.os,
        meta.arch,
        meta.rustc,
        meta.command,
        meta.started,
        meta.mode,
        meta.seed,
        meta.warmup,
        meta.blocks,
        meta.timeout_ms,
        meta.schedule,
        meta.expected,
        meta.host,
        meta.spin,
    )
}

fn trial_line(trial: &Trial) -> String {
    let observed = trial.observed.as_ref();
    let observed_number = |field: fn(&Observed) -> Option<u64>| {
        observed
            .and_then(field)
            .map_or(String::new(), |value| value.to_string())
    };
    format!(
        "TRIAL\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
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
        trial.phase_ns[0],
        trial.phase_ns[1],
        trial.phase_ns[2],
        observed.map_or(String::new(), |value| value.payload.to_string()),
        observed.map_or(String::new(), |value| value.capacity.to_string()),
        observed.map_or(String::new(), |value| value.in_flight.to_string()),
        observed.map_or(String::new(), |value| value.batch.to_string()),
        observed.map_or("", |value| value.topology.as_str()),
        observed.map_or("", |value| value.transport.as_str()),
        observed_number(|value| value.extent),
        observed.map_or("", |value| value.allocator.as_deref().unwrap_or("")),
        observed_number(|value| value.socket_send),
        observed_number(|value| value.socket_recv),
        status_name(trial.status),
        trial.error.as_deref().unwrap_or(""),
    )
}

pub fn decode(input: &str) -> Result<Study, CodecError> {
    let input = input.strip_suffix('\n').unwrap_or(input);
    let (prefix, footer) = input
        .rsplit_once('\n')
        .ok_or(CodecError::Study(StudyError::Incomplete))?;
    if !footer.starts_with("END\t") {
        return Err(CodecError::Study(StudyError::Incomplete));
    }
    let fields: Vec<_> = footer.split('\t').collect();
    let study = decode_prefix(&(prefix.to_owned() + "\n"))?;
    if fields.len() != 3
        || fields[0] != "END"
        || number::<u64>(fields[1])? != study.meta.schedule
        || number::<usize>(fields[2])? != study.meta.expected
    {
        return Err(CodecError::Study(StudyError::Incomplete));
    }
    validate_study(&study).map_err(CodecError::Study)?;
    Ok(study)
}

pub fn decode_prefix(input: &str) -> Result<Study, CodecError> {
    let input = if input.ends_with('\n') {
        input
    } else {
        input
            .rsplit_once('\n')
            .map(|(complete, _)| complete)
            .ok_or(CodecError::Syntax)?
    };
    let mut lines = input.lines();
    let meta = lines.next().ok_or(CodecError::Syntax)?;
    let fields: Vec<_> = meta.split('\t').collect();
    if fields.len() != 20 || fields[0] != "META" {
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
        diff: fields[4].into(),
        target: fields[5].into(),
        os: fields[6].into(),
        arch: fields[7].into(),
        rustc: fields[8].into(),
        command: fields[9].into(),
        started: fields[10].into(),
        mode: fields[11].into(),
        seed: number(fields[12])?,
        warmup: number(fields[13])?,
        blocks: number(fields[14])?,
        timeout_ms: number(fields[15])?,
        schedule: number(fields[16])?,
        expected: number(fields[17])?,
        host: fields[18].into(),
        spin: number(fields[19])?,
    };
    let mut trials = Vec::new();
    for line in lines {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 30 || fields[0] != "TRIAL" {
            return Err(CodecError::Syntax);
        }
        let observed = if fields[18..28].iter().all(|field| field.is_empty()) {
            None
        } else {
            Some(Observed {
                payload: number(fields[18])?,
                capacity: number(fields[19])?,
                in_flight: number(fields[20])?,
                batch: number(fields[21])?,
                topology: fields[22].into(),
                transport: fields[23].into(),
                extent: optional_number(fields[24])?,
                allocator: (!fields[25].is_empty()).then(|| fields[25].into()),
                socket_send: optional_number(fields[26])?,
                socket_recv: optional_number(fields[27])?,
            })
        };
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
            phase_ns: [
                number(fields[15])?,
                number(fields[16])?,
                number(fields[17])?,
            ],
            observed,
            status: parse_status(fields[28])?,
            error: (!fields[29].is_empty()).then(|| fields[29].into()),
        });
    }
    let study = Study { meta, trials };
    validate_prefix(&study).map_err(CodecError::Study)?;
    Ok(study)
}

pub struct Loaded {
    pub study: Study,
    pub complete: bool,
}

pub fn load(path: &std::path::Path) -> Result<Loaded, String> {
    let input = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let partial = path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().ends_with(".partial"));
    let study = if partial {
        let trimmed = input.strip_suffix('\n').unwrap_or(&input);
        let prefix = trimmed
            .rsplit_once('\n')
            .filter(|(_, line)| line.starts_with("END\t"))
            .map_or_else(|| input.clone(), |(prefix, _)| format!("{prefix}\n"));
        decode_prefix(&prefix)
    } else {
        decode(&input)
    }
    .map_err(|error| format!("{error:?}"))?;
    Ok(Loaded {
        study,
        complete: !partial,
    })
}

pub struct Recorder {
    final_path: std::path::PathBuf,
    partial_path: std::path::PathBuf,
    file: Option<std::fs::File>,
    study: Study,
    failed: bool,
}

impl Recorder {
    pub fn create(path: &std::path::Path, meta: Meta) -> Result<Self, String> {
        use std::io::Write;

        if path.exists() {
            return Err("evidence already exists".into());
        }
        let partial_path = std::path::PathBuf::from(format!("{}.partial", path.display()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial_path)
            .map_err(|error| error.to_string())?;
        let study = Study {
            meta,
            trials: Vec::new(),
        };
        validate_prefix(&study).map_err(|error| format!("{error:?}"))?;
        file.write_all(meta_line(&study.meta).as_bytes())
            .and_then(|()| file.sync_data())
            .map_err(|error| error.to_string())?;
        Ok(Self {
            final_path: path.into(),
            partial_path,
            file: Some(file),
            study,
            failed: false,
        })
    }

    pub fn append(&mut self, trial: Trial) -> Result<(), String> {
        use std::io::Write;

        self.study.trials.push(trial);
        if let Err(error) = validate_prefix(&self.study) {
            self.study.trials.pop();
            return Err(format!("{error:?}"));
        }
        let file = self.file.as_mut().ok_or("recorder finished")?;
        let result = file
            .write_all(trial_line(self.study.trials.last().unwrap()).as_bytes())
            .and_then(|()| file.sync_data())
            .map_err(|error| error.to_string());
        self.failed |= result.is_err();
        result
    }

    pub fn finish(mut self) -> Result<(), String> {
        use std::io::Write;

        if self.failed {
            return Err("recorder failed".into());
        }
        validate_study(&self.study).map_err(|error| format!("{error:?}"))?;
        let mut file = self.file.take().ok_or("recorder finished")?;
        file.write_all(
            format!(
                "END\t{}\t{}\n",
                self.study.meta.schedule, self.study.meta.expected
            )
            .as_bytes(),
        )
        .and_then(|()| file.sync_all())
        .map_err(|error| error.to_string())?;
        drop(file);
        std::fs::hard_link(&self.partial_path, &self.final_path)
            .and_then(|()| std::fs::remove_file(&self.partial_path))
            .map_err(|error| error.to_string())
    }
}

fn number<T: core::str::FromStr>(value: &str) -> Result<T, CodecError> {
    value.parse().map_err(|_| CodecError::Syntax)
}

fn optional_number<T: core::str::FromStr>(value: &str) -> Result<Option<T>, CodecError> {
    (!value.is_empty()).then(|| number(value)).transpose()
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
