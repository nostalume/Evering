mod analysis;
mod drive;
mod environment;
mod evering;
mod family;
pub mod geometry;
#[cfg(all(unix, feature = "local-socket"))]
mod local;
mod model;
mod pilot;
#[cfg(feature = "plot")]
mod plot;
mod stream;

use std::{
    env,
    io::Write,
    path::Path,
    process::{Command, ExitCode},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use drive::Deadline;
use model::{
    Meta, Scheduled, Status, Trial, load, record as record_evidence, schedule, schedule_id,
};

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

fn metadata(
    family: &family::Family,
    mode: &str,
    run: (u64, u64, u32, u64),
    schedule: (u64, usize),
    environment: &environment::Snapshot,
) -> Result<Meta, String> {
    let (seed, warmup, blocks, timeout_ms) = run;
    let (schedule, expected) = schedule;
    let rustc = output("rustc", &["--version", "--verbose"])?;
    let target = rustc
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or("rustc omitted host target")?
        .to_owned();
    let state = output("git", &["status", "--porcelain"])?;
    let diff = output("git", &["diff", "HEAD", "--no-ext-diff", "--binary"])?;
    Ok(Meta {
        format: 5,
        family: family.key.into(),
        family_revision: family.revision,
        revision: output("git", &["rev-parse", "HEAD"])?,
        dirty: !state.is_empty(),
        diff: environment::digest(format!("{state}\n{diff}").as_bytes()),
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
        host: environment.text.clone(),
        spin: evering::ADAPTIVE_SPINS as u32,
    })
}

fn pilot_identity(meta: &Meta, environment: &environment::Snapshot) -> pilot::Identity {
    pilot::Identity {
        algorithm: 3,
        family: meta.family.clone(),
        family_revision: meta.family_revision,
        revision: meta.revision.clone(),
        diff: meta.diff.clone(),
        target: meta.target.clone(),
        rustc: meta.rustc.clone(),
        environment: environment.digest.clone(),
        seed: meta.seed,
        warmup: meta.warmup,
        timeout_ms: meta.timeout_ms,
    }
}

fn progress(line: &str) {
    eprintln!("{line}");
    let _ = std::io::stderr().flush();
}

fn execute(
    scheduled: Scheduled,
    requested: u64,
    warmup: u64,
    seed: u64,
    deadline: Deadline,
    environment: &str,
) -> Trial {
    let cell = scheduled.cell();
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
        path: drive::Path::default(),
        status: Status::Unsupported,
        error: None,
    };
    let result = (scheduled.arm.run)(&trial.cell, requested, warmup, seed, deadline, environment);
    match result {
        Ok(counts) => Trial {
            accepted: counts.accepted,
            completed: counts.completed,
            validated: counts.validated,
            elapsed_ns: Some(counts.elapsed_ns),
            phase_ns: counts.phase_ns,
            observed: counts.observed,
            path: counts.path,
            status: Status::Ok,
            ..trial
        },
        Err(error) => {
            let counts = *error.counts;
            Trial {
                accepted: counts.accepted,
                completed: counts.completed,
                validated: counts.validated,
                status: error.status,
                error: Some(error.message),
                phase_ns: counts.phase_ns,
                observed: counts.observed,
                path: counts.path,
                ..trial
            }
        }
    }
}

struct Record<'a> {
    family: &'static family::Family,
    path: &'a str,
    mode: &'a str,
    selected: Vec<Scheduled>,
    blocks: u32,
    seed: u64,
    requested: u64,
    manifest: Option<(pilot::Manifest, String)>,
    warmup: u64,
    timeout_ms: u64,
    deadline: Deadline,
}

fn record(input: Record<'_>) -> Result<(), String> {
    if input.blocks == 0
        || (input.manifest.is_none() && input.requested == 0)
        || input.timeout_ms == 0
    {
        return Err("blocks, fixed operations, and timeout must be nonzero".into());
    }
    let environment = environment::capture()?;
    let mut meta = metadata(
        input.family,
        input.mode,
        (input.seed, input.warmup, input.blocks, input.timeout_ms),
        (schedule_id(&input.selected), input.selected.len()),
        &environment,
    )?;
    let counts = if let Some((manifest, digest)) = input.manifest {
        let counts = pilot::admit(
            &manifest,
            &pilot_identity(&meta, &environment),
            &input.selected,
        )?;
        meta.host += &format!(";pilot={digest}");
        counts
    } else {
        vec![input.requested; input.selected.len()]
    };
    let timeout = Duration::from_millis(input.timeout_ms);
    let total = input.selected.len();
    let mut done = 0;
    record_evidence(
        Path::new(input.path),
        meta,
        input
            .selected
            .into_iter()
            .zip(counts)
            .map(|(scheduled, requested)| {
                let deadline = input.deadline.within(Instant::now(), timeout)?;
                let trial = execute(
                    scheduled,
                    requested,
                    input.warmup,
                    input.seed,
                    deadline,
                    &environment.digest,
                );
                done += 1;
                progress(&pilot::progress("trial", done, total, &trial.cell));
                if done == total || done.is_multiple_of(total / input.blocks as usize) {
                    progress(&format!("block {} complete", trial.block));
                }
                Ok(trial)
            }),
    )?;
    Ok(())
}

