//! Path counts and geometry: what a counter may claim about frequency, cause and
//! overflow, which metrics a replay must keep causally separate, and why a projected
//! condition never authorizes a buddy allocation.

use crate::tests::evidence_attribution::system_evidence;
use crate::{drive, geometry};

#[test]
fn path_counts_preserve_frequency_cause_and_overflow() {
    let mut paths = drive::PathCounts::default();
    paths.send_full().unwrap();
    paths.send_full().unwrap();
    paths.send_busy().unwrap();
    paths.recv_empty().unwrap();

    assert_eq!(
        (
            paths.send_full,
            paths.send_busy,
            paths.recv_empty,
            paths.send_attempts
        ),
        (2, 1, 1, 0)
    );
    paths.send_full = u64::MAX;
    assert_eq!(paths.send_full(), Err(drive::CountOverflow));
}

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
    let evidence = system_evidence("focused", 1.0);
    let report = geometry::evidence_report(core::slice::from_ref(&evidence)).unwrap();
    assert!(report.starts_with("geometry-v1\nsource\t"));
    assert!(report.lines().nth(2).unwrap().ends_with("\tfalse"));
    assert_eq!(
        report,
        geometry::evidence_report(core::slice::from_ref(&evidence)).unwrap()
    );
}
