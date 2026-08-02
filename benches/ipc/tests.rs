#[cfg(feature = "plot")]
use super::plot;
use super::{analysis, drive, environment, evering, family, geometry, micro, model, pilot, stream};

#[test]
fn geometry_replay_keeps_causal_metrics_separate() {
    use geometry::{Event::*, Geometry, Rejection, Trace};

    let geometry = Geometry::from_classes(256, &[(64, 1), (128, 1)]).unwrap();
    let trace = Trace::synthetic([
        Reserve {
            id: 1,
            owner: 7,
            bytes: 64,
        },
        Reserve {
            id: 2,
            owner: 7,
            bytes: 64,
        },
        Reserve {
            id: 3,
            owner: 7,
            bytes: 64,
        },
        Release { id: 1 },
        Transfer { id: 2, to: 9 },
        Crash { owner: 9 },
        Reap { owner: 9 },
    ]);
    let first = geometry.replay(&trace);

    assert_eq!(first, geometry.replay(&trace));
    assert_eq!((first.accepted, first.rejected), (2, 1));
    assert_eq!(first.rejections, [(2, Rejection::Unavailable)]);
    assert_eq!((first.requested_bytes, first.peak_live_bytes), (192, 128));
    assert_eq!(
        (first.peak_physical_bytes, first.peak_fragmentation),
        (192, 64)
    );
    assert_eq!(
        (first.spills, first.recovery_work, first.reclaimed_bytes),
        (1, 2, 128)
    );
}

#[test]
fn replacement_geometry_repairs_static_history_without_a_buddy_model() {
    use geometry::{Event::*, Geometry, Outcome, Rejection, Trace};

    let trace = Trace::synthetic([
        Reserve {
            id: 1,
            owner: 1,
            bytes: 64,
        },
        Reserve {
            id: 2,
            owner: 1,
            bytes: 64,
        },
        Reserve {
            id: 3,
            owner: 1,
            bytes: 64,
        },
        Reserve {
            id: 4,
            owner: 1,
            bytes: 64,
        },
        Release { id: 1 },
        Release { id: 2 },
        Release { id: 3 },
        Reserve {
            id: 5,
            owner: 1,
            bytes: 256,
        },
    ]);
    let original = Geometry::from_classes(581, &[(64, 1), (128, 2), (256, 1)]).unwrap();
    let comparison = original.compare_compacting(&trace);
    let divergence = comparison.first_divergence.unwrap();
    assert_eq!(divergence.event, 7);
    assert_eq!(
        divergence.static_model,
        Outcome::Rejected(Rejection::Unavailable)
    );
    assert_eq!(divergence.compact_bound, Outcome::Reserved);
    assert_eq!(comparison.static_model.largest[6], 128);

    let replacement = Geometry::from_classes(581, &[(64, 4), (256, 1)]).unwrap();
    assert_eq!(replacement.replay(&trace).rejections, []);
}

#[test]
fn balanced_geometry_is_integer_deterministic_and_extent_bounded() {
    let first = geometry::Geometry::balanced(4 * 1024 * 1024, 64, 64 * 1024).unwrap();
    assert_eq!(
        first,
        geometry::Geometry::balanced(4 * 1024 * 1024, 64, 64 * 1024).unwrap()
    );
    assert!(geometry::Geometry::balanced(63, 64, 64).is_err());
    assert!(geometry::Geometry::balanced(u64::MAX, 64, u64::MAX).is_err());
}

#[test]
fn projected_benchmark_conditions_never_authorize_buddy() {
    let mut trial = successful_trial();
    trial.observed.as_mut().unwrap().extent = Some(8192);
    let report = geometry::evidence_report(&[study(vec![trial.clone()])]).unwrap();
    assert!(report.starts_with("geometry-v1\nsource\t"));
    assert!(report.lines().nth(2).unwrap().ends_with("\tfalse"));
    assert_eq!(
        report,
        geometry::evidence_report(&[study(vec![trial])]).unwrap()
    );
}

#[derive(Default)]
struct TraceEndpoint {
    staged: Option<u64>,
    replies: std::collections::VecDeque<u64>,
    trace: Vec<(char, u64)>,
    corrupt: bool,
    fail_before_send: bool,
    fail_after_send: bool,
    fail_after_recv: bool,
    delay_ready: bool,
    fail_abort: bool,
    aborted: bool,
}

#[test]
fn family_identity_is_explicit_unique_and_closed() {
    let core = family::find("core-ipc").unwrap();
    assert_eq!((core.key, core.revision), ("core-ipc", 1));
    assert_eq!(
        core.mode("screening"),
        Some((std::time::Duration::from_secs(180), 3))
    );
    assert_eq!(core.mode("unknown"), None);
    assert!(family::find("unknown").is_none());
}

#[cfg(unix)]
#[test]
fn local_family_owns_its_exact_two_arm_matrix() {
    let local = family::find("local-ipc-unix").unwrap();
    assert_eq!((local.key, local.revision), ("local-ipc-unix", 1));
    assert_eq!(local.baseline.key, "uds/readiness");
    assert_eq!(
        local.mode("pilot"),
        Some((std::time::Duration::from_secs(45), 1))
    );
    assert_eq!(
        local.mode("screening"),
        Some((std::time::Duration::from_secs(90), 3))
    );
    assert_eq!(
        local.mode("focused"),
        Some((std::time::Duration::from_secs(240), 15))
    );
    for mode in ["screening", "focused"] {
        let members = (local.members)(mode).unwrap();
        assert_eq!(members.len(), 10);
        for payload in [0, 64, 1024, 16 * 1024, 64 * 1024] {
            let pair: Vec<_> = members
                .iter()
                .filter(|(cell, _)| cell.payload == payload)
                .collect();
            assert_eq!(pair.len(), 2);
            assert!(
                pair.iter()
                    .all(|(cell, _)| (cell.capacity, cell.in_flight) == (8, 8))
            );
        }
    }
}

#[test]
fn environment_snapshot_is_stable_complete_and_self_identifying() {
    let first = environment::capture().unwrap();
    let second = environment::capture().unwrap();
    assert_eq!(first, second);
    for field in [
        "os=",
        "arch=",
        "logical=",
        "affinity=",
        "page=",
        "power=",
        "kernel=",
        "native=",
        "physical=",
        "smt=",
        "thermal=",
        "background=",
    ] {
        assert!(first.text.split(';').any(|value| value.starts_with(field)));
    }
    assert_eq!(first.digest, environment::digest(first.text.as_bytes()));
    assert_eq!(environment::admit(&first.digest).unwrap(), first);
    assert!(environment::admit("0000000000000000").is_err());
}

