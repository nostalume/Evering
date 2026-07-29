#[path = "ipc/model.rs"]
mod model;

use std::{env, fs, process::ExitCode};

use model::{Cell, decode, schedule};

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

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
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
        _ => Err("usage: ipc plan <blocks> <seed> | ipc validate <evidence.tsv>".into()),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
