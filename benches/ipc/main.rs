mod analysis;
mod drive;
mod environment;
mod evering;
mod family;
mod fixture;
pub mod geometry;
#[cfg(all(unix, feature = "local-socket"))]
mod local;
mod mechanism;
mod model;
mod pilot;
#[cfg(feature = "plot")]
mod plot;
mod stream;
mod study;
mod system;

use std::{
    env,
    io::Write,
    path::Path,
    process::ExitCode,
    time::{Duration, Instant},
};

use drive::Deadline;
use model::{Scheduled, schedule};
use study::{Header, Observation, Recorder, Run, Unit};

fn number<T: core::str::FromStr>(value: &str, name: &str) -> Result<T, String> {
    value.parse().map_err(|_| format!("invalid {name}"))
}

fn pilot_identity(
    family: &family::Family,
    capture: &environment::Capture,
    seed: u64,
    warmup: u64,
    timeout_ms: u64,
) -> pilot::Identity {
    pilot::Identity {
        algorithm: 4,
        family: family.key.into(),
        family_revision: family.revision,
        revision: capture.context.source.revision.clone(),
        dirty: capture.context.source.dirty,
        diff: capture.context.source.diff.clone(),
        target: capture.context.compiler.target.clone(),
        os: capture.context.host.os.clone(),
        arch: capture.context.host.arch.clone(),
        rustc: capture.context.compiler.rustc.clone(),
        host: capture.context.host.description.clone(),
        environment: capture.context.host.environment.clone(),
        command: capture.command.clone(),
        started: capture.started.clone(),
        seed,
        warmup,
        timeout_ms,
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
) -> Result<system::Measure, String> {
    let cell = scheduled.cell();
    let counts = (scheduled.arm.run)(&cell, requested, warmup, seed, deadline, environment)
        .map_err(|error| format!("{:?}: {}", error.status, error.message))?;
    Ok(system::Measure {
        requested,
        accepted: counts.accepted,
        completed: counts.completed,
        validated: counts.validated,
        elapsed_ns: counts.elapsed_ns,
        phase_ns: counts.phase_ns,
        observed: counts.observed.ok_or("runner omitted observed resources")?,
        path: counts.path,
    })
}

struct Record<'a> {
    family: &'static family::Family,
    path: &'a str,
    mode: &'a str,
    selected: Vec<Scheduled>,
    blocks: u32,
    seed: u64,
    requested: u64,
    manifest: Option<(pilot::Evidence, String)>,
    warmup: u64,
    timeout_ms: u64,
    deadline: Deadline,
}

fn system_header(
    input: &Record<'_>,
    capture: environment::Capture,
    calibration: Option<String>,
) -> Result<Header<system::Specification, system::Case>, String> {
    let mut cases = Vec::new();
    let mut execution = Vec::with_capacity(input.selected.len());
    for scheduled in &input.selected {
        let case = system::Case {
            workload: scheduled.cell(),
            resources: scheduled.arm.resources(),
        };
        let index = match cases.iter().position(|known| known == &case) {
            Some(index) => index,
            None => {
                cases.push(case);
                cases.len() - 1
            }
        };
        execution.push(study::Scheduled {
            unit: Unit {
                block: scheduled.block,
                order: scheduled.order,
            },
            case: u32::try_from(index).map_err(|_| "too many system cases")?,
        });
    }
    let contrasts = cases
        .iter()
        .filter(|case| case.workload.arm != input.family.baseline.key)
        .filter_map(|candidate| {
            let baseline = cases.iter().find(|case| {
                case.workload.condition() == candidate.workload.condition()
                    && case.workload.arm == input.family.baseline.key
            })?;
            Some(system::Contrast {
                candidate: study::case_id::<system::System>(candidate),
                baseline: study::case_id::<system::System>(baseline),
                delta: 0.05,
                role: system::Role::Primary,
            })
        })
        .collect();
    Ok(Header::new::<system::System>(
        capture.context,
        system::Specification {
            family: input.family.key.into(),
            family_revision: input.family.revision,
            mode: input.mode.into(),
            blocks: input.blocks,
            spin: evering::ADAPTIVE_SPINS as u32,
            timeout_ms: input.timeout_ms,
            alpha: 0.05,
            calibration,
            contrasts,
        },
        cases,
        Run {
            seed: input.seed,
            started: capture.started,
            command: capture.command,
            warmup: input.warmup,
            budget_ms: u64::try_from(
                input
                    .family
                    .mode(input.mode)
                    .ok_or("unknown family mode")?
                    .0
                    .as_millis(),
            )
            .map_err(|_| "family budget exceeds u64 milliseconds")?,
            schedule: execution,
        },
    ))
}

