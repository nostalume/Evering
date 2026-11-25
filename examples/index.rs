use std::{
    fmt::Debug,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use evering::{
    Channel, Encoded, PoolId, Port, Session, Tx,
    layout::{RegionId, SchemaId, SchemaKey},
    mapping::{Access, Request},
    notify::{Signals, Wait as _},
    os::{Event, Ring},
    process::{Bootstrap, Supervisor},
    runtime::Wait,
};

const REGION: RegionId = RegionId::new(0x4556_4552_494e_4458, 1);
const JOB: SchemaKey = SchemaKey::new(SchemaId(0x494e_4445_585f_4a4f), 1);
const OUTCOME: SchemaKey = SchemaKey::new(SchemaId(0x494e_4458_5f4f_5554), 1);
const EXTENT: usize = 16 * 1024 * 1024;
const WORKERS: usize = 2;
const BOOT_MAGIC: u64 = 0x4556_494e_4458_0001;
/// How long the coordinator lets a job stay outstanding. A queue reports contention and closure and never
/// silence, so a worker that died before answering leaves the peer-bound park with no cause to end on:
/// the run has to carry the bound, which is what an integration holding both roles is told to do. Where the
/// platform has a death witness the same clock bounds the reap too, because closing a channel is still a
/// wake the worker has to be given.
const BUDGET: Duration = Duration::from_secs(30);
const STUCK: &str =
    "a job stayed outstanding past the run's budget; the workers' own lines name why";
/// Only the arm with a death witness can report an unreaped worker: the others have no wake to bound a wait
/// on, so their reap runs as long as the child does and nothing here is left to name.
#[cfg(target_os = "linux")]
const UNREAPED: &str =
    "a worker outlived the run's budget with its channels closed; its own lines name why";
pub(crate) type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Index {
    pub bytes: u64,
    pub lines: u64,
    pub checksum: u64,
    pub error: Option<String>,
}

pub fn inspect(path: &Path) -> Index {
    match inspect_inner(path) {
        Ok(index) => index,
        Err(error) => Index {
            error: Some(format!("{}: {error}", path.display())),
            ..Index::default()
        },
    }
}

fn inspect_inner(path: &Path) -> std::io::Result<Index> {
    let mut file = File::open(path)?;
    let mut buffer = [0; 64 * 1024];
    let mut index = Index {
        checksum: 0xcbf2_9ce4_8422_2325,
        ..Index::default()
    };
    let mut last = None;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        index.bytes += read as u64;
        for &byte in &buffer[..read] {
            index.lines += u64::from(byte == b'\n');
            index.checksum = (index.checksum ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
            last = Some(byte);
        }
    }
    index.lines += u64::from(last.is_some_and(|byte| byte != b'\n'));
    Ok(index)
}

fn text(error: impl Debug) -> String {
    format!("{error:?}")
}

pub(crate) fn encode_boot(
    worker: usize,
    pool: PoolId,
    job: &Port<Encoded>,
    out: &Port<Encoded>,
) -> Bootstrap {
    let mut bytes = Vec::with_capacity(24 + PoolId::BYTE_LEN + 2 * Port::<Encoded>::BYTE_LEN);
    for value in [BOOT_MAGIC, worker as u64, EXTENT as u64] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(&pool.to_bytes());
    bytes.extend_from_slice(&job.to_bytes());
    bytes.extend_from_slice(&out.to_bytes());
    Bootstrap::new(bytes).unwrap()
}

pub(crate) fn decode_boot(
    boot: &Bootstrap,
    expected: usize,
) -> Result<(PoolId, Port<Encoded>, Port<Encoded>)> {
    let bytes = boot.as_ref();
    let pool = 24;
    let job = pool + PoolId::BYTE_LEN;
    let out = job + Port::<Encoded>::BYTE_LEN;
    let read = |offset| u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
    if bytes.len() != out + Port::<Encoded>::BYTE_LEN
        || read(0) != BOOT_MAGIC
        || read(8) != expected as u64
        || read(16) != EXTENT as u64
    {
        return Err("bootstrap identity mismatch".into());
    }
    Ok((
        PoolId::from_bytes(&bytes[pool..job]).ok_or("invalid pool")?,
        Port::from_bytes(&bytes[job..out]).ok_or("invalid job port")?,
        Port::from_bytes(&bytes[out..]).ok_or("invalid result port")?,
    ))
}

pub(crate) fn job(id: u64, path: &Path) -> Result<Vec<u8>> {
    let path = path.to_str().ok_or("path is not UTF-8")?;
    let mut body = id.to_le_bytes().to_vec();
    body.extend_from_slice(path.as_bytes());
    Ok(body)
}

pub(crate) fn read_job(body: &[u8]) -> Result<(u64, PathBuf)> {
    if body.len() < 8 {
        return Err("short job".into());
    }
    let id = u64::from_le_bytes(body[..8].try_into().unwrap());
    let path = std::str::from_utf8(&body[8..]).map_err(text)?;
    Ok((id, PathBuf::from(path)))
}

pub(crate) fn encode_outcome(id: u64, index: &Index) -> Vec<u8> {
    let mut body = Vec::with_capacity(33 + index.error.as_ref().map_or(0, String::len));
    for value in [id, index.bytes, index.lines, index.checksum] {
        body.extend_from_slice(&value.to_le_bytes());
    }
    body.push(u8::from(index.error.is_some()));
    if let Some(error) = &index.error {
        body.extend_from_slice(error.as_bytes());
    }
    body
}

pub(crate) fn read_outcome(body: &[u8]) -> Result<(usize, Index)> {
    if body.len() < 33 {
        return Err("short outcome".into());
    }
    let number = |offset| u64::from_le_bytes(body[offset..offset + 8].try_into().unwrap());
    let error = (body[32] != 0)
        .then(|| {
            std::str::from_utf8(&body[33..])
                .map(str::to_owned)
                .map_err(text)
        })
        .transpose()?;
    Ok((
        usize::try_from(number(0)).map_err(text)?,
        Index {
            bytes: number(8),
            lines: number(16),
            checksum: number(24),
            error,
        },
    ))
}

fn create() -> Result<(Session, evering::os::Shared)> {
    let access = Access::READ | Access::WRITE;
    let shared = evering::os::shared("evering-index", EXTENT, access).map_err(text)?;
    let session =
        Session::create(shared.borrow(), Request::new(EXTENT, access), REGION).map_err(text)?;
    Ok((session, shared))
}

fn spawn(
    index: usize,
    boot: &Bootstrap,
    shared: &evering::os::Shared,
    event: Event,
    ring: &Ring,
) -> Result<Supervisor> {
    let (handoff, grant) = evering::os::Handoff::bind().map_err(text)?;
    let mut command = Command::new(std::env::current_exe().map_err(text)?);
    command.arg("--worker").arg(index.to_string());
    grant.admit(&mut command).map_err(text)?;
    let child = Supervisor::spawn(&mut command).map_err(text)?;
    handoff
        .send(
            &child,
            Instant::now() + Duration::from_secs(5),
            boot,
            shared,
            event,
            ring,
        )
        .map_err(text)?;
    Ok(child)
}

fn attach() -> Result<(Bootstrap, Session, Event, Ring)> {
    let (_provenance, resources, _report) = evering::os::Handoff::receive().map_err(text)?;
    let session = Session::open(
        resources.mapping,
        Request::new(EXTENT, Access::READ | Access::WRITE),
        REGION,
    )
    .map_err(text)?;
    Ok((
        resources.bootstrap,
        session,
        resources.event,
        resources.ring,
    ))
}

struct Peer {
    jobs: Tx<Encoded>,
    results: evering::Rx<Encoded>,
    channels: [Channel<Encoded>; 2],
    child: Supervisor,
}

async fn coordinator(paths: Vec<PathBuf>) -> Result<Vec<Index>> {
    let (session, shared) = create()?;
    let pool = session.create_pool(8 * 1024 * 1024, None).map_err(text)?;
    let (worker_ring, event) = evering::os::event().map_err(text)?;
    let wait = Wait::new(event).map_err(text)?;
    let mut rings = Vec::new();
    let mut peers = Vec::new();
    for index in 0..WORKERS {
        let (jobs, job_port) = session.create_channel(4).map_err(text)?;
        let (results, result_port) = session.create_channel(4).map_err(text)?;
        let (jobs_tx, jobs_rx) = jobs.split();
        let (results_tx, results_rx) = results.split();
        drop((jobs_rx, results_tx));
        let boot = encode_boot(index, pool.id(), &job_port, &result_port);
        // One doorbell per worker, because the reading end moves into the child and a latch has exactly
        // one consuming owner: two peers parked on one pair would each clear the other's readiness, and a
        // wake spent before a process parks is never re-reported. The workers' ring back is one pair for
        // the whole set, since many writers on a single reader is what a coalescing latch is for.
        let (notify, worker_event) = evering::os::event().map_err(text)?;
        let child = spawn(index, &boot, &shared, worker_event, &worker_ring)?;
        rings.push(notify);
        peers.push(Peer {
            jobs: jobs_tx,
            results: results_rx,
            channels: [jobs, results],
            child,
        });
    }
    let window = WORKERS * 2;
    let mut output: Vec<Option<Index>> = (0..paths.len()).map(|_| None).collect();
    let mut sent = 0;
    let mut received = 0;
    let started = Instant::now();
    let remaining = || {
        BUDGET
            .checked_sub(started.elapsed())
            .ok_or_else(|| STUCK.to_owned())
    };
    while sent < paths.len() || received < sent {
        if sent < paths.len() && sent - received < window {
            let slot = sent % peers.len();
            let peer = &peers[slot];
            let signals = Signals::new(&rings[slot], &wait);
            let permit = tokio::time::timeout(remaining()?, signals.reserve(&peer.jobs))
                .await
                .map_err(|_| STUCK.to_owned())?
                .map_err(text)?;
            let body = job(sent as u64, &paths[sent])?;
            let block = pool.as_ref().copy(&body).map_err(text)?;
            permit
                .send(block.encode(JOB))
                .map_err(text)?
                .into_parts()
                .1
                .map_err(text)?;
            sent += 1;
            continue;
        }
        let mut progress = false;
        for (slot, peer) in peers.iter().enumerate() {
            if let Ok(claim) = peer.results.claim() {
                let committed = Signals::new(&rings[slot], &wait)
                    .admit::<[u8]>(claim, pool.as_ref(), OUTCOME)
                    .map_err(text)?;
                let (block, notified) = committed.into_parts();
                notified.map_err(text)?;
                let (id, index) = read_outcome(&block)?;
                if id >= output.len() || output[id].replace(index).is_some() {
                    return Err("duplicate outcome".into());
                }
                received += 1;
                progress = true;
            }
        }
        if !progress {
            tokio::time::timeout(remaining()?, wait.wait())
                .await
                .map_err(|_| STUCK.to_owned())?
                .map_err(text)?;
        }
    }
    for (slot, peer) in peers.iter().enumerate() {
        Signals::new(&rings[slot], &wait)
            .close_tx(&peer.jobs)
            .into_parts()
            .1
            .map_err(text)?;
    }
    for peer in &mut peers {
        // The reap is the one step with no queue left to report contention or closure on, so the run's
        // bound has to reach here too: a worker parked on a wake that was spent elsewhere would turn a
        // budgeted run into a parent that waits forever. Linux parks this task on the child itself; the
        // arms with no death witness wait on the reap, which is what `compile_cache` does for the same
        // reason.
        #[cfg(target_os = "linux")]
        {
            let death = peer.child.exited().map_err(text)?;
            if tokio::time::timeout(remaining()?, death.wait())
                .await
                .is_err()
            {
                return Err(UNREAPED.to_owned());
            }
        }
        if !peer.child.wait().map_err(text)?.success() {
            return Err("worker failed".into());
        }
    }
    for peer in peers {
        drop((peer.jobs, peer.results));
        for channel in peer.channels {
            session.remove(channel).map_err(text)?;
        }
    }
    output
        .into_iter()
        .map(|value| value.ok_or_else(|| "missing outcome".into()))
        .collect()
}

async fn worker(index: usize) -> Result<()> {
    let (boot, session, event, ring) = attach()?;
    let (pool_id, jobs, results) = decode_boot(&boot, index)?;
    let pool = session.open_pool(pool_id).map_err(text)?;
    let jobs = session.adopt(jobs).map_err(text)?;
    let results = session.adopt(results).map_err(text)?;
    let (jobs_tx, jobs_rx) = jobs.split();
    let (results_tx, results_rx) = results.split();
    drop((jobs_tx, results_rx));
    let wait = Wait::new(event).map_err(text)?;
    let signals = Signals::new(&ring, &wait);
    loop {
        let claim = match signals.claim(&jobs_rx).await {
            Ok(claim) => claim,
            Err(evering::notify::ProgressError::Closed) => break,
            Err(error) => return Err(text(error)),
        };
        let committed = signals
            .admit::<[u8]>(claim, pool.as_ref(), JOB)
            .map_err(text)?;
        let (block, notified) = committed.into_parts();
        notified.map_err(text)?;
        let (id, path) = read_job(&block)?;
        drop(block);
        let indexed = tokio::task::spawn_blocking(move || inspect(&path))
            .await
            .map_err(text)?;
        let permit = signals.reserve(&results_tx).await.map_err(text)?;
        let body = encode_outcome(id, &indexed);
        let block = pool.as_ref().copy(&body).map_err(text)?;
        permit
            .send(block.encode(OUTCOME))
            .map_err(text)?
            .into_parts()
            .1
            .map_err(text)?;
    }
    signals.close_tx(&results_tx).into_parts().1.map_err(text)?;
    Ok(())
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = if args.first().map(String::as_str) == Some("--worker") {
        let index = args[1].parse().expect("numeric worker index");
        runtime.block_on(worker(index)).map(|_| Vec::new())
    } else {
        let paths = std::env::args_os().skip(1).map(PathBuf::from).collect();
        runtime.block_on(coordinator(paths))
    };
    match result {
        Ok(indexes) => indexes.iter().enumerate().for_each(|(id, value)| {
            println!(
                "{id}\t{}\t{}\t{:016x}\t{}",
                value.bytes,
                value.lines,
                value.checksum,
                value.error.as_deref().unwrap_or("ok")
            )
        }),
        Err(error) => {
            eprintln!("index: {error}");
            std::process::exit(1);
        }
    }
}