fn pilot_identity() -> pilot::Identity {
    pilot::Identity {
        algorithm: 3,
        family: "core-ipc".into(),
        family_revision: 1,
        revision: "revision".into(),
        diff: "diff".into(),
        target: "target".into(),
        rustc: "rustc".into(),
        environment: "environment".into(),
        seed: 7,
        warmup: 8,
        timeout_ms: 5_000,
    }
}

#[test]
fn pilot_calibration_freezes_from_one_measurable_ramp_without_a_probe() {
    let cell = cell(&family::BUSY);
    let mut calls = 0;
    let row = pilot::calibrate(cell, 8, |_| {
        calls += 1;
        Ok(if calls == 1 { 49_000_000 } else { 110_000_000 })
    })
    .unwrap();
    assert_eq!((row.count, row.observations.len(), calls), (450, 2, 2));
}

#[test]
fn pilot_calibration_rejects_a_zero_duration_observation() {
    let cell = cell(&family::BUSY);
    assert!(pilot::calibrate(cell, 8, |_| Ok(0)).is_err());
}

#[test]
fn pilot_calibration_bounds_attempts_overflow_timeout_and_identity() {
    let mut attempts = 0;
    assert!(
        pilot::calibrate(cell(&family::BUSY), 8, |_| {
            attempts += 1;
            Ok(49_999_999)
        })
        .is_err()
    );
    assert_eq!(attempts, 8);

    let mut geometry = cell(&family::BUSY);
    geometry.capacity = u64::MAX;
    geometry.in_flight = u64::MAX;
    assert!(pilot::calibrate(geometry, 8, |_| unreachable!()).is_err());
    assert_eq!(
        pilot::calibrate(cell(&family::BUSY), 8, |_| Err("timeout".into())).unwrap_err(),
        "timeout"
    );
    assert!(pilot::calibrate(cell(&family::BUSY), u64::MAX, |_| Ok(50_000_000)).is_err());
}

#[test]
fn pilot_admission_rejects_foreign_missing_duplicate_and_unverified_counts() {
    let scheduled = model::schedule(
        &[contrast(&family::BUSY), (condition(), &family::STREAM)],
        2,
        7,
    );
    let cells: Vec<_> = scheduled[..2].iter().map(|entry| entry.cell()).collect();
    let verified = |cell| pilot::calibrate(cell, 8, |_| Ok(500_000_000)).unwrap();
    let manifest = pilot::Manifest {
        identity: pilot_identity(),
        rows: cells.into_iter().map(verified).collect(),
    };
    assert_eq!(
        pilot::admit(&manifest, &pilot_identity(), &scheduled).unwrap(),
        vec![96; 4]
    );
    let mut invalid = manifest.clone();
    invalid.identity.target = "foreign".into();
    assert!(pilot::admit(&invalid, &pilot_identity(), &scheduled).is_err());
    let mut invalid = manifest.clone();
    invalid.identity.family_revision += 1;
    assert!(pilot::admit(&invalid, &pilot_identity(), &scheduled).is_err());
    let mut invalid = manifest.clone();
    invalid.identity.algorithm = 2;
    assert!(pilot::admit(&invalid, &invalid.identity.clone(), &scheduled).is_err());
    let mut invalid = manifest.clone();
    invalid.rows.pop();
    assert!(pilot::admit(&invalid, &pilot_identity(), &scheduled).is_err());
    let mut invalid = manifest.clone();
    invalid.rows.push(invalid.rows[0].clone());
    assert!(pilot::admit(&invalid, &pilot_identity(), &scheduled).is_err());
    let mut invalid = manifest;
    invalid.rows[0].observations.pop();
    assert!(pilot::admit(&invalid, &pilot_identity(), &scheduled).is_err());
    let mut invalid = pilot::Manifest {
        identity: pilot_identity(),
        rows: scheduled[..2]
            .iter()
            .map(|entry| verified(entry.cell()))
            .collect(),
    };
    invalid.rows[0].count = 99;
    assert!(pilot::admit(&invalid, &pilot_identity(), &scheduled).is_err());
}

#[test]
fn pilot_manifest_round_trip_is_digest_bound_and_non_overwriting() {
    let scheduled = model::schedule(
        &[contrast(&family::BUSY), (condition(), &family::STREAM)],
        1,
        7,
    );
    let manifest = pilot::Manifest {
        identity: pilot_identity(),
        rows: scheduled
            .iter()
            .map(|entry| {
                let cell = entry.cell();
                pilot::calibrate(cell, 8, |count| Ok(count * 10_000_000)).unwrap()
            })
            .collect(),
    };
    let path = std::env::temp_dir().join(format!("evering-pilot-{}", std::process::id()));
    let digest = pilot::record(
        &path,
        manifest.identity.clone(),
        manifest.rows.len(),
        manifest
            .rows
            .iter()
            .cloned()
            .map(|row| (row.cell.clone(), Ok(row))),
    )
    .unwrap();
    assert_eq!(pilot::load(&path).unwrap(), (manifest.clone(), digest));
    assert!(
        pilot::record(
            &path,
            manifest.identity.clone(),
            manifest.rows.len(),
            std::iter::empty::<(model::Cell, Result<pilot::Row, String>)>(),
        )
        .is_err()
    );
    let mut corrupt = std::fs::read_to_string(&path).unwrap();
    corrupt = corrupt.replacen("revision", "foreign", 1);
    std::fs::write(&path, corrupt).unwrap();
    assert!(pilot::load(&path).is_err());
    std::fs::remove_file(path).unwrap();
}

#[test]
fn pilot_partial_is_durable_non_admissible_and_sanitizes_abort() {
    let scheduled = model::schedule(
        &[contrast(&family::BUSY), (condition(), &family::STREAM)],
        1,
        7,
    );
    let cell = scheduled[0].cell();
    let path = std::env::temp_dir().join(format!("evering-pilot-cut-{}", std::process::id()));
    let row = pilot::calibrate(cell.clone(), 8, |count| Ok(count * 10_000_000)).unwrap();
    assert!(
        pilot::record(
            &path,
            pilot_identity(),
            2,
            [
                (cell.clone(), Ok(row)),
                (cell, Err("bad\tline\nreason".into())),
            ],
        )
        .is_err()
    );
    assert!(pilot::load(&path).is_err());
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"kind\":\"abort\""));
    assert!(text.contains("bad line reason"));
    assert!(!text.contains("bad\tline\n"));
    std::fs::remove_file(path).unwrap();
}

