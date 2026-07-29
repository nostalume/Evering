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

use model::{Cell, Meta, Status, Study, Trial, decode, encode, schedule};

fn cells() -> Vec<Cell> {
    let mut cells = Vec::new();
    for implementation in ["evering", "os-stream"] {
        for policy in ["busy", "adaptive", "notified"] {
            for payload in [0_u64, 64, 1024, 16 * 1024, 64 * 1024] {
                for capacity in [1_u64, 8, 256] {
                    for in_flight in [1_u64, 8, 64] {
                        let working = payload.max(64) * capacity * 2;
                        let memory = (working + 4 * 1024 * 1024).next_multiple_of(4096);
                        cells.push(Cell {
                            implementation: implementation.into(),
                            policy: policy.into(),
                            payload,
                            capacity,
                            in_flight,
                            memory,
                        });
                    }
                }
            }
        }
    }
    cells
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
    block: u32,
    order: u32,
    cell: Cell,
    requested: u64,
    warmup: u64,
    seed: u64,
    timeout: Duration,
) -> Trial {
    if cell.implementation != "os-stream" || cell.policy != "notified" {
        return Trial {
            block,
            order,
            cell,
            requested,
            accepted: 0,
            completed: 0,
            validated: 0,
            elapsed_ns: None,
            status: Status::Unsupported,
            error: Some("implementation/policy is not implemented".into()),
        };
    }
    match stream::run(&cell, requested, warmup, seed, timeout) {
        Ok(counts) => Trial {
            block,
            order,
            cell,
            requested,
            accepted: counts.accepted,
            completed: counts.completed,
            validated: counts.validated,
            elapsed_ns: Some(counts.elapsed_ns),
            status: Status::Ok,
            error: None,
        },
        Err(error) => Trial {
            block,
            order,
            cell,
            requested,
            accepted: error.accepted,
            completed: error.completed,
            validated: error.validated,
            elapsed_ns: None,
            status: error.status,
            error: Some(error.message),
        },
    }
}

fn record(
    path: &str,
    selected: Vec<Cell>,
    blocks: u32,
    seed: u64,
    requested: u64,
    warmup: u64,
    timeout_ms: u64,
) -> Result<bool, String> {
    if blocks == 0 || requested == 0 || timeout_ms == 0 {
        return Err("blocks, operations, and timeout must be nonzero".into());
    }
    let meta = metadata(seed, warmup, blocks, timeout_ms)?;
    let timeout = Duration::from_millis(timeout_ms);
    let trials: Vec<Trial> = schedule(&selected, blocks, seed)
        .into_iter()
        .map(|(block, order, cell)| execute(block, order, cell, requested, warmup, seed, timeout))
        .collect();
    let all_ok = trials.iter().all(|trial| trial.status == Status::Ok);
    let encoded = encode(&Study { meta, trials }).map_err(|error| format!("{error:?}"))?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("open evidence: {error}"))?;
    file.write_all(encoded.as_bytes())
        .map_err(|error| error.to_string())?;
    Ok(all_ok)
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
            println!("block\torder\timplementation\tpolicy\tpayload\tcapacity\tin_flight\tmemory");
            for (block, order, cell) in schedule(&cells(), blocks, seed) {
                println!(
                    "{block}\t{order}\t{}\t{}\t{}\t{}\t{}\t{}",
                    cell.implementation,
                    cell.policy,
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
            record(&path, cells(), blocks, seed, requested, warmup, timeout).map(|_| ())
        }
        Some("smoke") => {
            let path = args.next().ok_or("missing evidence path")?;
            if args.next().is_some() {
                return Err("unexpected argument".into());
            }
            let all_ok = record(
                &path,
                vec![Cell {
                    implementation: "os-stream".into(),
                    policy: "notified".into(),
                    payload: 64,
                    capacity: 8,
                    in_flight: 8,
                    memory: 4 * 1024 * 1024,
                }],
                1,
                7,
                100,
                10,
                5000,
            )?;
            all_ok
                .then_some(())
                .ok_or_else(|| "smoke trial did not succeed; inspect the evidence record".into())
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
