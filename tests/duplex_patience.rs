#![cfg(all(feature = "map", feature = "tokio"))]

// One agent, both roles. `Channel::split` gives a holder a `Tx` on its own route and an
// `Rx` on the peer's, so a request/response side owns both directions in one task, and the
// waiting loops in `Signals` run one direction until it commits. That combination is what
// this file tests: an agent blocked in a send never looks at its receive, and a peer built
// the same way is blocked in its own send for want of exactly that draining.
//
// Both arms use the same region, capacity, window, and wait source, and the arms differ
// only in whether a receive is attempted between publishes. The wait source yields to the
// scheduler instead of sleeping, which is the most generous assumption available here: both
// agents run whenever the other is not, so a stall cannot be blamed on a notification that
// was lost. `Adaptive`, the library's own ladder, has no deadline, so each exchange is
// bounded from outside — which is what a caller has to invent to notice it is stuck.
//
// The completing arm is what makes the stalling arm mean anything: it shows this geometry
// moves data at all, so the other result is about loop shape rather than a broken fixture.

use std::{cell::Cell, rc::Rc, time::Duration};

use evering::{
    Decline, DeclineCause, Encoded, PoolRef, ReceiveError, Rx, Session, Transfer, Tx,
    layout::{RegionId, SchemaId, SchemaKey},
    notify::{Signals, Wait},
};

mod support;
use crate::support::region;

const REGION: RegionId = RegionId::new(0x6576_6475_706c_6578, 1);
const RUNTIME: SchemaKey = SchemaKey::new(SchemaId(0x6576_6475_706c_6531), 1);

/// Slots per direction, and the messages each agent wants to publish before it reads. Four
/// into two is the smallest window that lets one side outrun its peer.
const CAPACITY: usize = 2;
const WINDOW: usize = 4;

/// How long an exchange may take before it counts as stalled.
const LIMIT: Duration = Duration::from_millis(500);

#[derive(Default)]
struct Tally {
    published: Cell<usize>,
    admitted: Cell<usize>,
}

/// A wake that only hands the turn to the other agent.
struct Yield;

impl Wait for Yield {
    type Error = ();

    async fn wait(&self) -> Result<(), Self::Error> {
        tokio::task::yield_now().await;
        Ok(())
    }
}

fn sessions() -> (Session, Session) {
    region::pair("evering-duplex", 1 << 21, REGION)
}

/// Publishes the whole window, then reads the whole window: the shape a duplex agent
/// settles into when it trusts the loop to wait for the peer.
async fn burst(
    signals: &Signals<'_, (), Yield>,
    tx: &Tx<Encoded>,
    rx: &Rx<Encoded>,
    pool: PoolRef<'_>,
    tally: &Tally,
) {
    while tally.published.get() < WINDOW {
        let Ok(permit) = signals.reserve(tx).await else {
            return;
        };
        let next = tally.published.get() as u64;
        permit
            .send(pool.put(next).unwrap().encode(RUNTIME))
            .unwrap()
            .into_parts();
        tally.published.set(next as usize + 1);
    }
    while tally.admitted.get() < WINDOW {
        let Ok(received) = signals.claim(rx).await else {
            return;
        };
        let block = signals
            .admit::<u64>(received, pool, RUNTIME)
            .unwrap()
            .into_value();
        assert_eq!(*block, tally.admitted.get() as u64);
        tally.admitted.set(tally.admitted.get() + 1);
    }
}

