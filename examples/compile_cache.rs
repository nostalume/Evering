use std::{
    collections::HashMap,
    fmt::Display,
    process::Command,
    time::{Duration, Instant},
};

use evering::{
    BlockRange, Channel, Pool, PoolCreateError, PoolId, Port, RemoveError, Session,
    layout::{self, RegionId, Repr, SchemaKey, compose_schema, schema_id},
    mapping::{Access, Peer, REGION_ALIGN, Request},
    notify::Signals,
    os::{Handoff, Shared, event},
    process::{Bootstrap, Supervisor},
    runtime::Wait,
};

const REGION: RegionId = RegionId::new(0x4556_4552_4341_4348, 1);
/// A `BlockRange` is exact, and an exact range reserves its floor before any payload arrives: 64
/// slots of every power-of-two class up to `LARGEST`, which is 256 MiB of Pool for a 1 MiB class.
/// `compile_cache --geometry` prints the floor each range demands instead of trusting this number.
const POOL_BYTES: usize = 256 * LARGEST;
const EXTENT: usize = POOL_BYTES + 16 * 1024 * 1024;
/// The widest class a peer's mapping is guaranteed to reproduce is the arm's `REGION_ALIGN`, so the
/// largest object comes from the platform rather than from a number that only holds on one arm.
const LARGEST: usize = if REGION_ALIGN < 1 << 20 {
    REGION_ALIGN
} else {
    1 << 20
};
const BOOT_MAGIC: u64 = 0x4556_4552_4341_4301;
const TIMEOUT: Duration = Duration::from_secs(10);
/// How long the daemon waits for the next request before it declares the handover dead and reports
/// which half of the round trip went missing. A stall has to be distinguishable from idle.
const STALL: Duration = Duration::from_secs(5);
const DYING_CODE: i32 = 7;
const JOBS: [usize; 3] = [4 << 10, LARGEST, 4 << 10];
const SEEDS: [u64; 3] = [7, 11, 7];
/// The first pass pays whatever the region and the socket buffers cost to arrive at; the passes
/// after it are the steady state an integration actually lives in.
const PASSES: usize = 3;
/// Both halves of the round trip are one protocol, so their keys come from one domain name and
/// differ only by the part they describe. Derived rather than typed in, because an integer a caller
/// chose cannot show that it does not collide with another crate's.
const PROTOCOL: SchemaKey = SchemaKey::new(schema_id("evering.example.compile-cache"), 1);

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Job {
    digest: u64,
    len: u64,
}

impl evering::layout::SharedSchema for Job {
    const SCHEMA: SchemaKey = compose_schema(PROTOCOL, SchemaKey::new(schema_id("job"), 1));
}
unsafe impl Repr for Job {}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Verdict {
    digest: u64,
    cached: u64,
    len: u64,
}

impl evering::layout::SharedSchema for Verdict {
    const SCHEMA: SchemaKey = compose_schema(PROTOCOL, SchemaKey::new(schema_id("verdict"), 1));
}
unsafe impl Repr for Verdict {}

fn access() -> Access {
    Access::READ | Access::WRITE
}

fn text(error: impl Display) -> String {
    error.to_string()
}

fn object(seed: u64, len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| {
            let mixed = (index as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ seed;
            (mixed >> ((mixed >> 3) & 56)) as u8
        })
        .collect()
}

fn digest(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |sum, byte| {
        (sum ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    })
}

fn encode_boot(pool: PoolId, jobs: &Port<Job>, verdicts: &Port<Verdict>) -> Bootstrap {
    let mut bytes = Vec::with_capacity(8 + PoolId::BYTE_LEN + 2 * Port::<Job>::BYTE_LEN);
    bytes.extend_from_slice(&BOOT_MAGIC.to_le_bytes());
    bytes.extend_from_slice(&pool.to_bytes());
    bytes.extend_from_slice(&jobs.to_bytes());
    bytes.extend_from_slice(&verdicts.to_bytes());
    Bootstrap::new(bytes).unwrap()
}