fn record(input: Record<'_>) -> Result<(), String> {
    if input.blocks == 0
        || (input.manifest.is_none() && input.requested == 0)
        || input.timeout_ms == 0
    {
        return Err("blocks, fixed operations, and timeout must be nonzero".into());
    }
    let environment = environment::capture()?;
    let capture = environment::metadata(&environment)?;
    let counts = if let Some((manifest, digest)) = input.manifest.as_ref() {
        let counts = pilot::admit(
            manifest,
            &pilot_identity(
                input.family,
                &capture,
                input.seed,
                input.warmup,
                input.timeout_ms,
            ),
            &input.selected,
        )?;
        (counts, Some(digest.clone()))
    } else {
        (vec![input.requested; input.selected.len()], None)
    };
    let timeout = Duration::from_millis(input.timeout_ms);
    let total = input.selected.len();
    let header = system_header(&input, capture, counts.1)?;
    let execution = header.run.schedule.clone();
    let mut recorder = Recorder::<system::System>::create(Path::new(input.path), header)
        .map_err(|error| format!("{error:?}"))?;
    for (done, ((scheduled, requested), expected)) in input
        .selected
        .into_iter()
        .zip(counts.0)
        .zip(execution)
        .enumerate()
    {
        let unit = Unit {
            block: scheduled.block,
            order: scheduled.order,
        };
        let result = input
            .deadline
            .within(Instant::now(), timeout)
            .and_then(|deadline| {
                execute(
                    scheduled,
                    requested,
                    input.warmup,
                    input.seed,
                    deadline,
                    &environment.digest,
                )
            });
        let measure = match result {
            Ok(measure) => measure,
            Err(error) => {
                recorder
                    .abort(Some(unit), &error)
                    .map_err(|error| format!("{error:?}"))?;
                return Err(error);
            }
        };
        recorder
            .observe(Observation {
                unit,
                case: expected.case,
                measure,
            })
            .map_err(|error| format!("{error:?}"))?;
        progress(&pilot::progress(
            "trial",
            done + 1,
            total,
            &scheduled.cell(),
        ));
        if done + 1 == total || (done + 1).is_multiple_of(total / input.blocks as usize) {
            progress(&format!("block {} complete", scheduled.block));
        }
    }
    recorder
        .complete()
        .map(drop)
        .map_err(|error| format!("{error:?}"))
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
    let capture = environment::metadata(&environment)?;
    let timeout = Duration::from_millis(timeout_ms);
    let identity = pilot_identity(family, &capture, seed, warmup, timeout_ms);
    let total = selected.len();
    let cases = selected.iter().map(|scheduled| scheduled.cell()).collect();
    let rows = selected
        .iter()
        .copied()
        .enumerate()
        .map(|(index, scheduled)| {
            let cell = scheduled.cell();
            progress(&pilot::progress("pilot-start", index + 1, total, &cell));
            let result = pilot::calibrate(cell.clone(), warmup, |requested| {
                let deadline = command.within(Instant::now(), timeout)?;
                let measure = execute(
                    scheduled,
                    requested,
                    warmup,
                    seed,
                    deadline,
                    &environment.digest,
                )?;
                command.remaining(Instant::now())?;
                Ok(measure.elapsed_ns)
            });
            if result.is_ok() {
                progress(&pilot::progress("pilot-complete", index + 1, total, &cell));
            }
            result
        });
    let digest = pilot::record(Path::new(path), identity, cases, rows)?;
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
            let evidence = system::load(Path::new(path))?;
            println!(
                "valid complete: {} blocks, {} observations, revision {}",
                evidence.header.specification.blocks,
                evidence.observations.len(),
                evidence.header.context.source.revision
            );
            Ok(())
        }
        [command, specification, output] if command == "mechanism" => {
            fixture::record(Path::new(specification), Path::new(output))
        }
        [command, paths @ ..] if command == "analyze" && !paths.is_empty() => {
            print!("{}", analysis::command(paths)?);
            Ok(())
        }
        [command, paths @ ..] if command == "geometry" && !paths.is_empty() => {
            let studies = paths
                .iter()
                .map(|path| system::load(Path::new(path)))
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
            let identity = pilot::identity(&pilot);
            let seed = identity.seed;
            let warmup = identity.warmup;
            let timeout = identity.timeout_ms;
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
            "usage: ipc validate <file> | ipc mechanism <spec.json> <new-output> | \
             ipc analyze <file>... | ipc geometry <file>... | \
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
