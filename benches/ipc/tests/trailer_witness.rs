//! The channel a worker hands its own accounting back on: one fixed-shape trailer
//! line, the bracket a witness is only comparable inside, and absence read as absence
//! rather than as a zero figure.

use crate::tests::evidence_attribution::{system_header, system_measure};
use crate::{boundary, study, system};

#[test]
fn trailer_text_round_trips_a_witness_and_rejects_every_other_shape() {
    let witness = boundary::Witness {
        cost: boundary::Accounting {
            source: boundary::Source::Ticks,
            runtime_us: 300,
            voluntary_switches: 7,
            involuntary_switches: 0,
        },
        bracket_ns: 9_000_000_000,
    };
    let encoded = boundary::text::encode(&witness);
    assert_eq!(boundary::text::decode(&encoded).unwrap(), witness);
    for malformed in [
        "",
        "runtime_us=1 voluntary=1 involuntary=1 bracket_ns=1",
        "source=nanoseconds runtime_us=1 voluntary=1 involuntary=1",
        "source=uptime runtime_us=1 voluntary=1 involuntary=1 bracket_ns=1",
        "runtime_us=1 source=nanoseconds voluntary=1 involuntary=1 bracket_ns=1",
        "source=nanoseconds runtime_us=x voluntary=1 involuntary=1 bracket_ns=1",
        "source=nanoseconds runtime_us=-1 voluntary=1 involuntary=1 bracket_ns=1",
        "source=nanoseconds runtime_us=1 voluntary=1 involuntary=1 bracket_ns=1 trailing=2",
    ] {
        assert!(
            boundary::text::decode(malformed).is_none(),
            "{malformed:?} must not read as a witness"
        );
    }
}

#[test]
fn worker_witness_is_admitted_only_inside_its_bracket_and_the_trial_span() {
    let header = system_header("focused");
    let execution = header.run.schedule[0];
    let case = header.cases[execution.case as usize].clone();
    let witness = |bracket_ns, runtime_us| boundary::Witness {
        cost: boundary::Accounting {
            source: boundary::Source::Nanoseconds,
            runtime_us,
            voluntary_switches: 0,
            involuntary_switches: 0,
        },
        bracket_ns,
    };
    let admit = |worker| {
        let mut measure = system_measure(&case, 2_000_000);
        measure.worker = worker;
        <system::System as study::Schema>::validate(
            &header,
            &study::Observation {
                unit: execution.unit,
                case: execution.case,
                measure,
            },
        )
    };
    // The worker bracket opens at the probe and closes when the peer shuts the
    // channel down, so it legitimately runs past the timed window: only the
    // whole trial span bounds it.
    admit(Some(witness(2_000_001, 1))).unwrap();
    assert!(admit(Some(witness(2_000_003, 1))).is_err());
    // Run time cannot exceed the bracket it was measured over, and a
    // zero-length bracket is not a measurement of anything.
    assert!(admit(Some(witness(1_000, 2))).is_err());
    assert!(admit(Some(witness(0, 0))).is_err());
    // An unobserved worker is a state, not a zero.
    admit(None).unwrap();
}

#[test]
fn measure_omits_an_absent_worker_and_restores_a_present_one() {
    let header = system_header("focused");
    let execution = header.run.schedule[0];
    let case = header.cases[execution.case as usize].clone();
    let mut measure = system_measure(&case, 2_000_000);
    let encoded = serde_json::to_string(&measure).unwrap();
    assert!(!encoded.contains("worker"));
    measure.worker = Some(boundary::Witness {
        cost: boundary::Accounting {
            source: boundary::Source::Nanoseconds,
            runtime_us: 1,
            voluntary_switches: 0,
            involuntary_switches: 0,
        },
        bracket_ns: 2,
    });
    let encoded = serde_json::to_string(&measure).unwrap();
    assert!(encoded.contains("worker"));
    assert_eq!(
        serde_json::from_str::<system::Measure>(&encoded).unwrap(),
        measure
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_claimed_trailer_reports_absence_until_the_worker_fills_it() {
    let trailer = boundary::Trailer::claim()
        .expect("a writable temporary directory")
        .expect("procfs is readable on this host");
    // Claiming creates the file empty, so an unread report is absence before
    // any worker has written one, and never a zero.
    assert!(trailer.read().is_none());
    let other = boundary::Trailer::claim().unwrap().unwrap();
    assert_ne!(trailer.argument(), other.argument());
    let path = trailer.argument().to_owned();
    let phase = boundary::Phase::begin(trailer.argument()).expect("a claimed trailer brackets");
    let observed = std::time::Instant::now();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let caller_ns = observed.elapsed().as_nanos() as u64;
    phase.record(&path).expect("the trailer is writable");
    let witness = trailer.read().expect("the worker reported its bracket");
    // A width is asserted against the window it claims to span, never against a duration
    // the host was expected to spend: the bracket opens before the caller starts its own
    // clock and closes after that clock is read, so it cannot be narrower than the phase
    // taken over. The absolute width is still a recorded figure, which is what makes the
    // witness comparable to another window of its own kind.
    assert!(
        witness.bracket_ns >= caller_ns,
        "the reported {} ns bracket did not span the {} ns phase it was taken over",
        witness.bracket_ns,
        caller_ns
    );
    // `Phase` brackets with [`boundary::sample_here`], so this is the count of the task
    // that blocked (`/proc/thread-self/status`) and not the leader's.
    assert!(
        witness.cost.voluntary_switches > 0,
        "the task that blocked must report a voluntary switch of its own"
    );
    assert!(witness.cost.runtime_us <= witness.bracket_ns / 1_000);
    drop(trailer);
    assert!(
        !std::path::Path::new(&path).exists(),
        "a trailer must not outlive the trial that claimed it"
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn an_unobservable_host_claims_no_trailer_at_all() {
    assert!(matches!(boundary::Trailer::claim(), Ok(None)));
}