fn decode_boot(boot: &Bootstrap) -> Result<(PoolId, Port<Job>, Port<Verdict>), String> {
    let bytes = boot.as_ref();
    let pool = 8;
    let jobs = pool + PoolId::BYTE_LEN;
    let verdicts = jobs + Port::<Job>::BYTE_LEN;
    let end = verdicts + Port::<Verdict>::BYTE_LEN;
    if bytes.len() != end || u64::from_le_bytes(bytes[..8].try_into().unwrap()) != BOOT_MAGIC {
        return Err("bootstrap identity mismatch".into());
    }
    Ok((
        PoolId::from_bytes(&bytes[pool..jobs]).ok_or("invalid pool")?,
        Port::from_bytes(&bytes[jobs..verdicts]).ok_or("invalid job port")?,
        Port::from_bytes(&bytes[verdicts..end]).ok_or("invalid verdict port")?,
    ))
}

fn open_region() -> Result<(Session, Shared, Pool), String> {
    let shared = evering::os::shared("evering-compile-cache", EXTENT, access()).map_err(text)?;
    let session =
        Session::create(shared.borrow(), Request::new(EXTENT, access()), REGION).map_err(text)?;
    let classes = BlockRange::new(64, LARGEST).map_err(text)?;
    let pool = session
        .create_pool(POOL_BYTES, Some(classes))
        .map_err(text)?;
    Ok((session, shared, pool))
}

fn spawn_serving(
    role: &str,
    boot: &Bootstrap,
    shared: &Shared,
    wakes: &evering::os::Event,
    rings: &evering::os::Ring,
) -> Result<Supervisor, String> {
    let mut command = Command::new(std::env::current_exe().map_err(text)?);
    command.arg(role);
    let (handoff, grant) = Handoff::bind().map_err(text)?;
    grant.admit(&mut command).map_err(text)?;
    let child = Supervisor::spawn(&mut command).map_err(text)?;
    handoff
        .send(&child, Instant::now() + TIMEOUT, boot, shared, wakes, rings)
        .map_err(text)?;
    Ok(child)
}

async fn serve(dying: bool) -> Result<(), String> {
    let (_provenance, resources, _report) = Handoff::receive().map_err(text)?;
    let session =
        Session::open(resources.mapping, Request::new(EXTENT, access()), REGION).map_err(text)?;
    let (pool_id, jobs_port, verdicts_port) = decode_boot(&resources.bootstrap)?;
    let pool = session.open_pool(pool_id).map_err(text)?;
    let jobs = session.adopt(jobs_port).map_err(text)?;
    let verdicts = session.adopt(verdicts_port).map_err(text)?;
    let (jobs_tx, jobs_rx) = jobs.split();
    let (verdicts_tx, _) = verdicts.split();
    drop((jobs, verdicts));
    let wait = Wait::new(resources.event).map_err(text)?;
    let signals = Signals::new(&resources.ring, &wait);

    if dying {
        let me = session.peer();
        let block = pool.as_ref().copy(&me.slot().to_le_bytes()).map_err(text)?;
        let announced = Job {
            digest: u64::from(me.slot()),
            len: me.generation() as u64,
        };
        match signals.try_send(&jobs_tx, block.transfer(announced)) {
            Ok(committed) => {
                committed.into_parts().1.map_err(text)?;
            }
            Err(error) => return Err(text(error)),
        }
        std::process::exit(DYING_CODE);
    }

    let mut store: HashMap<u64, Vec<u8>> = HashMap::new();
    loop {
        let claim = match tokio::time::timeout(STALL, signals.claim(&jobs_rx)).await {
            Ok(Ok(claim)) => claim,
            Ok(Err(evering::notify::ProgressError::Closed)) => break,
            Ok(Err(error)) => return Err(text(error)),
            Err(_) => {
                let queued = match jobs_rx.claim() {
                    Ok(_) => "a job was already published",
                    Err(_) => "nothing was published",
                };
                return Err(format!("the daemon stalled waiting for a job: {queued}"));
            }
        };
        let (job, archive) = claim.adopt::<[u8]>(pool.as_ref()).map_err(text)?;
        if archive.len() as u64 != job.len || digest(&archive) != job.digest {
            return Err("admitted archive does not match its header".into());
        }
        let (verdict, body) = match store.get(&job.digest) {
            Some(cached) => (
                Verdict {
                    digest: job.digest,
                    cached: 1,
                    len: cached.len() as u64,
                },
                pool.as_ref().copy(cached).map_err(text)?,
            ),
            None => {
                store.insert(job.digest, archive.to_vec());
                (
                    Verdict {
                        digest: job.digest,
                        cached: 0,
                        len: 8,
                    },
                    pool.as_ref()
                        .copy(&job.digest.to_le_bytes())
                        .map_err(text)?,
                )
            }
        };
        match signals.try_send(&verdicts_tx, body.transfer(verdict)) {
            Ok(committed) => {
                committed.into_parts().1.map_err(text)?;
            }
            Err(error) => return Err(text(error)),
        }
        eprintln!(
            "serve\t{}\t{}\t{}",
            job.digest,
            if verdict.cached == 1 { "hit" } else { "stored" },
            job.len
        );
    }
    Ok(())
}