#[test]
fn nested_deadlines_never_reset_or_exceed_the_command() {
    use std::time::{Duration, Instant};

    let now = Instant::now();
    let at = |seconds| now + Duration::from_secs(seconds);
    let command = drive::Deadline::after(now, Duration::from_secs(10)).unwrap();
    let early = command.within(at(2), Duration::from_secs(3)).unwrap();
    let capped = command.within(at(8), Duration::from_secs(5)).unwrap();
    assert_eq!(early.remaining(at(2)), Ok(Duration::from_secs(3)));
    assert_eq!(capped.remaining(at(8)), Ok(Duration::from_secs(2)));
    assert!(capped.remaining(at(10)).is_err());
    assert!(command.within(at(10), Duration::from_secs(1)).is_err());
    assert!(drive::Deadline::after(now, Duration::MAX).is_err());
    let cell = cell(&family::BUSY);
    assert_eq!(
        pilot::progress("pilot-start", 1, 2, &cell),
        "pilot-start 1/2: evering/busy payload=64 capacity=8 in-flight=3"
    );
}

impl drive::Endpoint for TraceEndpoint {
    type Error = &'static str;

    fn stage(&mut self, operation: u64, _: Vec<u8>) -> Result<(), Self::Error> {
        if self.staged.replace(operation).is_some() {
            return Err("double stage");
        }
        Ok(())
    }

    fn try_send(
        &mut self,
        _: &mut drive::Path,
    ) -> Result<drive::Step<(), Self::Error>, Self::Error> {
        if self.fail_before_send {
            return Err("pre-commit send failure");
        }
        let operation = self.staged.take().ok_or("send without stage")?;
        self.trace.push(('s', operation));
        self.replies.push_back(operation);
        let _advisory_signal_failed = self.fail_after_send;
        Ok(drive::Step::Committed(Ok(())))
    }

    fn try_recv(
        &mut self,
        _: &mut drive::Path,
        expected: drive::Expected,
    ) -> Result<drive::Step<bool, Self::Error>, Self::Error> {
        let Some(operation) = self.replies.pop_front() else {
            return Ok(drive::Step::Pending);
        };
        if self.delay_ready && operation == u64::MAX {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        self.trace.push(('r', operation));
        if self.fail_after_recv {
            return Ok(drive::Step::Committed(Err(
                "post-commit reconstruction failure",
            )));
        }
        let mut bytes = model::payload(7, operation, usize::from(operation != u64::MAX));
        if let Some(byte) = bytes.first_mut() {
            *byte ^= 0xa5;
            *byte ^= u8::from(self.corrupt);
        }
        Ok(drive::Step::Committed(Ok(
            expected.matches(operation, &bytes)
        )))
    }

    fn wait(
        &mut self,
        _: drive::Interest,
        _: drive::Deadline,
        _: &mut drive::Path,
    ) -> Result<(), Self::Error> {
        Err("unexpected wait")
    }

    fn abort(&mut self) -> Result<(), Self::Error> {
        self.aborted = true;
        self.staged = None;
        if self.fail_abort {
            Err("cleanup failure")
        } else {
            Ok(())
        }
    }
}

fn trace_transfer(
    endpoint: &mut TraceEndpoint,
    count: u64,
    window: u64,
) -> Result<drive::Counts, drive::Error<&'static str>> {
    drive::transfer(
        endpoint,
        drive::Work {
            start: 0,
            count,
            window,
            payload: 1,
            seed: 7,
        },
        drive::Deadline::after(std::time::Instant::now(), std::time::Duration::from_secs(1))
            .unwrap(),
        &mut drive::Path::default(),
    )
}

#[test]
fn shared_driver_uses_a_sliding_window_and_shared_validation() {
    let mut endpoint = TraceEndpoint::default();
    let counts = trace_transfer(&mut endpoint, 4, 2).unwrap();
    assert_eq!(
        (counts.accepted, counts.completed, counts.validated),
        (4, 4, 4)
    );
    assert_eq!(
        endpoint.trace,
        [
            ('s', 0),
            ('s', 1),
            ('r', 0),
            ('s', 2),
            ('r', 1),
            ('s', 3),
            ('r', 2),
            ('r', 3),
        ]
    );
}

#[test]
fn shared_driver_aborts_and_retains_counts_after_invalid_response() {
    let mut endpoint = TraceEndpoint {
        corrupt: true,
        ..TraceEndpoint::default()
    };
    let error = trace_transfer(&mut endpoint, 2, 1).unwrap_err();
    assert_eq!(error.kind, drive::Kind::InvalidResponse(0));
    assert_eq!(
        (
            error.counts.accepted,
            error.counts.completed,
            error.counts.validated
        ),
        (1, 1, 0)
    );
    assert!(endpoint.aborted);
}

#[test]
fn shared_driver_ignores_advisory_failure_after_send_commit() {
    let mut endpoint = TraceEndpoint {
        fail_after_send: true,
        ..TraceEndpoint::default()
    };
    let counts = trace_transfer(&mut endpoint, 1, 1).unwrap();
    assert_eq!(
        counts,
        drive::Counts {
            accepted: 1,
            completed: 1,
            validated: 1,
        }
    );
    assert!(!endpoint.aborted);
}

#[test]
fn shared_driver_counts_only_committed_work_before_errors() {
    let mut before = TraceEndpoint {
        fail_before_send: true,
        fail_abort: true,
        ..TraceEndpoint::default()
    };
    let error = trace_transfer(&mut before, 1, 1).unwrap_err();
    assert_eq!(error.kind, drive::Kind::Endpoint("pre-commit send failure"));
    assert_eq!(error.counts, drive::Counts::default());
    assert!(before.aborted);

    let mut after = TraceEndpoint {
        fail_after_recv: true,
        ..TraceEndpoint::default()
    };
    let error = trace_transfer(&mut after, 1, 1).unwrap_err();
    assert_eq!(
        error.kind,
        drive::Kind::Endpoint("post-commit reconstruction failure")
    );
    assert_eq!(
        error.counts,
        drive::Counts {
            accepted: 1,
            completed: 1,
            validated: 0,
        }
    );
    assert!(after.aborted);
}

#[cfg(feature = "process")]
#[test]
fn evering_bootstrap_binds_region_channel_and_extent() {
    use ::evering::{ChannelId as Id, Port};

    let id = Id::new(evering::REGION, 2, 3, 4, 5);
    let port = Port::from_parts(id, 1, 8).unwrap();
    let pool = ::evering::PoolId::new(evering::REGION, 5, 6, 7);
    let encoded = evering::bootstrap(&port, pool, 4096).unwrap();
    let (decoded, decoded_pool, decoded_extent) = evering::parse(encoded.as_ref()).unwrap();
    assert_eq!(decoded.id(), id);
    assert_eq!((decoded.role(), decoded.generation()), (1, 8));
    assert_eq!((decoded_pool, decoded_extent), (pool, 4096));
    let mut invalid = encoded.into_boxed();
    invalid[0] ^= 1;
    assert!(evering::parse(&invalid).is_err());
    assert!(
        evering::parse(
            evering::bootstrap(
                &Port::from_parts(Id::new(evering::REGION, 2, 3, 4, 0), 1, 8).unwrap(),
                pool,
                4096,
            )
            .unwrap()
            .as_ref()
        )
        .is_err()
    );
    assert!(evering::parse(evering::bootstrap(&port, pool, 0).unwrap().as_ref()).is_err());
}