fn pilot(
    family: &'static family::Family,
    path: &str,
    seed: u64,
    warmup: u64,
    timeout_ms: u64,
) -> Result<(), String> {
    if warmup == 0 || timeout_ms == 0 {
        return Err("warmup and timeout must be nonzero".into());
    }
    let began = Instant::now();
    let command = family.deadline("pilot", began)?;
    let selected = schedule(&(family.members)("screening").unwrap(), 1, seed);
    let environment = environment::capture()?;
    let meta = metadata(
        family,
        "pilot",
        (seed, warmup, 1, timeout_ms),
        (schedule_id(&selected), selected.len()),
        &environment,
    )?;
    let timeout = Duration::from_millis(timeout_ms);
    let identity = pilot_identity(&meta, &environment);
    let total = selected.len();
    let rows = selected
        .iter()
        .copied()
        .enumerate()
        .map(|(index, scheduled)| {
            let cell = scheduled.cell();
            progress(&pilot::progress("pilot-start", index + 1, total, &cell));
            let result = pilot::calibrate(cell.clone(), warmup, |requested| {
                let deadline = command.within(Instant::now(), timeout)?;
                let trial = execute(
                    scheduled,
                    requested,
                    warmup,
                    seed,
                    deadline,
                    &environment.digest,
                );
                let result = if trial.status == Status::Ok {
                    Ok(trial.elapsed_ns.unwrap())
                } else {
                    Err(trial.error.unwrap_or_else(|| "pilot trial failed".into()))
                };
                command.remaining(Instant::now())?;
                result
            });
            if result.is_ok() {
                progress(&pilot::progress("pilot-complete", index + 1, total, &cell));
            }
            (cell, result)
        });
    let digest = pilot::record(Path::new(path), identity, total, rows)?;
    println!("pilot {digest}: {total} frozen arm counts");
    Ok(())
}

fn dispatch() -> Result<(), String> {
    let args: Vec<_> = env::args()
        .skip(1)
        .filter(|argument| argument != "--bench")
        .collect();
    if args.is_empty() {
        return Ok(());
    }
    match args.as_slice() {
        [command, address, environment] if command == "worker-stream" => {
            stream::worker(address, environment)
        }
        #[cfg(all(unix, feature = "local-socket"))]
        [command, path, environment] if command == "worker-local" => {
            local::worker(path, environment)
        }
        [command, address, policy, timeout, environment] if command == "worker-evering" => {
            evering::worker(
                address,
                policy,
                Duration::from_millis(number(timeout, "timeout")?),
                environment,
            )
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
        [command, paths @ ..] if command == "analyze" && !paths.is_empty() => {
            print!("{}", analysis::command(paths)?);
            Ok(())
        }
        [command, paths @ ..] if command == "geometry" && !paths.is_empty() => {
            let studies = paths
                .iter()
                .map(|path| {
                    load(Path::new(path)).and_then(|loaded| {
                        loaded
                            .complete
                            .then_some(loaded.study)
                            .ok_or("partial evidence".into())
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            print!("{}", geometry::evidence_report(&studies)?);
            Ok(())
        }
        #[cfg(feature = "plot")]
        [command, output, paths @ ..] if command == "plot" && !paths.is_empty() => {
            for path in plot::command(Path::new(output), paths)? {
                println!("{}", path.display());
            }
            Ok(())
        }
        [command, family, path, seed, warmup, timeout] if command == "pilot" => pilot(
            family::find(family).ok_or_else(|| format!("unknown family: {family}"))?,
            path,
            number(seed, "seed")?,
            number(warmup, "warmup")?,
            number(timeout, "timeout")?,
        ),
        [command, family, path, manifest] if command == "screening" || command == "focused" => {
            let family = family::find(family).ok_or_else(|| format!("unknown family: {family}"))?;
            let deadline = family.deadline(command, Instant::now())?;
            let (pilot, digest) = pilot::load(Path::new(manifest))?;
            let seed = pilot.identity.seed;
            let warmup = pilot.identity.warmup;
            let timeout = pilot.identity.timeout_ms;
            let blocks = family.mode(command).unwrap().1;
            let members = (family.members)(command).unwrap();
            let selected = schedule(&members, blocks, seed);
            record(Record {
                family,
                path,
                mode: command,
                selected,
                blocks,
                seed,
                requested: 0,
                manifest: Some((pilot, digest)),
                warmup,
                timeout_ms: timeout,
                deadline,
            })
        }
        [command, family, path] if command == "smoke" => {
            let family = family::find(family).ok_or_else(|| format!("unknown family: {family}"))?;
            let deadline = family.deadline("smoke", Instant::now())?;
            let selected = schedule(&(family.members)("smoke").unwrap(), 1, 7);
            record(Record {
                family,
                path,
                mode: "smoke",
                selected,
                blocks: 1,
                seed: 7,
                requested: 101,
                manifest: None,
                warmup: 10,
                timeout_ms: 5000,
                deadline,
            })
        }
        _ => Err(
            "usage: ipc validate <file> | ipc analyze <file>... | ipc geometry <file>... | \
             ipc plot <output-dir> <file>... | \
             ipc pilot <family> <manifest> <seed> <warmup> <timeout-ms> | \
             ipc screening|focused <family> <file> <manifest> | \
             ipc smoke <family> <file>"
                .into(),
        ),
    }
}

fn main() -> ExitCode {
    if let Err(error) = dispatch() {
        eprintln!("{error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
