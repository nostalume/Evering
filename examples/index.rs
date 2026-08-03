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
type Result<T> = std::result::Result<T, String>;

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

fn encode_boot(worker: usize, pool: PoolId, job: &Port<Encoded>, out: &Port<Encoded>) -> Bootstrap {
    let mut bytes = Vec::with_capacity(24 + PoolId::BYTE_LEN + 2 * Port::<Encoded>::BYTE_LEN);
    for value in [BOOT_MAGIC, worker as u64, EXTENT as u64] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(&pool.to_bytes());
    bytes.extend_from_slice(&job.to_bytes());
    bytes.extend_from_slice(&out.to_bytes());
    Bootstrap::new(bytes).unwrap()
}

fn decode_boot(
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

fn job(id: u64, path: &Path) -> Result<Vec<u8>> {
    let path = path.to_str().ok_or("path is not UTF-8")?;
    let mut body = id.to_le_bytes().to_vec();
    body.extend_from_slice(path.as_bytes());
    Ok(body)
}

fn read_job(body: &[u8]) -> Result<(u64, PathBuf)> {
    if body.len() < 8 {
        return Err("short job".into());
    }
    let id = u64::from_le_bytes(body[..8].try_into().unwrap());
    let path = std::str::from_utf8(&body[8..]).map_err(text)?;
    Ok((id, PathBuf::from(path)))
}

fn encode_outcome(id: u64, index: &Index) -> Vec<u8> {
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

fn read_outcome(body: &[u8]) -> Result<(usize, Index)> {
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
    event: &Event,
    ring: &Ring,
) -> Result<Supervisor> {
    let handoff = evering::os::Handoff::bind().map_err(text)?;
    let mut command = Command::new(std::env::current_exe().map_err(text)?);
    command
        .arg("--worker")
        .arg(handoff.address())
        .arg(index.to_string());
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

fn attach(address: &str) -> Result<(Bootstrap, Session, Event, Ring)> {
    let resources = evering::os::Handoff::receive(address).map_err(text)?;
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
    let (notify, worker_event) = evering::os::event().map_err(text)?;
    let (worker_ring, event) = evering::os::event().map_err(text)?;
    let wait = Wait::new(event).map_err(text)?;
    let mut peers = Vec::new();
    for index in 0..WORKERS {
        let (jobs, job_port) = session.create_channel(4).map_err(text)?;
        let (results, result_port) = session.create_channel(4).map_err(text)?;
        let (jobs_tx, jobs_rx) = jobs.split();
        let (results_tx, results_rx) = results.split();
        drop((jobs_rx, results_tx));
        let boot = encode_boot(index, pool.id(), &job_port, &result_port);
        peers.push(Peer {
            jobs: jobs_tx,
            results: results_rx,
            channels: [jobs, results],
            child: spawn(index, &boot, &shared, &worker_event, &worker_ring)?,
        });
    }
    let signals = Signals::new(&notify, &wait);
    let window = WORKERS * 2;
    let mut output: Vec<Option<Index>> = (0..paths.len()).map(|_| None).collect();
    let mut sent = 0;
    let mut received = 0;
    while sent < paths.len() || received < sent {
        if sent < paths.len() && sent - received < window {
            let peer = &peers[sent % peers.len()];
            let permit = signals.reserve(&peer.jobs).await.map_err(text)?;
            let body = job(sent as u64, &paths[sent])?;
            let block = pool.as_ref().copy(&body).map_err(text)?;
            permit
                .send(block.encode(JOB))
                .into_parts()
                .1
                .map_err(text)?;
            sent += 1;
            continue;
        }
        let mut progress = false;
        for peer in &peers {
            if let Ok(claim) = peer.results.claim() {
                let committed = signals
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
            wait.wait().await.map_err(text)?;
        }
    }
    for peer in &peers {
        signals.close_tx(&peer.jobs).into_parts().1.map_err(text)?;
    }
    for peer in &mut peers {
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

async fn worker(address: &str, index: usize) -> Result<()> {
    let (boot, session, event, ring) = attach(address)?;
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
        let index = args[2].parse().expect("numeric worker index");
        runtime
            .block_on(worker(&args[1], index))
            .map(|_| Vec::new())
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
