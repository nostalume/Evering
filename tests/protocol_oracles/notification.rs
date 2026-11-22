//! The notification model: every commit/ring interleaving a clear may retry through, which refusals are
//! allowed to admit a peer-only wait, and how a deferred batch's rings are bounded.

use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Producer {
    Before,
    Committed,
    Ringed,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Consumer {
    Try,
    Wait,
    Clear,
    Done,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct State {
    producer: Producer,
    consumer: Consumer,
    shared: bool,
    latched: bool,
}

#[test]
fn clear_retry_covers_every_commit_ring_interleaving() {
    let mut pending = vec![State {
        producer: Producer::Before,
        consumer: Consumer::Try,
        shared: false,
        latched: false,
    }];
    let mut seen = HashSet::new();

    while let Some(state) = pending.pop() {
        if !seen.insert(state) {
            continue;
        }
        assert!(
            state.producer != Producer::Ringed || state.consumer != Consumer::Wait || state.latched,
            "a completed ring may not leave its waiter asleep"
        );
        if state.consumer == Consumer::Done {
            continue;
        }

        match state.producer {
            Producer::Before => pending.push(State {
                producer: Producer::Committed,
                shared: true,
                ..state
            }),
            Producer::Committed => pending.push(State {
                producer: Producer::Ringed,
                latched: true,
                ..state
            }),
            Producer::Ringed => {}
        }
        match state.consumer {
            Consumer::Try if state.shared => pending.push(State {
                consumer: Consumer::Done,
                ..state
            }),
            Consumer::Try => pending.push(State {
                consumer: Consumer::Wait,
                ..state
            }),
            Consumer::Wait => pending.push(State {
                consumer: Consumer::Clear,
                ..state
            }),
            Consumer::Clear => pending.push(State {
                consumer: Consumer::Try,
                latched: false,
                ..state
            }),
            Consumer::Done => {}
        }
    }

    assert!(seen.iter().any(|state| state.consumer == Consumer::Done));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RetryCause {
    PeerFull,
    LocalBusy,
}

const fn admits_peer_wait(cause: RetryCause) -> bool {
    matches!(cause, RetryCause::PeerFull)
}

#[test]
fn local_contention_never_admits_peer_only_wait() {
    assert!(admits_peer_wait(RetryCause::PeerFull));
    assert!(!admits_peer_wait(RetryCause::LocalBusy));

    // Counterexample retained from the former collapsed error: producer A owns
    // the slot, producer B observes local contention, and only the consumer is
    // notified when A publishes. Sleeping B on the consumer's notification can
    // therefore remain asleep despite local progress.
    let local_producer_will_publish = true;
    let producer_waiter_is_notified = false;
    assert!(local_producer_will_publish && !producer_waiter_is_notified);
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Batch {
    commits: usize,
    visible: usize,
    owed: bool,
    rung: usize,
    guard_open: bool,
    parked: bool,
    latched: bool,
}

const BATCH_LIMIT: usize = 4;

#[test]
fn a_deferred_batch_rings_once_and_can_never_end_owing() {
    let mut pending = vec![Batch {
        commits: 0,
        visible: 0,
        owed: false,
        rung: 0,
        guard_open: true,
        parked: false,
        latched: false,
    }];
    let mut seen = HashSet::new();

    while let Some(state) = pending.pop() {
        if !seen.insert(state) {
            continue;
        }
        assert!(
            !(state.owed && !state.guard_open),
            "a guard that left scope delivered what its batch owed"
        );
        assert!(
            state.rung <= state.commits,
            "a doorbell cannot be rung more times than the batch committed"
        );
        assert!(
            !(state.latched && state.rung == 0),
            "only a ring can set the peer's latch"
        );
        assert!(
            !(state.visible > 0 && state.parked && !state.latched && !state.guard_open),
            "committed bytes a parked peer has not seen cannot outlive their doorbell"
        );

        if state.guard_open {
            if state.commits < BATCH_LIMIT {
                pending.push(Batch {
                    commits: state.commits + 1,
                    visible: state.visible + 1,
                    owed: true,
                    ..state
                });
            }
            if state.owed {
                pending.push(Batch {
                    owed: false,
                    rung: state.rung + 1,
                    latched: true,
                    ..state
                });
            }
        }
        if state.parked && state.visible > 0 {
            pending.push(Batch {
                visible: state.visible - 1,
                parked: false,
                latched: false,
                ..state
            });
        } else if state.visible > 0 {
            pending.push(Batch {
                visible: state.visible - 1,
                latched: false,
                ..state
            });
        }
        if !state.parked && state.visible == 0 {
            pending.push(Batch {
                parked: true,
                ..state
            });
        }
        pending.push(Batch {
            guard_open: false,
            owed: false,
            rung: if state.owed {
                state.rung + 1
            } else {
                state.rung
            },
            latched: if state.owed { true } else { state.latched },
            ..state
        });
    }

    assert!(
        seen.iter().any(|state| state.commits == BATCH_LIMIT
            && state.visible == BATCH_LIMIT
            && state.rung == 1),
        "several commits must be able to sit behind one doorbell"
    );
    assert!(
        seen.iter()
            .any(|state| state.commits == 0 && state.rung == 0 && !state.guard_open),
        "an empty batch must cost no trap"
    );
    assert!(
        !seen
            .iter()
            .any(|state| state.commits == 0 && state.rung > 0),
        "a batch that committed nothing may not wake its peer"
    );
}