fn retire<H: Repr>(session: &Session, channel: Channel<H>) -> Result<(), String> {
    let mut channel = channel;
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match session.remove(channel) {
            Ok(()) => return Ok(()),
            Err(RemoveError::Busy(returned) | RemoveError::Evidence(returned)) => {
                channel = returned;
            }
        }
        if Instant::now() >= deadline {
            return Err("channel retirement never settled".into());
        }
        std::thread::yield_now();
    }
}

/// The same round trips over the transport sccache uses today: loopback TCP, one heap buffer and a
/// length-prefixed frame per compile unit. The server is a thread rather than a second process, which
/// is the one asymmetry against the shared-memory side below — that one crosses a real `exec`.
fn traffic_tcp() -> Result<(), String> {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(text)?;
    let address = listener.local_addr().map_err(text)?;
    let server = std::thread::spawn(move || {
        let mut store: HashMap<u64, Vec<u8>> = HashMap::new();
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut head = [0u8; 16];
        while stream.read_exact(&mut head).is_ok() {
            let len = u64::from_le_bytes(head[..8].try_into().unwrap()) as usize;
            let key = u64::from_le_bytes(head[8..].try_into().unwrap());
            let mut body = vec![0u8; len];
            if stream.read_exact(&mut body).is_err() || digest(&body) != key {
                return;
            }
            let mut reply = Vec::with_capacity(17);
            reply.extend_from_slice(&key.to_le_bytes());
            match store.get(&key) {
                Some(cached) => {
                    reply.push(1);
                    reply.extend_from_slice(&(cached.len() as u64).to_le_bytes());
                    reply.extend_from_slice(cached);
                }
                None => {
                    reply.push(0);
                    reply.extend_from_slice(&8u64.to_le_bytes());
                    reply.extend_from_slice(&key.to_le_bytes());
                    store.insert(key, body);
                }
            }
            if stream.write_all(&reply).is_err() {
                return;
            }
        }
    });

    let opening = Instant::now();
    let mut stream = loop {
        match TcpStream::connect(address) {
            Ok(stream) => break stream,
            Err(error) if opening.elapsed() > TIMEOUT => return Err(text(error)),
            Err(_) => std::thread::sleep(Duration::from_millis(1)),
        }
    };

    for pass in 0..PASSES {
        for (index, (len, seed)) in JOBS.iter().zip(SEEDS).enumerate() {
            let body = object(seed, *len);
            let key = digest(&body);
            let started = Instant::now();
            let mut request = Vec::with_capacity(16 + body.len());
            request.extend_from_slice(&(body.len() as u64).to_le_bytes());
            request.extend_from_slice(&key.to_le_bytes());
            request.extend_from_slice(&body);
            stream.write_all(&request).map_err(text)?;
            stream.flush().map_err(text)?;
            let mut head = [0u8; 17];
            stream.read_exact(&mut head).map_err(text)?;
            let cached = head[8] == 1;
            let reply = u64::from_le_bytes(head[9..17].try_into().unwrap()) as usize;
            let mut body = vec![0u8; reply];
            stream.read_exact(&mut body).map_err(text)?;
            if cached && digest(&body) != key {
                return Err("cached archive does not match its key".into());
            }
            if !cached && body != key.to_le_bytes() {
                return Err("stored echo does not match its key".into());
            }
            println!(
                "TCPRTT\t{pass}:{index}\t{len}\t{}\t{}",
                if cached { "hit" } else { "stored" },
                started.elapsed().as_nanos()
            );
        }
    }
    drop(stream);
    server
        .join()
        .map_err(|_| "the tcp server panicked".to_owned())
}

