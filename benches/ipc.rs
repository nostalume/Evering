#[path = "ipc/model.rs"]
mod model;
#[path = "ipc/stream.rs"]
mod stream;

use std::{
    env, fs,
    io::Write,
    process::{Command, ExitCode},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use model::{
    Arm, Contrast, ContrastKey, Meta, Policy, Scheduled, Status, Study, Trial, decode, encode,
    mandatory_success, schedule,
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

fn number<T: core::str::FromStr>(value: Option<String>, name: &str) -> Result<T, String> {
    value
        .ok_or_else(|| format!("missing {name}"))?
        .parse()
        .map_err(|_| format!("invalid {name}"))
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

fn metadata(seed: u64, warmup: u64, blocks: u32, timeout_ms: u64) -> Result<Meta, String> {
    let rustc = output("rustc", &["--version", "--verbose"])?;
    let target = rustc
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or("rustc omitted host target")?
        .to_owned();
    Ok(Meta {
        format: 1,
        revision: output("git", &["rev-parse", "HEAD"])?,
        dirty: !output("git", &["status", "--porcelain"])?.is_empty(),
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
        seed,
        warmup,
        blocks,
        timeout_ms,
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
            status: Status::Ok,
            ..trial
        },
        Err(error) => Trial {
            accepted: error.accepted,
            completed: error.completed,
            validated: error.validated,
            status: error.status,
            error: Some(error.message),
            ..trial
        },
    }
}

fn record(
    path: &str,
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
    let meta = metadata(seed, warmup, blocks, timeout_ms)?;
    let timeout = Duration::from_millis(timeout_ms);
    let trials: Vec<Trial> = selected
        .into_iter()
        .map(|scheduled| execute(scheduled, requested, warmup, seed, timeout))
        .collect();
    let all_ok = mandatory_success(&trials);
    let encoded = encode(&Study { meta, trials }).map_err(|error| format!("{error:?}"))?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("open evidence: {error}"))?;
    file.write_all(encoded.as_bytes())
        .map_err(|error| error.to_string())?;
    all_ok
        .then_some(())
        .ok_or_else(|| "mandatory trial failed; inspect the evidence record".into())
}

fn dispatch() -> Result<(), String> {
    let mut args = env::args().skip(1).filter(|argument| argument != "--bench");
    match args.next().as_deref() {
        Some("worker-stream") => {
            let address = args.next().ok_or("missing worker address")?;
            if args.next().is_some() {
                return Err("unexpected argument".into());
            }
            stream::worker(&address)
        }
        Some("plan") => {
            let blocks = number(args.next(), "blocks")?;
            let seed = number(args.next(), "seed")?;
            if args.next().is_some() {
                return Err("unexpected argument".into());
            }
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
        Some("validate") => {
            let path = args.next().ok_or("missing evidence path")?;
            if args.next().is_some() {
                return Err("unexpected argument".into());
            }
            let study = decode(&fs::read_to_string(path).map_err(|error| error.to_string())?)
                .map_err(|error| format!("{error:?}"))?;
            println!(
                "valid: {} blocks, {} trials, revision {}",
                study.meta.blocks,
                study.trials.len(),
                study.meta.revision
            );
            Ok(())
        }
        Some("run") => {
            let path = args.next().ok_or("missing evidence path")?;
            let blocks = number(args.next(), "blocks")?;
            let seed = number(args.next(), "seed")?;
            let requested = number(args.next(), "operations")?;
            let warmup = number(args.next(), "warmup")?;
            let timeout = number(args.next(), "timeout")?;
            if args.next().is_some() {
                return Err("unexpected argument".into());
            }
            let selected = schedule(&contrasts(), blocks, seed);
            record(&path, selected, blocks, seed, requested, warmup, timeout)
        }
        Some("smoke") => {
            let path = args.next().ok_or("missing evidence path")?;
            if args.next().is_some() {
                return Err("unexpected argument".into());
            }
            let contrast = contrast(64, 8, 8, Policy::Notified);
            record(
                &path,
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
    match dispatch() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