#[test]
fn deterministic_validation_checks_every_response_byte() {
    let mut response = model::payload(7, 11, 4096);
    response.iter_mut().for_each(|byte| *byte ^= 0xa5);
    assert!(model::valid_response(7, 11, 4096, &response));
    response[2047] ^= 1;
    assert!(!model::valid_response(7, 11, 4096, &response));
    let mut truncated = model::payload(7, 11, 2048);
    truncated.iter_mut().for_each(|byte| *byte ^= 0xa5);
    assert!(!model::valid_response(7, 11, 4096, &truncated));
}

#[test]
fn framed_stream_round_trip_validates_complete_payload() {
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || stream::serve(listener.accept().unwrap().0).unwrap());
    let mut client = TcpStream::connect(address).unwrap();
    let payload = model::payload(7, 11, 4096);
    client.write_all(&11_u64.to_le_bytes()).unwrap();
    client
        .write_all(&(payload.len() as u32).to_le_bytes())
        .unwrap();
    client.write_all(&payload).unwrap();
    let mut header = [0; 12];
    client.read_exact(&mut header).unwrap();
    let mut response = vec![0; u32::from_le_bytes(header[8..].try_into().unwrap()) as usize];
    client.read_exact(&mut response).unwrap();
    assert_eq!(u64::from_le_bytes(header[..8].try_into().unwrap()), 11);
    assert!(model::valid_response(7, 11, 4096, &response));
    client.shutdown(Shutdown::Write).unwrap();
    worker.join().unwrap();
}

#[cfg(unix)]
#[test]
fn framing_is_transport_independent() {
    use std::{
        io::{Read, Write},
        net::Shutdown,
        os::unix::net::UnixStream,
    };

    let (mut client, server) = UnixStream::pair().unwrap();
    let worker = std::thread::spawn(move || stream::serve(server).unwrap());
    client.write_all(&7_u64.to_le_bytes()).unwrap();
    client.write_all(&3_u32.to_le_bytes()).unwrap();
    client.write_all(&[1, 2, 3]).unwrap();
    let mut response = [0; 15];
    client.read_exact(&mut response).unwrap();
    assert_eq!(&response[..12], &[7, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0]);
    assert_eq!(&response[12..], &[0xa4, 0xa7, 0xa6]);
    client.shutdown(Shutdown::Write).unwrap();
    worker.join().unwrap();
}

#[test]
fn framed_stream_ready_is_a_validated_round_trip() {
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::time::Duration;

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || stream::serve(listener.accept().unwrap().0).unwrap());
    let mut client = TcpStream::connect(address).unwrap();
    client
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    client.write_all(&u64::MAX.to_le_bytes()).unwrap();
    client.write_all(&0_u32.to_le_bytes()).unwrap();
    let mut response = [0; 12];
    client.read_exact(&mut response).unwrap();
    assert_eq!(
        u64::from_le_bytes(response[..8].try_into().unwrap()),
        u64::MAX
    );
    assert_eq!(u32::from_le_bytes(response[8..].try_into().unwrap()), 0);
    client.shutdown(Shutdown::Write).unwrap();
    worker.join().unwrap();
}

#[test]
fn readiness_stream_progresses_with_constrained_duplex_buffers() {
    use drive::Endpoint as _;
    use std::net::{TcpListener, TcpStream};
    use std::time::{Duration, Instant};

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        let stream = listener.accept().unwrap().0;
        let socket = socket2::SockRef::from(&stream);
        socket.set_send_buffer_size(4096).unwrap();
        socket.set_recv_buffer_size(4096).unwrap();
        stream::serve(stream).unwrap();
    });
    let client = TcpStream::connect(address).unwrap();
    let socket = socket2::SockRef::from(&client);
    socket.set_send_buffer_size(4096).unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    let mut client = stream::Client::<tokio::net::TcpStream>::new(client).unwrap();
    let deadline = drive::Deadline::after(Instant::now(), Duration::from_secs(5)).unwrap();
    drive::transfer(
        &mut client,
        drive::Work {
            start: u64::MAX,
            count: 1,
            window: 1,
            payload: 0,
            seed: 7,
        },
        deadline,
        &mut drive::Path::default(),
    )
    .unwrap();
    let mut path = drive::Path::default();
    let counts = drive::transfer(
        &mut client,
        drive::Work {
            start: 0,
            count: 32,
            window: 8,
            payload: 64 * 1024,
            seed: 7,
        },
        deadline,
        &mut path,
    )
    .unwrap();
    assert_eq!(
        (counts.accepted, counts.completed, counts.validated),
        (32, 32, 32)
    );
    client.abort().unwrap();
    worker.join().unwrap();
}

fn successful_trial() -> model::Trial {
    model::Trial {
        block: 0,
        order: 0,
        cell: model::Cell {
            arm: family::BUSY.key.into(),
            payload: 64,
            capacity: 8,
            in_flight: 3,
            memory: 4096,
        },
        requested: 7,
        accepted: 7,
        completed: 7,
        validated: 7,
        elapsed_ns: Some(1),
        phase_ns: [1; 3],
        observed: Some(model::Observed {
            payload: 64,
            capacity: 8,
            in_flight: 3,
            window: 3,
            topology: "1c1w".into(),
            transport: "shared-memory".into(),
            extent: Some(4096),
            allocator: Some("adaptive".into()),
            socket_send: None,
            socket_recv: None,
        }),
        path: drive::Path::default(),
        status: model::Status::Ok,
        error: None,
    }
}

fn study(trials: Vec<model::Trial>) -> model::Study {
    let expected = trials.len();
    let mut study = model::Study {
        meta: model::Meta {
            format: 5,
            family: "core-ipc".into(),
            family_revision: 1,
            revision: "abc123".into(),
            dirty: false,
            diff: "clean".into(),
            target: "x86_64-test".into(),
            os: "test".into(),
            arch: "x86_64".into(),
            rustc: "rustc-test".into(),
            command: "study --seed 7".into(),
            started: "0".into(),
            mode: "test".into(),
            seed: 7,
            warmup: 1,
            blocks: 1,
            timeout_ms: 1000,
            schedule: 0,
            expected,
            host: "test-host".into(),
            spin: 0,
        },
        trials,
    };
    study.meta.schedule = model::trial_schedule_id(&study.trials);
    study
}