async fn traffic(session: &Session, pool: &Pool, shared: &Shared) -> Result<(), String> {
    let (jobs, jobs_port) = session.create_channel::<Job>(4).map_err(text)?;
    let (verdicts, verdicts_port) = session.create_channel::<Verdict>(4).map_err(text)?;
    let (jobs_tx, _) = jobs.split();
    let (_, verdicts_rx) = verdicts.split();
    let (to_daemon, daemon_wakes) = event().map_err(text)?;
    let (daemon_rings, my_wakes) = event().map_err(text)?;
    let boot = encode_boot(pool.id(), &jobs_port, &verdicts_port);
    let mut daemon = spawn_serving("--serve", &boot, shared, &daemon_wakes, &daemon_rings)?;
    let wait = Wait::new(my_wakes).map_err(text)?;
    let signals = Signals::new(&to_daemon, &wait);

    for pass in 0..PASSES {
        for (index, (len, seed)) in JOBS.iter().zip(SEEDS).enumerate() {
            let body = object(seed, *len);
            let job = Job {
                digest: digest(&body),
                len: *len as u64,
            };
            let started = Instant::now();
            let block = pool.as_ref().copy(&body).map_err(text)?;
            match signals.try_send(&jobs_tx, block.transfer(job)) {
                Ok(committed) => {
                    committed.into_parts().1.map_err(text)?;
                }
                Err(error) => return Err(text(error)),
            }
            let claim = match tokio::time::timeout(TIMEOUT, signals.claim(&verdicts_rx)).await {
                Ok(claim) => claim.map_err(text)?,
                Err(_) => {
                    // Which half stalled is the whole diagnosis: a verdict sitting in the queue means
                    // the doorbell was lost, an empty queue means the daemon never produced one.
                    return Err(match verdicts_rx.claim() {
                        Ok(_) => "stalled with a verdict already published".into(),
                        Err(error) => format!("stalled with nothing published: {}", text(error)),
                    });
                }
            };
            let (verdict, cached) = claim.adopt::<[u8]>(pool.as_ref()).map_err(text)?;
            if verdict.cached == 1
                && (cached.len() as u64 != job.len || digest(&cached) != job.digest)
            {
                return Err("cached archive does not match its key".into());
            }
            if verdict.cached != 1 && cached.as_ref() != job.digest.to_le_bytes().as_slice() {
                return Err("stored echo does not match its key".into());
            }
            println!(
                "ROUNDTRIP\t{pass}:{index}\t{}\t{}\t{}",
                job.len,
                if verdict.cached == 1 { "hit" } else { "stored" },
                started.elapsed().as_nanos()
            );
        }
    }

    signals.close_tx(&jobs_tx).into_parts().1.map_err(text)?;
    if !daemon.wait().map_err(text)?.success() {
        return Err("the daemon exited unsuccessfully".into());
    }
    drop((jobs_tx, verdicts_rx));
    retire(session, jobs)?;
    retire(session, verdicts)
}

