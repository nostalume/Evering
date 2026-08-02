#![cfg(feature = "tokio")]

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