fn condition() -> model::Condition {
    model::Condition {
        payload: 64,
        capacity: 8,
        in_flight: 3,
        memory: 4096,
    }
}

fn contrast(arm: &'static family::Arm) -> (model::Condition, &'static family::Arm) {
    (condition(), arm)
}

fn cell(arm: &'static family::Arm) -> model::Cell {
    condition().cell(arm)
}

#[test]
fn study_identity_must_resolve_its_exact_family_revision() {
    let mut evidence = study(vec![successful_trial()]);
    evidence.meta.family = "unknown".into();
    assert_eq!(
        model::validate_study(&evidence).unwrap_err(),
        model::StudyError::Family
    );
    evidence.meta.family = "core-ipc".into();
    evidence.meta.family_revision += 1;
    assert_eq!(
        model::validate_study(&evidence).unwrap_err(),
        model::StudyError::Family
    );
}

#[test]
fn schedule_shares_one_baseline_across_evering_policies() {
    let members = [
        contrast(&family::BUSY),
        contrast(&family::NOTIFIED),
        (condition(), &family::STREAM),
    ];
    let scheduled = model::schedule(&members, 3, 7);
    for block in 0..3 {
        let arms: Vec<_> = scheduled
            .iter()
            .filter(|trial| trial.block == block)
            .map(|trial| trial.arm.key)
            .collect();
        assert_eq!(arms.len(), 3);
        assert_eq!(
            arms.iter()
                .filter(|arm| **arm == family::STREAM.key)
                .count(),
            1
        );
        assert!(arms.contains(&family::BUSY.key));
        assert!(arms.contains(&family::NOTIFIED.key));
    }
}

#[test]
fn registered_families_have_exact_bounded_membership() {
    let screening = (family::CORE.members)("screening").unwrap();
    let focused = (family::CORE.members)("focused").unwrap();
    assert_eq!((screening.len(), focused.len()), (36, 10));
    assert_eq!(model::schedule(&screening, 3, 7).len(), 108);
    assert_eq!(model::schedule(&focused, 15, 7).len(), 150);
    assert!(
        focused
            .iter()
            .all(|(_, arm)| [family::ADAPTIVE.key, family::STREAM.key].contains(&arm.key))
    );
}

#[test]
fn schedule_randomizes_arm_order_without_changing_membership() {
    let members = [contrast(&family::ADAPTIVE), (condition(), &family::STREAM)];
    let first = model::schedule(&members, 8, 7);
    let second = model::schedule(&members, 8, 8);
    assert_eq!(first.len(), second.len());
    assert_ne!(
        first.iter().map(|entry| entry.arm.key).collect::<Vec<_>>(),
        second.iter().map(|entry| entry.arm.key).collect::<Vec<_>>()
    );
}

#[test]
fn stream_window_obeys_capacity_and_in_flight() {
    assert_eq!(model::window(100, 1, 64), 1);
    assert_eq!(model::window(100, 8, 64), 8);
    assert_eq!(model::window(3, 8, 64), 3);
}

#[test]
fn duplicate_baseline_rows_are_rejected() {
    let mut busy = successful_trial();
    busy.cell.arm = family::STREAM.key.into();
    busy.observed = Some(stream_observed());
    let mut duplicate = busy.clone();
    duplicate.order = 1;
    assert_eq!(
        model::validate_study(&study(vec![busy, duplicate])).unwrap_err(),
        model::StudyError::DuplicateCell
    );
}

#[test]
fn analysis_reuses_one_baseline_for_every_policy_in_a_condition() {
    let evidence = registered_study("screening", &[2.0]);
    let analysis = analysis::analyze(&evidence).unwrap();
    assert_eq!(
        analysis
            .estimates
            .iter()
            .filter(|estimate| {
                estimate.condition.payload == 64
                    && estimate.condition.capacity == 8
                    && estimate.condition.in_flight == 8
            })
            .map(|estimate| (estimate.candidate, estimate.effect))
            .collect::<Vec<_>>(),
        vec![
            (family::ADAPTIVE.key, 2.0),
            (family::BUSY.key, 2.0),
            (family::NOTIFIED.key, 2.0),
        ]
    );
}

fn stream_observed() -> model::Observed {
    model::Observed {
        payload: 64,
        capacity: 8,
        in_flight: 3,
        window: 3,
        topology: "1c1w".into(),
        transport: "ipv4-loopback".into(),
        extent: None,
        allocator: None,
        socket_send: Some(4096),
        socket_recv: Some(4096),
    }
}

fn registered_study(mode: &str, ratios: &[f64]) -> model::Study {
    let blocks = family::CORE.mode(mode).unwrap().1;
    let scheduled = model::schedule(&(family::CORE.members)(mode).unwrap(), blocks, 7);
    let trials = scheduled
        .into_iter()
        .map(|entry| {
            let mut trial = successful_trial();
            trial.block = entry.block;
            trial.order = entry.order;
            trial.cell = entry.cell();
            let observed = if entry.arm.key == family::STREAM.key {
                stream_observed()
            } else {
                trial.observed.take().unwrap()
            };
            trial.observed = Some(model::Observed {
                payload: trial.cell.payload,
                capacity: trial.cell.capacity,
                in_flight: trial.cell.in_flight,
                window: model::window(trial.requested, trial.cell.capacity, trial.cell.in_flight),
                extent: (entry.arm.key != family::STREAM.key).then_some(trial.cell.memory),
                ..observed
            });
            let ratio = ratios[entry.block as usize % ratios.len()];
            trial.elapsed_ns = Some(
                if entry.arm.key == family::STREAM.key {
                    ratio
                } else {
                    1.0
                }
                .mul_add(1_000_000.0, 0.0) as u64,
            );
            trial
        })
        .collect();
    let mut evidence = study(trials);
    evidence.meta.mode = mode.into();
    evidence.meta.blocks = blocks;
    evidence.meta.expected = evidence.trials.len();
    evidence.meta.schedule = model::trial_schedule_id(&evidence.trials);
    evidence
}

