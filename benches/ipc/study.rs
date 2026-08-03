use std::{collections::HashSet, marker::PhantomData, path::Path};

use serde::{Serialize, de::DeserializeOwned};

pub trait Identity {
    fn identity(&self, hash: &mut blake3::Hasher);
}

fn field(hash: &mut blake3::Hasher, bytes: &[u8]) {
    hash.update(&(bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

pub fn random(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut value = *state;
    value = (value ^ value >> 30).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ value >> 27).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ value >> 31
}

impl Identity for String {
    fn identity(&self, hash: &mut blake3::Hasher) {
        field(hash, self.as_bytes());
    }
}

impl Identity for u32 {
    fn identity(&self, hash: &mut blake3::Hasher) {
        hash.update(&self.to_le_bytes());
    }
}

impl Identity for u64 {
    fn identity(&self, hash: &mut blake3::Hasher) {
        hash.update(&self.to_le_bytes());
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct SchemaId {
    pub name: String,
    pub revision: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Source {
    pub revision: String,
    pub dirty: bool,
    pub diff: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Compiler {
    pub target: String,
    pub rustc: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Host {
    pub os: String,
    pub arch: String,
    pub description: String,
    pub environment: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Context {
    pub source: Source,
    pub compiler: Compiler,
    pub host: Host,
}

impl Identity for Context {
    fn identity(&self, hash: &mut blake3::Hasher) {
        self.source.revision.identity(hash);
        hash.update(&[u8::from(self.source.dirty)]);
        self.source.diff.identity(hash);
        self.compiler.target.identity(hash);
        self.compiler.rustc.identity(hash);
        self.host.os.identity(hash);
        self.host.arch.identity(hash);
        self.host.description.identity(hash);
        self.host.environment.identity(hash);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Run {
    pub seed: u64,
    pub started: String,
    pub command: String,
    pub warmup: u64,
    pub budget_ms: u64,
    pub schedule: Vec<Scheduled>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Header<P, C> {
    pub schema: SchemaId,
    pub context: Context,
    pub specification: P,
    pub cases: Vec<C>,
    pub run: Run,
}

impl<P, C> Header<P, C> {
    pub fn new<S: Schema<Specification = P, Case = C>>(
        context: Context,
        specification: P,
        cases: Vec<C>,
        run: Run,
    ) -> Self {
        Self {
            schema: S::id(),
            context,
            specification,
            cases,
            run,
        }
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Unit {
    pub block: u32,
    pub order: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Scheduled {
    pub unit: Unit,
    pub case: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Observation<M> {
    pub unit: Unit,
    pub case: u32,
    pub measure: M,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Complete {
    pub rows: u64,
    pub schedule: String,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Abort {
    pub rows: u64,
    pub at: Option<Unit>,
    pub reason: String,
    pub content: String,
}

pub trait Schema: Sized + 'static {
    const NAME: &'static str;
    const REVISION: u32;
    type Specification: Clone + Identity + Serialize + DeserializeOwned;
    type Case: Clone + Eq + Identity + Serialize + DeserializeOwned;
    type Measure: Clone + Serialize + DeserializeOwned;

    fn validate_header(_: &Header<Self::Specification, Self::Case>) -> Result<(), String> {
        Ok(())
    }

    fn validate(
        header: &Header<Self::Specification, Self::Case>,
        observation: &Observation<Self::Measure>,
    ) -> Result<(), String>;

    fn id() -> SchemaId {
        SchemaId {
            name: Self::NAME.into(),
            revision: Self::REVISION,
        }
    }
}

fn identity(value: &impl Identity) -> String {
    let mut hash = blake3::Hasher::new();
    value.identity(&mut hash);
    hash.finalize().to_hex().to_string()
}

pub fn context_id(context: &Context) -> String {
    identity(context)
}

pub fn spec_id<S: Schema>(specification: &S::Specification) -> String {
    let mut hash = blake3::Hasher::new();
    field(&mut hash, S::NAME.as_bytes());
    S::REVISION.identity(&mut hash);
    specification.identity(&mut hash);
    hash.finalize().to_hex().to_string()
}

pub fn case_id<S: Schema>(case: &S::Case) -> String {
    let mut hash = blake3::Hasher::new();
    field(&mut hash, S::NAME.as_bytes());
    S::REVISION.identity(&mut hash);
    case.identity(&mut hash);
    hash.finalize().to_hex().to_string()
}

pub fn study_id<S: Schema>(header: &Header<S::Specification, S::Case>) -> String {
    let mut hash = blake3::Hasher::new();
    field(&mut hash, context_id(&header.context).as_bytes());
    field(&mut hash, spec_id::<S>(&header.specification).as_bytes());
    hash.finalize().to_hex().to_string()
}

pub struct Study<S: Schema> {
    pub header: Header<S::Specification, S::Case>,
    pub observations: Vec<Observation<S::Measure>>,
    pub complete: Complete,
    marker: PhantomData<S>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Io,
    Syntax,
    Schema,
    Schedule,
    Invalid,
    Incomplete,
    Aborted,
    Digest,
}

#[derive(serde::Deserialize)]
struct RawLine {
    kind: String,
    value: serde_json::Value,
}

#[derive(serde::Deserialize)]
struct HeaderProbe {
    schema: SchemaId,
}

pub fn schema_str(input: &str) -> Result<SchemaId, Error> {
    let raw: RawLine = serde_json::from_str(input.lines().next().ok_or(Error::Incomplete)?)
        .map_err(|_| Error::Syntax)?;
    if raw.kind != "header" {
        return Err(Error::Syntax);
    }
    serde_json::from_value::<HeaderProbe>(raw.value)
        .map(|header| header.schema)
        .map_err(|_| Error::Syntax)
}

#[derive(serde::Serialize)]
struct Line<'a, T> {
    kind: &'static str,
    value: &'a T,
}

fn schedule_digest(entries: &[Scheduled]) -> String {
    let mut digest = blake3::Hasher::new();
    for entry in entries {
        digest.update(&entry.unit.block.to_le_bytes());
        digest.update(&entry.unit.order.to_le_bytes());
        digest.update(&entry.case.to_le_bytes());
    }
    digest.finalize().to_hex().to_string()
}

fn validate_schedule<C>(header: &Header<impl Sized, C>) -> Result<(), Error> {
    let mut units = HashSet::with_capacity(header.run.schedule.len());
    if header
        .run
        .schedule
        .iter()
        .any(|entry| entry.case as usize >= header.cases.len() || !units.insert(entry.unit))
    {
        return Err(Error::Schedule);
    }
    Ok(())
}

fn check<S: Schema>(
    header: &Header<S::Specification, S::Case>,
    observations: &[Observation<S::Measure>],
) -> Result<(), Error> {
    let expected = &header.run.schedule;
    if expected.len() != observations.len() {
        return Err(Error::Incomplete);
    }
    for (index, observation) in observations.iter().enumerate() {
        if expected[index]
            != (Scheduled {
                unit: observation.unit,
                case: observation.case,
            })
        {
            return Err(Error::Schedule);
        }
        S::validate(header, observation).map_err(|_| Error::Invalid)?;
    }
    Ok(())
}

pub(super) struct Journal {
    file: std::fs::File,
    digest: blake3::Hasher,
}

impl Journal {
    pub(super) fn create(path: &Path) -> Result<Self, String> {
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

    pub(super) fn append(&mut self, value: &impl Serialize) -> Result<(), String> {
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

    pub(super) fn seal(mut self, value: &impl Serialize) -> Result<(), String> {
        use std::io::Write;
        serde_json::to_writer(&mut self.file, value).map_err(|error| error.to_string())?;
        self.file
            .write_all(b"\n")
            .and_then(|()| self.file.flush())
            .and_then(|()| self.file.sync_all())
            .map_err(|error| error.to_string())
    }
}

pub struct Recorder<S: Schema> {
    journal: Journal,
    header: Header<S::Specification, S::Case>,
    observations: Vec<Observation<S::Measure>>,
}

impl<S: Schema> Recorder<S> {
    pub fn create(path: &Path, header: Header<S::Specification, S::Case>) -> Result<Self, Error> {
        if header.schema != S::id() {
            return Err(Error::Schema);
        }
        S::validate_header(&header).map_err(|_| Error::Invalid)?;
        validate_schedule(&header)?;
        let mut journal = Journal::create(path).map_err(|_| Error::Io)?;
        journal
            .append(&Line {
                kind: "header",
                value: &header,
            })
            .map_err(|_| Error::Io)?;
        Ok(Self {
            journal,
            header,
            observations: Vec::new(),
        })
    }

    pub fn observe(&mut self, observation: Observation<S::Measure>) -> Result<(), Error> {
        let index = self.observations.len();
        if self.header.run.schedule.get(index)
            != Some(&Scheduled {
                unit: observation.unit,
                case: observation.case,
            })
            || observation.case as usize >= self.header.cases.len()
        {
            return Err(Error::Schedule);
        }
        S::validate(&self.header, &observation).map_err(|_| Error::Invalid)?;
        self.journal
            .append(&Line {
                kind: "observation",
                value: &observation,
            })
            .map_err(|_| Error::Io)?;
        self.observations.push(observation);
        Ok(())
    }

    pub fn complete(self) -> Result<String, Error> {
        check::<S>(&self.header, &self.observations)?;
        let complete = Complete {
            rows: self.observations.len() as u64,
            schedule: schedule_digest(&self.header.run.schedule),
            content: self.journal.digest(),
        };
        self.journal
            .seal(&Line {
                kind: "complete",
                value: &complete,
            })
            .map_err(|_| Error::Io)?;
        Ok(complete.content)
    }

    pub fn abort(self, at: Option<Unit>, reason: &str) -> Result<(), Error> {
        let abort = Abort {
            rows: self.observations.len() as u64,
            at,
            reason: reason.replace(['\n', '\r', '\t'], " "),
            content: self.journal.digest(),
        };
        self.journal
            .seal(&Line {
                kind: "abort",
                value: &abort,
            })
            .map_err(|_| Error::Io)
    }
}

pub fn load_str<S: Schema>(input: &str) -> Result<Study<S>, Error> {
    if !input.ends_with('\n') {
        return Err(Error::Incomplete);
    }
    let mut lines = input.split_inclusive('\n');
    let header_raw = lines.next().ok_or(Error::Incomplete)?;
    let raw: RawLine = serde_json::from_str(header_raw.trim_end()).map_err(|_| Error::Syntax)?;
    if raw.kind != "header" {
        return Err(Error::Syntax);
    }
    let probe: HeaderProbe =
        serde_json::from_value(raw.value.clone()).map_err(|_| Error::Syntax)?;
    if probe.schema != S::id() {
        return Err(Error::Schema);
    }
    let header: Header<S::Specification, S::Case> =
        serde_json::from_value(raw.value).map_err(|_| Error::Syntax)?;
    S::validate_header(&header).map_err(|_| Error::Invalid)?;
    validate_schedule(&header)?;
    let mut digest = blake3::Hasher::new();
    digest.update(header_raw.as_bytes());
    let mut observations = Vec::new();
    let mut terminal = None;
    for encoded in lines {
        let raw: RawLine = serde_json::from_str(encoded.trim_end()).map_err(|_| Error::Syntax)?;
        match raw.kind.as_str() {
            "observation" if terminal.is_none() => {
                let observation = serde_json::from_value(raw.value).map_err(|_| Error::Syntax)?;
                observations.push(observation);
                digest.update(encoded.as_bytes());
            }
            "complete" if terminal.is_none() => {
                terminal = Some(Ok(
                    serde_json::from_value::<Complete>(raw.value).map_err(|_| Error::Syntax)?
                ));
            }
            "abort" if terminal.is_none() => {
                let _: Abort = serde_json::from_value(raw.value).map_err(|_| Error::Syntax)?;
                terminal = Some(Err(Error::Aborted));
            }
            _ => return Err(Error::Syntax),
        }
    }
    let complete = terminal.ok_or(Error::Incomplete)??;
    check::<S>(&header, &observations)?;
    if complete.rows != observations.len() as u64
        || complete.schedule != schedule_digest(&header.run.schedule)
        || complete.content != digest.finalize().to_hex().as_str()
    {
        return Err(Error::Digest);
    }
    Ok(Study {
        header,
        observations,
        complete,
        marker: PhantomData,
    })
}

pub fn load<S: Schema>(path: &Path) -> Result<Study<S>, Error> {
    let input = std::fs::read_to_string(path).map_err(|_| Error::Io)?;
    load_str::<S>(&input)
}
