//! The driver's own window: sliding issuance with validation shared with the worker,
//! counts that credit only committed work, and the two claims the instrument has to
//! survive — a path counter inside one percent and a warmup that cannot reset the
//! measured deadline.

use crate::{drive, model};

/// How long the trace endpoint parks the reply of the setup phase's invalid-response
/// probe (the `u64::MAX` operation). A leg that measures a deadline against the fixture
/// names this constant rather than a duration the host is expected to spend.
const READY_DELAY: std::time::Duration = std::time::Duration::from_millis(20);

#[derive(Default)]
struct TraceEndpoint {
    staged: Option<u64>,
    staged_digest: u64,
    payloads: Vec<Vec<u8>>,
    replies: std::collections::VecDeque<(u64, u64, usize)>,
    trace: Vec<(char, u64)>,
    corrupt: bool,
    fail_before_send: bool,
    fail_after_send: bool,
    fail_after_recv: bool,
    delay_ready: bool,
    fail_abort: bool,
    aborted: bool,
    quiet: bool,
}

impl drive::Endpoint for TraceEndpoint {
    type Error = &'static str;

    fn stage(&mut self, operation: u64, payload: &[u8]) -> Result<(), Self::Error> {
        if self.staged.replace(operation).is_some() {
            return Err("double stage");
        }
        self.staged_digest = model::digest(payload);
        self.payloads.push(payload.to_vec());
        Ok(())
    }

    fn try_send(
        &mut self,
        _: &mut drive::PathCounts,
    ) -> Result<drive::Step<(), Self::Error>, Self::Error> {
        if self.fail_before_send {
            return Err("pre-commit send failure");
        }
        let operation = self.staged.take().ok_or("send without stage")?;
        if !self.quiet {
            self.trace.push(('s', operation));
        }
        self.replies.push_back((
            operation,
            self.staged_digest,
            self.payloads.last().map_or(0, Vec::len),
        ));
        let _advisory_signal_failed = self.fail_after_send;
        Ok(drive::Step::Committed(Ok(())))
    }

    fn try_recv(
        &mut self,
        _: &mut drive::PathCounts,
        expected: drive::Expected,
    ) -> Result<drive::Step<bool, Self::Error>, Self::Error> {
        let Some((operation, digest, payload_len)) = self.replies.pop_front() else {
            return Ok(drive::Step::Pending);
        };
        if self.delay_ready && operation == u64::MAX {
            std::thread::sleep(READY_DELAY);
        }
        if !self.quiet {
            self.trace.push(('r', operation));
        }
        if self.fail_after_recv {
            return Ok(drive::Step::Committed(Err(
                "post-commit reconstruction failure",
            )));
        }
        let digest = digest ^ u64::from(self.corrupt);
        Ok(drive::Step::Committed(Ok(expected.matches_digest(
            operation,
            digest,
            payload_len,
        ))))
    }

    fn wait(
        &mut self,
        _: drive::Interest,
        _: drive::Deadline,
        _: &mut drive::PathCounts,
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
    trace_transfer_with_paths(endpoint, count, window).map(|(counts, _)| counts)
}

fn trace_transfer_with_paths(
    endpoint: &mut TraceEndpoint,
    count: u64,
    window: u64,
) -> Result<(drive::Counts, drive::PathCounts), drive::Error<&'static str>> {
    let mut paths = drive::PathCounts::default();
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
        &mut paths,
    )
    .map(|counts| (counts, paths))
}