#[test]
fn paired_analysis_is_order_independent_and_hand_checked() {
    let evidence = registered_study("focused", &[2.0]);
    let analysis = analysis::analyze(&evidence).unwrap();
    assert_eq!(analysis.estimates.len(), 5);
    let estimate = &analysis.estimates[0];
    assert_eq!(estimate.blocks, 15);
    assert_eq!(
        analysis.authority,
        analysis::Authority::Focused(vec![analysis::Decision::Faster; 5])
    );
    assert!((estimate.effect - 2.0).abs() < 1e-12);
    assert!((estimate.low - 2.0).abs() < 1e-12);
    assert!((estimate.high - 2.0).abs() < 1e-12);

    let mut reversed = evidence.clone();
    reversed.trials.reverse();
    reversed.meta.schedule = model::trial_schedule_id(&reversed.trials);
    assert_eq!(
        analysis::analyze(&reversed).unwrap().estimates,
        analysis.estimates
    );
}

#[test]
fn analysis_classifies_registered_band_and_rejects_bad_admission() {
    let classify = |ratios: &[f64]| match analysis::analyze(&registered_study("focused", ratios))
        .unwrap()
        .authority
    {
        analysis::Authority::Focused(decisions) => decisions[0],
        analysis::Authority::Screening => unreachable!(),
    };
    assert_eq!(classify(&[0.97, 1.0, 1.03]), analysis::Decision::Equivalent);
    assert_eq!(
        classify(&[0.90, 1.0, 1.10]),
        analysis::Decision::Inconclusive
    );
    assert_eq!(classify(&[0.8]), analysis::Decision::Slower);

    let mut failed = registered_study("focused", &[1.0]);
    failed.trials[0].status = model::Status::TimedError;
    failed.trials[0].elapsed_ns = None;
    failed.trials[0].error = Some("timeout".into());
    failed.meta.schedule = model::trial_schedule_id(&failed.trials);
    assert!(matches!(
        analysis::analyze(&failed),
        Err(analysis::Error::Pair)
    ));

    let first = registered_study("focused", &[1.0]);
    let mut other_platform = first.clone();
    other_platform.meta.os = "other".into();
    assert_eq!(
        analysis::report(&[first.clone(), other_platform]).unwrap_err(),
        analysis::Error::Duplicate
    );

    let mut subset = first.clone();
    subset.trials.retain(|trial| trial.cell.payload == 0);
    for block in 0..subset.meta.blocks {
        subset
            .trials
            .iter_mut()
            .filter(|trial| trial.block == block)
            .enumerate()
            .for_each(|(order, trial)| trial.order = order as u32);
    }
    subset.meta.expected = subset.trials.len();
    subset.meta.schedule = model::trial_schedule_id(&subset.trials);
    assert!(model::validate_study(&subset).is_ok());
    assert!(matches!(
        analysis::analyze(&subset),
        Err(analysis::Error::Family)
    ));

    let mut missing = first.clone();
    missing.trials.pop();
    assert!(matches!(
        analysis::analyze(&missing),
        Err(analysis::Error::Study(model::StudyError::Incomplete))
    ));
    let mut duplicate = first;
    duplicate.trials.push(duplicate.trials[0].clone());
    duplicate.meta.expected += 1;
    duplicate.meta.schedule = model::trial_schedule_id(&duplicate.trials);
    assert!(matches!(
        analysis::analyze(&duplicate),
        Err(analysis::Error::Study(model::StudyError::DuplicateCell))
    ));
}

#[test]
fn screening_has_estimates_but_no_decision_authority() {
    let evidence = registered_study("screening", &[2.0]);
    let markdown = analysis::report(core::slice::from_ref(&evidence)).unwrap();
    assert_eq!(markdown, analysis::report(&[evidence]).unwrap());
    assert!(
        ["core-ipc/v1", "2.000000", "tcp/readiness", "Limit: 180 s"]
            .iter()
            .all(|value| markdown.contains(value))
    );
    assert!(markdown.contains("Screening is descriptive"));
    assert!(!markdown.contains("| decision |"));
    assert!(!markdown.contains("Faster"));
}