async fn liveness(session: &Session, pool: &Pool, shared: &Shared) -> Result<(), String> {
    let (jobs, jobs_port) = session.create_channel::<Job>(2).map_err(text)?;
    let (verdicts, verdicts_port) = session.create_channel::<Verdict>(2).map_err(text)?;
    let (_, jobs_rx) = jobs.split();
    let boot = encode_boot(pool.id(), &jobs_port, &verdicts_port);
    let (to_child, child_wakes) = event().map_err(text)?;
    let (child_rings, my_wakes) = event().map_err(text)?;
    let mut child = spawn_serving("--dying", &boot, shared, &child_wakes, &child_rings)?;
    let pid = child.id();
    let wait = Wait::new(my_wakes).map_err(text)?;

    // The death is waited on rather than noticed. A witness parks this task on the child itself, so the
    // round no longer re-arms a timer to find out whether the peer is gone, and the answer is not a poll
    // that happened to land after the exit. The latch stays in the race because this peer rings before it
    // exits: a wake that arrives first is the announcement arriving, not a reason to stop waiting.
    let started = Instant::now();
    #[cfg(target_os = "linux")]
    let (status, woken) = {
        use evering::notify::Wait as _;
        let death = child.exited().map_err(text)?;
        let mut woken = 0;
        let status = loop {
            let Some(remaining) = TIMEOUT.checked_sub(started.elapsed()) else {
                return Err("a daemon that never served was never observed exiting".into());
            };
            tokio::select! {
                ready = wait.wait() => {
                    ready.map_err(text)?;
                    woken += 1;
                }
                gone = death.wait() => {
                    gone.map_err(text)?;
                    // The witness says the child is gone; only the owner can say what it returned, and
                    // taking the status is what keeps the reap on the lane that owes it.
                    break child.wait().map_err(text)?;
                }
                _ = tokio::time::sleep(remaining) => {
                    return Err("a daemon that never served was never observed exiting".into());
                }
            }
        };
        (status, woken)
    };
    #[cfg(not(target_os = "linux"))]
    let (status, woken) = {
        // No death witness is written for this target, and `Supervisor::exited` names the reap as the
        // way its child's end is reached, so this round ends there and reports nothing parked on.
        (child.wait().map_err(text)?, 0)
    };
    let detected = started.elapsed();
    let code = status.status().code().unwrap_or_default();

    let inbound = Signals::new(&to_child, &wait);
    let claim = match tokio::time::timeout(TIMEOUT, inbound.claim(&jobs_rx)).await {
        Ok(claim) => claim.map_err(text)?,
        Err(_) => return Err("the dead peer published nothing it left behind".into()),
    };
    let (announced, _) = claim.adopt::<[u8]>(pool.as_ref()).map_err(text)?;
    let dead = Peer::from_parts(
        u8::try_from(announced.digest).map_err(text)?,
        announced.len as usize,
    );
    let recovery = unsafe { session.assume_dead(dead) }.ok_or("no such member generation")?;
    let reclaimed = recovery.reap_with(&[
        layout::recovery_handler::<Job>(),
        layout::recovery_handler::<Verdict>(),
    ]);
    println!(
        "LIVENESS\tpid={pid}\texit={code}\tdetect_ns={}\twakes={woken}\treap={}",
        detected.as_nanos(),
        reclaimed.is_ok()
    );
    // The line above is the measurement; this one is the leg. A daemon died and its storage is the whole
    // claim, so a reclaim that did not settle has to end the run rather than be reported beside a success.
    if reclaimed.is_err() {
        return Err("the dead peer's storage was never reclaimed".into());
    }
    drop((jobs_rx, jobs_port, verdicts_port));
    retire(session, jobs)?;
    retire(session, verdicts)
}

fn geometry() -> Result<(), String> {
    for (min, max) in [
        (64, 1 << 20),
        (64, 1 << 15),
        (64, 4 << 10),
        (64, 64),
        (4 << 10, 1 << 20),
    ] {
        let range = BlockRange::new(min, max).map_err(text)?;
        let floor = match range.required_extent() {
            Ok(required) => format!("{required}"),
            Err(PoolCreateError::ClassTooWide(bytes)) => format!("refused-class-too-wide-{bytes}"),
            Err(other) => return Err(text(other)),
        };
        println!("GEOMETRY\t{min}\t{max}\t{floor}");
    }
    Ok(())
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let served = match args.first().map(String::as_str) {
        Some("--serve") => runtime.block_on(serve(false)),
        Some("--dying") => runtime.block_on(serve(true)),
        Some("--geometry") => geometry(),
        _ => runtime.block_on(async {
            traffic_tcp()?;
            let (session, shared, pool) = open_region()?;
            println!(
                "EVERING\tcompile-cache\textent={EXTENT}\tpool={POOL_BYTES}\tclass_max={LARGEST}"
            );
            traffic(&session, &pool, &shared).await?;
            liveness(&session, &pool, &shared).await
        }),
    };
    if let Err(error) = served {
        eprintln!("compile_cache: {error}");
        std::process::exit(1);
    }
}
