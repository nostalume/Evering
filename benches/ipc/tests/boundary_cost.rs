//! Cost attributed to a boundary: procfs text parsed without a libc dependency, a
//! delta that discards regressions and mixed resolutions, accounting admitted only
//! inside the timed window, the sample pair that proves the counters see a phase at
//! all, and the pairing that decides whether the analysis may report the figures.
//! The batched publish group is the claim the instrument must be able to carry.

use crate::analysis;
use crate::tests::evidence_attribution::{
    system_evidence, system_evidence_with, system_header, system_measure,
};
use crate::{boundary, family, study, system};

#[test]
fn procfs_text_parsing_keeps_tick_and_nanosecond_resolution_apart() {
    assert_eq!(
        boundary::text::schedstat("987654321 12 34\n"),
        Some((987_654, boundary::Source::Nanoseconds))
    );
    assert_eq!(boundary::text::schedstat("run\n"), None);
    // The command name is parenthesised and may contain the separator, so the
    // tick fields are only addressable from the end of the line.
    let ticks = "9 (worker) S 1 2 3 4 5 6 7 8 9 10 100 200 13 14 15\n";
    let embedded = "9 (a) b) S 1 2 3 4 5 6 7 8 9 10 100 200 13 14 15\n";
    let expected = Some((3_000_000, boundary::Source::Ticks));
    assert_eq!(boundary::text::stat(ticks), expected);
    assert_eq!(boundary::text::stat(embedded), expected);
    assert_eq!(boundary::text::stat("9 (worker S 1 2\n"), None);
    assert_eq!(boundary::text::stat("9 (worker) S 1 2 3 4\n"), None);
    let status = "Name:\tevering\nvoluntary_ctxt_switches:\t41\nnonvoluntary_ctxt_switches:\t7\n";
    assert_eq!(boundary::text::switches(status), Some((41, 7)));
    let reversed = "nonvoluntary_ctxt_switches:\t7\nvoluntary_ctxt_switches:\t41\n";
    assert_eq!(boundary::text::switches(reversed), Some((41, 7)));
    assert_eq!(
        boundary::text::switches("nonvoluntary_ctxt_switches:\t7\n"),
        None
    );
}

#[test]
fn sample_delta_rejects_regressions_and_mixed_resolutions() {
    let before = boundary::Sample {
        runtime: (1_000, boundary::Source::Nanoseconds),
        voluntary: 10,
        involuntary: 2,
    };
    let after = boundary::Sample {
        runtime: (2_500, boundary::Source::Nanoseconds),
        voluntary: 14,
        involuntary: 2,
    };
    assert_eq!(
        before.since(after).unwrap(),
        boundary::Accounting {
            source: boundary::Source::Nanoseconds,
            runtime_us: 1_500,
            voluntary_switches: 4,
            involuntary_switches: 0,
        }
    );
    assert!(after.since(before).is_none());
    let coarser = boundary::Sample {
        runtime: (3_000, boundary::Source::Ticks),
        ..after
    };
    assert!(before.since(coarser).is_none());
}

#[test]
fn boundary_accounting_is_admitted_only_within_the_timed_window() {
    let header = system_header("focused");
    let execution = header.run.schedule[0];
    let case = header.cases[execution.case as usize].clone();
    let admit = |cost| {
        let mut measure = system_measure(&case, 2_000_000);
        measure.boundary = Some(cost);
        <system::System as study::Schema>::validate(
            &header,
            &study::Observation {
                unit: execution.unit,
                case: execution.case,
                measure,
            },
        )
    };
    let cost = |source, runtime_us| boundary::Accounting {
        source,
        runtime_us,
        voluntary_switches: 0,
        involuntary_switches: 0,
    };
    let nanoseconds = boundary::Source::Nanoseconds;
    let ticks = boundary::Source::Ticks;
    // The sample window sits inside the timed window, so attributed run time
    // cannot exceed the phase that contains it.
    assert!(admit(cost(nanoseconds, 2_001)).is_err());
    assert!(admit(cost(ticks, 2_001)).is_err());
    // Either source may floor to zero: the kernel refreshes run time at
    // scheduling events, so this bounds the claim, not the validity.
    admit(cost(nanoseconds, 0)).unwrap();
    admit(cost(ticks, 0)).unwrap();
    admit(cost(nanoseconds, 1_999)).unwrap();
}

#[test]
fn measure_round_trips_an_absent_and_a_present_boundary() {
    let header = system_header("focused");
    let case = header.cases[header.run.schedule[0].case as usize].clone();
    let mut measure = system_measure(&case, 2_000_000);
    let encoded = serde_json::to_string(&measure).unwrap();
    assert!(!encoded.contains("boundary"));
    assert_eq!(
        serde_json::from_str::<system::Measure>(&encoded).unwrap(),
        measure
    );
    measure.boundary = Some(boundary::Accounting {
        source: boundary::Source::Nanoseconds,
        runtime_us: 1_500,
        voluntary_switches: 4,
        involuntary_switches: 1,
    });
    let encoded = serde_json::to_string(&measure).unwrap();
    assert!(encoded.contains("boundary"));
    assert_eq!(
        serde_json::from_str::<system::Measure>(&encoded).unwrap(),
        measure
    );
}

