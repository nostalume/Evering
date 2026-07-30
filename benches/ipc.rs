#[path = "ipc/model.rs"]
mod model;
#[path = "ipc/stream.rs"]
mod stream;

use std::{
    env,
    path::Path,
    process::{Command, ExitCode},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use model::{
    Arm, Contrast, ContrastKey, Meta, Policy, Recorder, Scheduled, Status, Trial, load, schedule,
    schedule_id,
};

fn contrast(payload: u64, capacity: u64, in_flight: u64, candidate: Policy) -> Contrast {
    let working = payload.max(64) * capacity * 2;
    Contrast {
        key: ContrastKey {
            payload,
            capacity,
            in_flight,
            memory: (working + 4 * 1024 * 1024).next_multiple_of(4096),
        },
        candidate,
    }
}

fn contrasts() -> Vec<Contrast> {
    let mut result = Vec::with_capacity(23);
    for payload in [0, 64, 1024, 16 * 1024, 64 * 1024] {
        for policy in [Policy::Busy, Policy::Adaptive, Policy::Notified] {
            result.push(contrast(payload, 8, 8, policy));
        }
    }
    for capacity in [1, 256] {
        result.push(contrast(1024, capacity, 8, Policy::Notified));
    }
    for in_flight in [1, 64] {
        result.push(contrast(1024, 8, in_flight, Policy::Notified));
    }
    for (capacity, in_flight) in [(1, 1), (1, 64), (256, 1), (256, 64)] {
        result.push(contrast(1024, capacity, in_flight, Policy::Notified));
    }
    result
}

fn number<T: core::str::FromStr>(value: &str, name: &str) -> Result<T, String> {
    value.parse().map_err(|_| format!("invalid {name}"))
}

fn output(program: &str, arguments: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| format!("{program}: {error}"))?;
    if !output.status.success() {
        return Err(format!("{program} failed"));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|error| error.to_string())
}

fn digest(bytes: &[u8]) -> String {
    format!(
        "{:016x}",
        bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
        })
    )
}

fn metadata(
    mode: &str,
    seed: u64,
    warmup: u64,
    blocks: u32,
    timeout_ms: u64,
    schedule: u64,
    expected: usize,
) -> Result<Meta, String> {
    let rustc = output("rustc", &["--version", "--verbose"])?;
    let target = rustc
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or("rustc omitted host target")?
        .to_owned();
    let state = output("git", &["status", "--porcelain"])?;
    let diff = output("git", &["diff", "HEAD", "--no-ext-diff", "--binary"])?;
    let host = if cfg!(windows) {
        output("cmd", &["/c", "ver"])?
    } else {
        output("uname", &["-srvmo"])?
    };
    Ok(Meta {
        format: 2,
        revision: output("git", &["rev-parse", "HEAD"])?,
        dirty: !state.is_empty(),
        diff: digest(format!("{state}\n{diff}").as_bytes()),
        target,
        os: env::consts::OS.into(),
        arch: env::consts::ARCH.into(),
        rustc: rustc.replace(['\t', '\n', '\r'], " "),
        command: env::args().collect::<Vec<_>>().join(" "),
        started: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_millis()
            .to_string(),
        mode: mode.into(),
        seed,
        warmup,
        blocks,
        timeout_ms,
        schedule,
        expected,
        host: format!(
            "{};profile=bench;cpu=unavailable;topology=logical:{};affinity=uncontrolled;power=unavailable;page=unavailable",
            host.replace(['\t', '\n', '\r'], " "),
            std::thread::available_parallelism().map_or(0, usize::from)
        ),
        spin: 0,
    })
}