#[test]
fn a_sliding_window_and_shared_validation_account_for_every_transfer() {
    let mut endpoint = TraceEndpoint::default();
    let (counts, paths) = trace_transfer_with_paths(&mut endpoint, 4, 2).unwrap();
    assert_eq!(
        (counts.accepted, counts.completed, counts.validated),
        (4, 4, 4)
    );
    assert_eq!((paths.send_attempts, paths.recv_attempts), (4, 4));
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
    assert!(endpoint.payloads.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn an_invalid_response_aborts_and_retains_the_committed_counts() {
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
fn an_advisory_failure_after_commit_is_ignored() {
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
fn only_committed_work_is_counted_before_an_error() {
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

#[test]
#[cfg_attr(debug_assertions, ignore = "optimized evidence gate")]
fn path_counter_overhead_stays_inside_one_percent() {
    use std::time::{Duration, Instant};

    fn timed(instrumented: bool) -> f64 {
        let mut endpoint = TraceEndpoint {
            replies: std::collections::VecDeque::with_capacity(8),
            quiet: true,
            ..TraceEndpoint::default()
        };
        let work = drive::Work {
            start: 0,
            count: 100_000,
            window: 8,
            payload: 0,
            seed: 7,
        };
        let began = Instant::now();
        let deadline = drive::Deadline::after(began, Duration::from_secs(10)).unwrap();
        let counts = if instrumented {
            drive::transfer(
                &mut endpoint,
                work,
                deadline,
                &mut drive::PathCounts::default(),
            )
        } else {
            drive::transfer_control(&mut endpoint, work, deadline)
        }
        .unwrap();
        assert_eq!(counts.validated, work.count);
        began.elapsed().as_secs_f64()
    }

    for _ in 0..3 {
        std::hint::black_box((timed(false), timed(true)));
    }
    let ratios = (0..101)
        .map(|pair| {
            let (control, instrumented) = if pair % 2 == 0 {
                let first = timed(false);
                let inner = timed(true) * timed(true);
                (first * timed(false), inner)
            } else {
                let first = timed(true);
                let inner = timed(false) * timed(false);
                (inner, first * timed(true))
            };
            (instrumented / control).sqrt()
        })
        .collect::<Vec<_>>();
    let mut state = 7_u64;
    let mut interval = (0..10_000)
        .map(|_| {
            let mut sample = (0..ratios.len())
                .map(|_| {
                    state = state
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1);
                    ratios[state as usize % ratios.len()]
                })
                .collect::<Vec<_>>();
            sample.sort_unstable_by(f64::total_cmp);
            sample[sample.len() / 2]
        })
        .collect::<Vec<_>>();
    interval.sort_unstable_by(f64::total_cmp);
    let low = interval[250];
    let high = interval[9_749];
    assert!(
        low >= 0.99 && high <= 1.01,
        "instrumentation interval [{:.4}, {:.4}]",
        low,
        high
    );
}

#[test]
fn warmup_consumption_cannot_reset_the_measured_deadline() {
    let endpoint = || TraceEndpoint {
        delay_ready: true,
        ..TraceEndpoint::default()
    };
    let work = || drive::Work {
        start: 0,
        count: 1,
        window: 1,
        payload: 1,
        seed: 7,
    };
    // The budget is half the delay the fixture inserts, and the pass above is what makes
    // that meaningful: a cold process reaches the driver's first deadline check through
    // its own start-up, and a run that expires there reports a warmup that never happened
    // rather than one that consumed the window. Driving the same phase untimed first pays
    // that cost once, so the timed pass measures the warmup and nothing else.
    let settled = {
        let began = std::time::Instant::now();
        drive::measure(
            &mut endpoint(),
            work(),
            1,
            began,
            drive::Deadline::after(began, std::time::Duration::from_secs(1)).unwrap(),
        )
        .expect("an untimed setup completes")
    };
    assert!(
        settled.setup_ns >= READY_DELAY.as_nanos() as u64,
        "the untimed setup is the phase that parks: {} ns observed, {:?} parked",
        settled.setup_ns,
        READY_DELAY
    );

    let began = std::time::Instant::now();
    let budget = READY_DELAY / 2;
    let setup_ns = settled.setup_ns;
    let error = drive::measure(
        &mut endpoint(),
        work(),
        1,
        began,
        drive::Deadline::after(began, budget).unwrap(),
    )
    .err()
    .expect("a budget inside the parked setup cannot survive to the measured transfer");
    assert!(
        error.timed,
        "a deadline spent warming up is the measurement timing out, not a setup that \
         failed: {budget:?} budget against {setup_ns} ns of untimed setup"
    );
    assert_eq!(error.error.kind, drive::Kind::Deadline);
}