/// The second estimand reaches the report through the same pairing as the ratio: every
/// block of the contrast must carry a figure on both arms at one resolution, because a
/// half-paired block says nothing about the boundary and a subset of blocks would be a
/// different sample than the ratio the figures sit beside.
#[test]
fn paired_boundary_figures_reach_the_report_and_an_unpaired_contrast_reports_none() {
    let cost = |case: &system::Case| {
        let baseline = case.workload.arm == family::CORE.baseline.key;
        Some(boundary::Accounting {
            source: boundary::Source::Nanoseconds,
            runtime_us: if baseline { 800 } else { 400 },
            voluntary_switches: if baseline { 100 } else { 25 },
            involuntary_switches: if baseline { 3 } else { 1 },
        })
    };
    let evidence = system_evidence_with("focused", 1.2, |case, _| cost(case));
    let analysis = analysis::analyze(&evidence).unwrap();
    for estimate in &analysis.estimates {
        // Every trial in this fixture runs one hundred operations, so the reported
        // figures are the row values divided by a known count.
        let cost = estimate.boundary.expect("both arms record a boundary");
        assert_eq!(cost.blocks, estimate.blocks);
        assert_eq!(cost.source, boundary::Source::Nanoseconds);
        assert_eq!(cost.candidate_runtime_us, 4.0);
        assert_eq!(cost.baseline_runtime_us, 8.0);
        assert_eq!(cost.runtime_delta_us, -4.0);
        assert_eq!(cost.candidate_voluntary, 0.25);
        assert_eq!(cost.baseline_voluntary, 1.0);
        assert_eq!(cost.candidate_involuntary, 0.01);
        assert_eq!(cost.baseline_involuntary, 0.03);
        assert!((cost.candidate_wall_us - 8.33333).abs() < 1e-4);
        assert_eq!(cost.baseline_wall_us, 10.0);
    }
    assert!(
        analysis
            .markdown()
            .contains("Boundary cost per operation, paired over")
    );

    // One arm silent and the contrast reports nothing at all.
    let one_sided = system_evidence_with("focused", 1.2, |case, _| {
        (case.workload.arm != family::CORE.baseline.key).then(|| cost(case).unwrap())
    });
    let analysis = analysis::analyze(&one_sided).unwrap();
    assert!(
        analysis
            .estimates
            .iter()
            .all(|estimate| estimate.boundary.is_none())
    );
    assert!(!analysis.markdown().contains("Boundary cost"));

    // Rows written before the instrument existed analyze exactly as they did, with no
    // second section appended.
    let uninstrumented = system_evidence("focused", 1.2);
    let analysis = analysis::analyze(&uninstrumented).unwrap();
    assert!(
        analysis
            .estimates
            .iter()
            .all(|estimate| estimate.boundary.is_none())
    );
    assert!(!analysis.markdown().contains("Boundary cost"));
}

/// Resolution, stated because the two windows below are only evidence with it: the switch
/// counts come from `/proc/thread-self/status`, which the kernel keeps per task, and the
/// run time from `/proc/thread-self/schedstat` in nanoseconds — or from
/// `/proc/thread-self/stat` in whole 10 ms ticks when the host exports no `schedstat`,
/// which is what [`boundary::Source`] records on every figure. Nothing here reads
/// `/proc/self`: those counters name the thread-group leader, while a `#[test]` body runs
/// on a harness thread, so a leader-wide reading only moves when some other task happens
/// to block. That is a fact about the suite's schedule rather than about the instrument,
/// and it is why both windows sample the task that runs the phase.
#[cfg(target_os = "linux")]
#[test]
fn procfs_sample_sees_a_blocking_phase() {
    // One pure-CPU phase: hold the core, then read the counters of this task.
    fn cpu_window() -> boundary::Sample {
        let spun = std::time::Instant::now();
        while spun.elapsed() < std::time::Duration::from_millis(20) {
            std::hint::spin_loop();
        }
        boundary::sample_here().expect("the same task is still readable")
    }

    // A test body starts while the process is still settling: code is faulted in and the
    // first reads of `/proc/thread-self` size their own buffers, and either can block the
    // task even though the phase is pure CPU. One window is spent paying that before the
    // control is read, so what it reports belongs to the phase and not to start-up.
    cpu_window();

    let before = boundary::sample_here().expect("procfs is readable");
    // The CPU-only phase is the control. Run time is what it earns and a voluntary switch
    // is what it must not earn, so the switch the next phase reports is credited to
    // blocking rather than to scheduling in general.
    let after_cpu = cpu_window();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let after_block = boundary::sample_here().expect("the same task is still readable");

    let cpu = before
        .since(after_cpu)
        .expect("two readings of one task are comparable");
    assert!(
        cpu.runtime_us > 0,
        "a phase that holds the core must register run time ({:?})",
        cpu.source
    );
    assert_eq!(
        cpu.voluntary_switches, 0,
        "a phase that never blocked must not log a voluntary switch"
    );
    let blocking = after_cpu
        .since(after_block)
        .expect("two readings of one task are comparable");
    assert!(
        blocking.voluntary_switches > 0,
        "a blocking phase must register a voluntary switch"
    );

    let zeros = after_block.since(after_block).unwrap();
    assert_eq!(
        (
            zeros.runtime_us,
            zeros.voluntary_switches,
            zeros.involuntary_switches
        ),
        (0, 0, 0)
    );
}