fn execute(
    scheduled: Scheduled,
    requested: u64,
    warmup: u64,
    seed: u64,
    timeout: Duration,
) -> Trial {
    let cell = scheduled.contrast.cell(scheduled.arm);
    let trial = Trial {
        block: scheduled.block,
        order: scheduled.order,
        cell,
        requested,
        accepted: 0,
        completed: 0,
        validated: 0,
        elapsed_ns: None,
        phase_ns: [0; 3],
        observed: None,
        status: Status::Unsupported,
        error: None,
    };
    if scheduled.arm != Arm::Stream {
        return Trial {
            error: Some("Evering transport is not implemented".into()),
            ..trial
        };
    }
    match stream::run(&trial.cell, requested, warmup, seed, timeout) {
        Ok(counts) => Trial {
            accepted: counts.accepted,
            completed: counts.completed,
            validated: counts.validated,
            elapsed_ns: Some(counts.elapsed_ns),
            phase_ns: counts.phase_ns,
            observed: counts.observed,
            status: Status::Ok,
            ..trial
        },
        Err(error) => {
            let counts = error.counts;
            Trial {
                accepted: counts.accepted,
                completed: counts.completed,
                validated: counts.validated,
                status: error.status,
                error: Some(error.message),
                phase_ns: counts.phase_ns,
                observed: counts.observed,
                ..trial
            }
        }
    }
}

fn record(
    path: &str,
    mode: &str,
    selected: Vec<Scheduled>,
    blocks: u32,
    seed: u64,
    requested: u64,
    warmup: u64,
    timeout_ms: u64,
) -> Result<(), String> {
    if blocks == 0 || requested == 0 || timeout_ms == 0 {
        return Err("blocks, operations, and timeout must be nonzero".into());
    }
    let meta = metadata(
        mode,
        seed,
        warmup,
        blocks,
        timeout_ms,
        schedule_id(&selected),
        selected.len(),
    )?;
    let timeout = Duration::from_millis(timeout_ms);
    let mut recorder = Recorder::create(Path::new(path), meta)?;
    let mut all_ok = true;
    for scheduled in selected {
        let trial = execute(scheduled, requested, warmup, seed, timeout);
        all_ok &= trial.status == Status::Ok;
        recorder.append(trial)?;
    }
    if all_ok {
        recorder.finish()
    } else {
        Err("mandatory trial failed; inspect the partial evidence record".into())
    }
}

fn dispatch() -> Result<(), String> {
    let args: Vec<_> = env::args()
        .skip(1)
        .filter(|argument| argument != "--bench")
        .collect();
    match args.as_slice() {
        [command, address] if command == "worker-stream" => stream::worker(address),
        [command, blocks, seed] if command == "plan" => {
            let blocks = number(blocks, "blocks")?;
            let seed = number(seed, "seed")?;
            println!(
                "block\torder\timplementation\tpolicy\tcandidate\tpayload\tcapacity\tin_flight\tmemory"
            );
            for entry in schedule(&contrasts(), blocks, seed) {
                let cell = entry.contrast.cell(entry.arm);
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    entry.block,
                    entry.order,
                    cell.implementation,
                    cell.policy,
                    cell.candidate,
                    cell.payload,
                    cell.capacity,
                    cell.in_flight,
                    cell.memory,
                );
            }
            Ok(())
        }
        [command, path] if command == "validate" => {
            let loaded = load(Path::new(path))?;
            let study = loaded.study;
            let state = if loaded.complete {
                "complete"
            } else {
                "partial"
            };
            println!(
                "valid {state}: {} blocks, {} trials, revision {}",
                study.meta.blocks,
                study.trials.len(),
                study.meta.revision
            );
            Ok(())
        }
        [command, path, blocks, seed, requested, warmup, timeout] if command == "run" => {
            let blocks = number(blocks, "blocks")?;
            let seed = number(seed, "seed")?;
            let requested = number(requested, "operations")?;
            let warmup = number(warmup, "warmup")?;
            let timeout = number(timeout, "timeout")?;
            let selected = schedule(&contrasts(), blocks, seed);
            record(
                path, "run", selected, blocks, seed, requested, warmup, timeout,
            )
        }
        [command, path] if command == "smoke" => {
            let contrast = contrast(64, 8, 8, Policy::Notified);
            record(
                path,
                "smoke",
                vec![Scheduled {
                    block: 0,
                    order: 0,
                    contrast,
                    arm: Arm::Stream,
                }],
                1,
                7,
                100,
                10,
                5000,
            )
        }
        _ => Err("usage: ipc plan <blocks> <seed> | ipc validate <file> | \
             ipc run <file> <blocks> <seed> <operations> <warmup> <timeout-ms> | \
             ipc smoke <file>"
            .into()),
    }
}

fn main() -> ExitCode {
    if let Err(error) = dispatch() {
        eprintln!("{error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