/// The same window, publishing one message at a time and attempting a read after every
/// attempt, which is what the study's client loop does at a window of eight.
async fn alternate(
    signals: &Signals<'_, (), Yield>,
    tx: &Tx<Encoded>,
    rx: &Rx<Encoded>,
    pool: PoolRef<'_>,
    tally: &Tally,
) {
    let mut staged: Vec<Transfer<'_, Encoded>> = Vec::new();
    while tally.admitted.get() < WINDOW {
        if tally.published.get() < WINDOW && staged.is_empty() {
            let next = tally.published.get() as u64;
            staged.push(pool.put(next).unwrap().encode(RUNTIME));
        }
        if let Some(value) = staged.pop() {
            match signals.try_send(tx, value) {
                Ok(committed) => {
                    committed.into_parts();
                    tally.published.set(tally.published.get() + 1);
                }
                Err(Decline { cause, value }) => match (cause, value) {
                    (
                        DeclineCause::NoRoom { .. } | DeclineCause::Contested { .. },
                        Some(returned),
                    ) => staged.push(returned),
                    (DeclineCause::Sealed, _) => panic!("the peer closed early"),
                    (_, None) => panic!("the block lost its authority: {cause:?}"),
                    (cause, Some(_)) => panic!("the peer refused for {cause:?}"),
                },
            }
        }
        match rx.claim() {
            Ok(received) => {
                let block = signals
                    .admit::<u64>(received, pool, RUNTIME)
                    .unwrap()
                    .into_value();
                assert_eq!(*block, tally.admitted.get() as u64);
                tally.admitted.set(tally.admitted.get() + 1);
            }
            Err(ReceiveError::Empty | ReceiveError::Busy) => {}
            Err(ReceiveError::Closed) => panic!("the peer closed early"),
        }
        tokio::task::yield_now().await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn agents_that_publish_before_reading_stall_at_the_ring_limit() {
    let (left, right) = sessions();
    let (channel, port) = left.create_channel::<Encoded>(CAPACITY).unwrap();
    let peer_channel = right.adopt(port).unwrap();
    let (left_tx, left_rx) = channel.split();
    let (right_tx, right_rx) = peer_channel.split();
    let pool = left.create_pool(64 * 1024, None).unwrap();
    let peer_pool = right.open_pool(pool.id()).unwrap();
    let signals = Signals::new(&(), &Yield);
    let left_tally = Rc::new(Tally::default());
    let right_tally = Rc::new(Tally::default());

    let ran = tokio::time::timeout(LIMIT, async {
        tokio::join!(
            burst(&signals, &left_tx, &left_rx, pool.as_ref(), &left_tally),
            burst(
                &signals,
                &right_tx,
                &right_rx,
                peer_pool.as_ref(),
                &right_tally
            ),
        )
    })
    .await;

    assert!(
        ran.is_err(),
        "both agents finished: left {:?} right {:?}",
        (left_tally.published.get(), left_tally.admitted.get()),
        (right_tally.published.get(), right_tally.admitted.get())
    );
    for (name, tally) in [("left", &left_tally), ("right", &right_tally)] {
        assert!(
            (1..WINDOW).contains(&tally.published.get()),
            "{name} published {} of {WINDOW}, so it did not stop with its ring full",
            tally.published.get()
        );
        assert_eq!(
            tally.admitted.get(),
            0,
            "{name} read {} responses while its peer was still publishing",
            tally.admitted.get()
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn the_same_window_drains_when_a_read_is_attempted_between_publishes() {
    let (left, right) = sessions();
    let (channel, port) = left.create_channel::<Encoded>(CAPACITY).unwrap();
    let peer_channel = right.adopt(port).unwrap();
    let (left_tx, left_rx) = channel.split();
    let (right_tx, right_rx) = peer_channel.split();
    let pool = left.create_pool(64 * 1024, None).unwrap();
    let peer_pool = right.open_pool(pool.id()).unwrap();
    let signals = Signals::new(&(), &Yield);
    let left_tally = Rc::new(Tally::default());
    let right_tally = Rc::new(Tally::default());

    tokio::time::timeout(LIMIT, async {
        tokio::join!(
            alternate(&signals, &left_tx, &left_rx, pool.as_ref(), &left_tally),
            alternate(
                &signals,
                &right_tx,
                &right_rx,
                peer_pool.as_ref(),
                &right_tally
            ),
        )
    })
    .await
    .expect("one publish and one read per turn must clear both rings");
    for (name, tally) in [("left", &left_tally), ("right", &right_tally)] {
        assert_eq!(
            (tally.published.get(), tally.admitted.get()),
            (WINDOW, WINDOW),
            "{name} stalled part way through the window"
        );
    }
}