/// Stage 1.3's demo on the real provider. Stage 0.3a counted two doorbells per
/// operation for the shared-memory arm: one when the producer publishes, one when
/// the consumer recycles the slot. Batching spends one of each per group instead,
/// so the same 40 operations cost `2 * groups` plus `2 * operations` entries rather
/// than `2 * operations` everywhere. The counting adapter wraps the eventfd ring,
/// so every entry it records is a real `write(2)` into the peer's latch, and the
/// phase that batched is still admitted by the finished Stage 0 instrument.
#[cfg(target_os = "linux")]
#[test]
fn a_batched_publish_group_costs_one_doorbell_and_the_instrument_admits_it() {
    use crate::evering_arm;
    use std::{cell::Cell, time::Instant};

    use ::evering::{
        Session,
        layout::{Repr, SchemaId, SchemaKey},
        mapping::{Access, Request},
        notify::{Notify, Signals, Wait as _},
        os, runtime,
    };

    #[derive(Debug)]
    #[repr(C)]
    struct Door(u64);

    unsafe impl Repr for Door {
        const SCHEMA: SchemaKey = SchemaKey::new(SchemaId(0x73315f335f646f6f), 1);
    }

    struct Counted<'r>(&'r os::Ring, Cell<usize>);

    impl Notify for Counted<'_> {
        type Error = <os::Ring as Notify>::Error;

        fn notify(&self) -> Result<(), Self::Error> {
            self.1.set(self.1.get() + 1);
            self.0.notify()
        }
    }

    const PER_GROUP: u64 = 8;
    const GROUPS: u64 = 4;

    let source = os::unix::UnixFd::memfd("evering-s1-3", 1 << 20, false).unwrap();
    let request = Request::new(1 << 20, Access::READ | Access::WRITE);
    let writer = Session::create(source.borrow(), request, evering_arm::REGION).unwrap();
    let reader = Session::open(source.borrow(), request, evering_arm::REGION).unwrap();
    let pool = writer.create_pool(64 * 1024, None).unwrap();
    let reader_pool = reader.open_pool(pool.id()).unwrap();
    let (write_channel, port) = writer.create_channel::<Door>(PER_GROUP as usize).unwrap();
    let read_channel = reader.adopt(port).unwrap();
    let (tx, _) = write_channel.split();
    let (_, rx) = read_channel.split();

    let (ring, event) = os::event().unwrap();
    let traps = Counted(&ring, Cell::new(0));
    let signals = Signals::notify(&traps);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let (boundary, elapsed) = runtime.block_on(async move {
        let wait = runtime::Wait::new(event).unwrap();
        // The bracket belongs to the task that serves the phase: this exchange runs on the
        // harness thread, and the leader spends the same window blocked in the harness, so
        // a leader-wide reading would cost the batch to a task that never ran it.
        let before = boundary::sample_here();
        let started = Instant::now();

        let mut batch = signals.defer();
        for group in 0..GROUPS {
            for index in 0..PER_GROUP {
                let ordinal = group * PER_GROUP + index;
                signals
                    .try_publish(
                        &tx,
                        pool.as_ref().put(ordinal).unwrap().transfer(Door(ordinal)),
                        &mut batch,
                    )
                    .unwrap();
            }
            batch.deliver().unwrap();
            wait.wait().await.unwrap();
            for index in 0..PER_GROUP {
                let door = batch
                    .discard(rx.claim().unwrap(), reader_pool.as_ref())
                    .unwrap();
                assert_eq!(door.0, group * PER_GROUP + index);
            }
            batch.deliver().unwrap();
        }

        // The same exchange rung per operation, as the notified arm does today.
        for ordinal in 0..PER_GROUP {
            signals
                .try_send(
                    &tx,
                    pool.as_ref().put(ordinal).unwrap().transfer(Door(ordinal)),
                )
                .unwrap();
            wait.wait().await.unwrap();
            let door = signals
                .discard(rx.claim().unwrap(), reader_pool.as_ref())
                .unwrap()
                .into_parts()
                .0;
            assert_eq!(door.0, ordinal);
        }
        drop(batch);

        let after = boundary::sample_here();
        (
            after.and_then(|after| before.and_then(|before| before.since(after))),
            Instant::now().duration_since(started),
        )
    });

    assert_eq!(
        traps.1.get(),
        (2 * GROUPS + 2 * PER_GROUP) as usize,
        "{} operations cost two doorbells per group while {} cost two per operation",
        GROUPS * PER_GROUP,
        PER_GROUP
    );
    let Some(boundary) = boundary else {
        panic!("a blocking batched phase must produce an admitted boundary figure");
    };
    assert!(
        boundary.runtime_us <= u64::try_from(elapsed.as_micros()).unwrap(),
        "attributed run time cannot exceed the window that contains it"
    );
}