#[cfg(feature = "plot")]
#[test]
fn plot_is_separate_stable_descriptive_and_non_overwriting() {
    let study = registered_study("screening", &[1.5, 2.0, 2.5]);
    let markdown = analysis::report(core::slice::from_ref(&study)).unwrap();
    let root = std::env::temp_dir().join(format!(
        "evering-plot-{}-{}",
        std::process::id(),
        fastrand::u64(..)
    ));
    std::fs::create_dir(&root).unwrap();
    let first = root.join("first");
    let second = root.join("second");
    let paths = plot::render(&first, core::slice::from_ref(&study)).unwrap();
    assert_eq!(paths, [first.join("core-ipc.svg")]);
    let svg = std::fs::read(&paths[0]).unwrap();
    let repeated = plot::render(&second, core::slice::from_ref(&study)).unwrap();
    assert_eq!(svg, std::fs::read(&repeated[0]).unwrap());
    let text = std::str::from_utf8(&svg).unwrap();
    assert!(
        [
            "<svg",
            "core-ipc/v1",
            "screening descriptive only",
            "TCP readiness",
            "Evering adaptive",
            "empty payload",
            "queue 8",
            "8 in flight",
            "shared memory"
        ]
        .iter()
        .all(|value| text.contains(value))
    );
    assert!(
        !["c8", "f8", "m4198400"]
            .iter()
            .any(|value| text.contains(value))
    );
    assert!(
        !["Faster", "Slower", "Equivalent"]
            .iter()
            .any(|value| text.contains(value))
    );
    assert_eq!(
        markdown,
        analysis::report(core::slice::from_ref(&study)).unwrap()
    );
    assert!(plot::render(&first, core::slice::from_ref(&study)).is_err());
    assert!(plot::render(&root.join("duplicate"), &[study.clone(), study]).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn micro_evidence_requires_one_reset_transition_per_iteration() {
    use std::time::Duration;

    let row = micro::measure(micro::Mechanism::ReservePublish, 3, "capacity=8", |_| {
        Ok(micro::Sample {
            elapsed: Duration::from_nanos(7),
            operations: 1,
            reset: true,
        })
    })
    .unwrap();
    assert_eq!(row.iterations, 3);
    assert_eq!(row.gross_ns, 21);
    assert_eq!(row.net_ns, 21_i128 - row.control_ns as i128);
    assert!(
        row.encode()
            .starts_with("MICRO\treserve-publish\tsame-process\t")
    );
    assert!(model::decode(&row.encode()).is_err());

    for sample in [
        micro::Sample {
            elapsed: Duration::ZERO,
            operations: 0,
            reset: true,
        },
        micro::Sample {
            elapsed: Duration::from_nanos(1),
            operations: 1,
            reset: false,
        },
    ] {
        assert!(
            micro::measure(micro::Mechanism::ClaimRecycle, 1, "capacity=8", |_| {
                Ok(sample)
            })
            .is_err()
        );
    }
    let negative = micro::measure(micro::Mechanism::SignalConsume, 1, "sticky=native", |_| {
        Ok(micro::Sample {
            elapsed: Duration::ZERO,
            operations: 1,
            reset: true,
        })
    })
    .unwrap();
    assert_eq!(negative.net_ns, -(negative.control_ns as i128));
    let notify = micro::measure(micro::Mechanism::Notify, 1, "sticky=native", |_| {
        Ok(micro::Sample {
            elapsed: Duration::from_nanos(1),
            operations: 1,
            reset: true,
        })
    })
    .unwrap();
    assert!(notify.encode().starts_with("MICRO\tnotify\tsame-process\t"));
    assert!(
        micro::measure(micro::Mechanism::AllocateRelease, 1, "bytes=64", |_| {
            Err("failed operation".into())
        })
        .is_err()
    );
}

#[test]
fn seeded_blocks_are_reproducible_but_not_fixed_order() {
    let contrasts = [
        contrast(&family::BUSY),
        contrast(&family::ADAPTIVE),
        contrast(&family::NOTIFIED),
    ];
    let first = model::schedule(&contrasts, 2, 7);
    assert_eq!(
        model::schedule_id(&first),
        model::schedule_id(&model::schedule(&contrasts, 2, 7))
    );
    assert_ne!(
        model::schedule_id(&first),
        model::schedule_id(&model::schedule(&contrasts, 2, 8))
    );
}

#[test]
fn study_rejects_duplicate_block_cell_rows() {
    let trial = successful_trial();
    assert_eq!(
        model::validate_study(&study(vec![trial.clone(), trial])).unwrap_err(),
        model::StudyError::DuplicateCell
    );
}

#[test]
fn recorder_persists_rows_and_seals_the_final_path() {
    use std::fs;

    let root = std::env::temp_dir().join(format!(
        "evering-study-{}-{}",
        std::process::id(),
        fastrand::u64(..)
    ));
    fs::create_dir_all(&root).unwrap();
    let final_path = root.join("evidence.jsonl");
    let mut trial = successful_trial();
    trial.path = drive::Path {
        send_stalled: true,
        recv_stalled: true,
        wait_entered: true,
        wait_returned: true,
        stale_wake: true,
        partial_io: true,
    };
    let evidence = study(vec![trial]);

    model::record(
        &final_path,
        evidence.meta.clone(),
        evidence.trials.iter().cloned().map(Ok),
    )
    .unwrap();
    let complete = fs::read_to_string(&final_path).unwrap();
    assert_eq!(model::decode(&complete).unwrap(), evidence);
    let mismatched = complete.replacen("\"rows\":1", "\"rows\":2", 1);
    assert_eq!(
        model::decode(&mismatched).unwrap_err(),
        model::CodecError::Study(model::StudyError::Incomplete)
    );
    assert!(
        model::record(
            &final_path,
            evidence.meta,
            std::iter::empty::<Result<model::Trial, String>>(),
        )
        .is_err()
    );

    fs::remove_file(final_path).unwrap();
    fs::remove_dir(root).unwrap();
}

#[test]
fn recorder_stops_before_evaluating_work_after_a_mandatory_failure() {
    use std::{cell::Cell, fs, rc::Rc};

    let root = std::env::temp_dir().join(format!(
        "evering-fail-fast-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    let final_path = root.join("evidence.jsonl");
    let mut failed = successful_trial();
    failed.status = model::Status::TimedError;
    failed.error = Some("deadline".into());
    failed.elapsed_ns = None;
    let evidence = study(vec![failed.clone()]);
    let evaluated = Rc::new(Cell::new(0));
    let later = Rc::clone(&evaluated);
    let trials = std::iter::once(Ok(failed)).chain(std::iter::once_with(move || {
        later.set(later.get() + 1);
        Ok(successful_trial())
    }));

    assert!(model::record(&final_path, evidence.meta, trials).is_err());
    assert_eq!(evaluated.get(), 0);
    let partial = fs::read_to_string(&final_path).unwrap();
    assert!(!partial.contains("\"kind\":\"end\""));
    assert_eq!(
        model::decode_prefix(&partial).unwrap().trials,
        evidence.trials
    );

    fs::remove_file(final_path).unwrap();
    fs::remove_dir(root).unwrap();

    let root = std::env::temp_dir().join(format!(
        "evering-invalid-fast-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    let final_path = root.join("evidence.jsonl");
    let mut invalid = successful_trial();
    invalid.completed = invalid.accepted + 1;
    let meta = study(vec![invalid.clone()]).meta;
    let evaluated = Rc::new(Cell::new(0));
    let later = Rc::clone(&evaluated);
    let trials = std::iter::once(Ok(invalid)).chain(std::iter::once_with(move || {
        later.set(later.get() + 1);
        Ok(successful_trial())
    }));
    assert!(model::record(&final_path, meta, trials).is_err());
    assert_eq!(evaluated.get(), 0);
    fs::remove_file(final_path).unwrap();
    fs::remove_dir(root).unwrap();
}

#[test]
fn dropped_recorder_leaves_an_unsealed_final_path() {
    use std::{fs, io::Write};

    let root = std::env::temp_dir().join(format!("evering-study-drop-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let final_path = root.join("evidence.jsonl");
    let evidence = study(vec![successful_trial()]);
    assert!(
        model::record(
            &final_path,
            evidence.meta.clone(),
            [Ok(evidence.trials[0].clone()), Err("process cut".into()),],
        )
        .is_err()
    );
    let loaded = model::load(&final_path).unwrap();
    assert!(!loaded.complete);
    assert_eq!(loaded.study, evidence);

    fs::OpenOptions::new()
        .append(true)
        .open(&final_path)
        .unwrap()
        .write_all(b"{\"kind\"")
        .unwrap();
    assert!(model::load(&final_path).is_err());
    assert!(
        model::record(
            &final_path,
            study(Vec::new()).meta,
            std::iter::empty::<Result<model::Trial, String>>(),
        )
        .is_err()
    );

    fs::remove_file(final_path).unwrap();
    fs::remove_dir(root).unwrap();
}

#[test]
fn unsealed_path_never_authorizes_evidence_at_any_persistence_cut() {
    use std::{fs, io::Write};

    let root = std::env::temp_dir().join(format!("evering-study-cuts-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let mut second = successful_trial();
    second.order = 1;
    second.cell.arm = family::ADAPTIVE.key.into();
    let evidence = study(vec![successful_trial(), second]);
    for cut in 0..=evidence.trials.len() {
        let path = root.join(format!("cut-{cut}.jsonl"));
        let trials = evidence
            .trials
            .iter()
            .take(cut)
            .cloned()
            .map(Ok)
            .chain(std::iter::once(Err("cut".into())));
        assert!(model::record(&path, evidence.meta.clone(), trials).is_err());
        let loaded = model::load(&path).unwrap();
        assert!(!loaded.complete);
        assert_eq!(loaded.study.trials.len(), cut);
        writeln!(
            fs::OpenOptions::new().append(true).open(&path).unwrap(),
            "{{}}"
        )
        .unwrap();
        assert!(model::load(&path).is_err());
        fs::remove_file(path).unwrap();
    }
    fs::remove_dir(root).unwrap();
}

#[test]
fn actual_configuration_is_required_after_setup() {
    let mut trial = successful_trial();
    trial.observed = None;
    assert_eq!(
        model::validate_trial(&trial).unwrap_err(),
        model::TrialError::MissingObserved
    );
    trial.status = model::Status::TimedError;
    trial.elapsed_ns = None;
    trial.error = Some("timed out".into());
    assert_eq!(
        model::validate_trial(&trial).unwrap_err(),
        model::TrialError::MissingObserved
    );
}

#[test]
fn observed_configuration_must_exactly_match_the_scheduled_trial() {
    let mut payload = successful_trial();
    payload.observed.as_mut().unwrap().payload += 1;
    assert_eq!(
        model::validate_study(&study(vec![payload])).unwrap_err(),
        model::StudyError::Trial(model::TrialError::InvalidCell)
    );

    let mut window = successful_trial();
    window.observed.as_mut().unwrap().window -= 1;
    assert_eq!(
        model::validate_study(&study(vec![window])).unwrap_err(),
        model::StudyError::Trial(model::TrialError::InvalidCell)
    );
}

#[test]
fn family_rejects_unknown_arms_and_undeclared_resources() {
    let mut unknown = successful_trial();
    unknown.cell.arm = "foreign/arm".into();
    assert_eq!(
        model::validate_study(&study(vec![unknown])).unwrap_err(),
        model::StudyError::Family
    );

    let mut extra = successful_trial();
    extra.observed.as_mut().unwrap().socket_send = Some(4096);
    assert_eq!(
        model::validate_study(&study(vec![extra])).unwrap_err(),
        model::StudyError::Trial(model::TrialError::InvalidCell)
    );
}

#[test]
fn schema5_rejects_corrupt_metadata_seal_and_elapsed_value() {
    assert_eq!(
        model::decode_prefix("{}").unwrap_err(),
        model::CodecError::Syntax
    );
    let mut zero = successful_trial();
    zero.elapsed_ns = Some(0);
    assert_eq!(
        model::validate_trial(&zero).unwrap_err(),
        model::TrialError::ZeroElapsed
    );

    let first = successful_trial();
    let mut second = first.clone();
    second.order = 1;
    second.cell.arm = family::ADAPTIVE.key.into();
    let mut missing = study(vec![first, second]);
    missing.trials.pop();
    assert_eq!(
        model::validate_study(&missing).unwrap_err(),
        model::StudyError::Incomplete
    );

    let root = std::env::temp_dir().join(format!("evering-study-footer-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let final_path = root.join("evidence.jsonl");
    let evidence = study(vec![successful_trial()]);
    model::record(
        &final_path,
        evidence.meta,
        evidence.trials.into_iter().map(Ok),
    )
    .unwrap();
    let mut complete = std::fs::read_to_string(&final_path).unwrap();
    complete.truncate(complete.len() - 2);
    assert!(model::decode(&complete).is_err());
    std::fs::remove_file(final_path).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[test]
fn warmup_consumption_cannot_reset_the_measured_deadline() {
    let began = std::time::Instant::now();
    let error = drive::measure(
        &mut TraceEndpoint {
            delay_ready: true,
            ..TraceEndpoint::default()
        },
        drive::Work {
            start: 0,
            count: 1,
            window: 1,
            payload: 1,
            seed: 7,
        },
        1,
        began,
        drive::Deadline::after(began, std::time::Duration::from_millis(2)).unwrap(),
    )
    .err()
    .unwrap();
    assert!(error.timed);
    assert_eq!(error.error.kind, drive::Kind::Deadline);
}

#[test]
fn study_requires_complete_blocks_and_environment_identity() {
    let mut first = successful_trial();
    let mut second = successful_trial();
    second.block = 1;
    let mut study = study(vec![first.clone(), second]);
    study.meta.blocks = 2;
    assert!(model::validate_study(&study).is_ok());

    first.cell.capacity = 16;
    first.observed.as_mut().unwrap().capacity = 16;
    first.order = 1;
    study.trials.push(first);
    study.meta.expected = 3;
    study.meta.schedule = model::trial_schedule_id(&study.trials);
    assert_eq!(
        model::validate_study(&study).unwrap_err(),
        model::StudyError::IncompleteBlock
    );

    study.trials.pop();
    study.meta.revision.clear();
    assert_eq!(
        model::validate_study(&study).unwrap_err(),
        model::StudyError::Metadata
    );
}

#[test]
fn successful_trial_requires_exact_conserved_work() {
    let mut trial = successful_trial();
    trial.accepted -= 1;
    trial.completed -= 1;
    trial.validated -= 1;
    assert_eq!(
        model::validate_trial(&trial).unwrap_err(),
        model::TrialError::CountMismatch
    );
}

#[test]
fn trial_status_cannot_disguise_failure_as_timing() {
    let mut trial = successful_trial();
    trial.status = model::Status::TimedError;
    trial.error = Some("worker exited".into());
    assert_eq!(
        model::validate_trial(&trial).unwrap_err(),
        model::TrialError::UnexpectedElapsed
    );

    trial.elapsed_ns = None;
    assert!(model::validate_trial(&trial).is_ok());
}

#[test]
fn unsupported_cell_has_reason_and_no_work_or_timing() {
    let mut trial = successful_trial();
    trial.status = model::Status::Unsupported;
    trial.accepted = 0;
    trial.completed = 0;
    trial.validated = 0;
    trial.elapsed_ns = None;
    trial.error = Some("policy unavailable".into());
    assert!(model::validate_trial(&trial).is_ok());

    trial.error = None;
    assert_eq!(
        model::validate_trial(&trial).unwrap_err(),
        model::TrialError::MissingError
    );
}

#[test]
fn every_failure_phase_has_no_performance_value() {
    for status in [
        model::Status::Invalid,
        model::Status::SetupError,
        model::Status::TimedError,
        model::Status::DrainError,
    ] {
        let mut trial = successful_trial();
        trial.status = status;
        trial.elapsed_ns = None;
        trial.error = Some("phase failed".into());
        assert!(model::validate_trial(&trial).is_ok());
    }
}
