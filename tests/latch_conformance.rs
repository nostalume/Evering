#![cfg(all(feature = "tokio", feature = "notify", any(unix, windows)))]

// The latch contract, in the four clauses every notification backend has to meet for
// `Signals` to park on it. A backend is conformant when all four hold for the pair
// `os::event()` hands out, whichever primitive it wraps, so this file is what a new
// adapter — including a kernel-side provider — is checked against.
//
// Which claims are portable and which are a convenience of the primitives here:
//
// - Stickiness, coalescing without counting, and cancellation that takes nothing are
//   the contract. seL4's notification object documents the same shape: a signalled word
//   stays until `receiveSignal` returns it to Idle, the same badge twice is
//   indistinguishable from once, and a thread that stops waiting consumes nothing.
// - That a park left the latch empty is not observable here: a park that stays asleep
//   cannot tell an empty latch from a readiness the driver never re-reported, so each
//   clause below keeps a live waiter as a positive control and the consumption claim
//   lives with the primitive it is about, in each backend's own test.
// - Releasing *every* registered waiter is not. `seL4_Signal` dequeues only the head
//   TCB while the others wait for the next signal, so a kernel peer cannot satisfy this
//   clause. Nothing in the library may depend on it: a woken waiter re-examines its own
//   state and re-parks, which is what `Contention` and `Patience` are for. It is tested
//   here because these backends really do release the whole set, and an accidental
//   change to a one-waiter-per-kick latch must not look like an improvement.
// - What keeps a head-only latch harmless is structural rather than disciplinary:
//   `os::event()` hands out one pair per condition — one ring, one direction — so no
//   two conditions are ever parked on one primitive. An adapter that multiplexed
//   causes onto a single latch would need the wake to name its cause, which is the
//   badge a bounded notification word carries and a byte queue or a bit does not.
// - The synchronous park over a *set* of latches, `Event::wait_any`, is none of these
//   clauses: it multiplexes latches rather than causes, so each condition still owns
//   one pair. Its own claims — one latch reported per park, the rest of the set kept
//   ready — are checked in `src/os/mux.rs`, beside the leg that makes them.

use std::time::Duration;

use evering::{
    notify::{Notify, Wait as _},
    os, runtime,
};

/// How long a clause waits for the wake it expects.
const EXPECT: Duration = Duration::from_secs(1);

/// How long a clause waits to show that no wake comes.
const ABSENT: Duration = Duration::from_millis(50);

#[tokio::test]
async fn a_kick_before_the_waiter_exists_is_not_lost() {
    let (ring, event) = os::event().unwrap();
    ring.notify().unwrap();

    let wait = runtime::Wait::new(event).unwrap();
    fn require_send<T: Send>(_: T) {}
    require_send(wait.wait());

    tokio::time::timeout(EXPECT, wait.wait())
        .await
        .expect("a kick that arrived before any waiter must still be pending")
        .unwrap();
}

#[tokio::test]
async fn repeated_kicks_coalesce_and_one_park_consumes_them_all() {
    let (ring, event) = os::event().unwrap();
    let wait = runtime::Wait::new(event).unwrap();
    ring.notify().unwrap();
    ring.notify().unwrap();
    ring.notify().unwrap();

    tokio::time::timeout(EXPECT, wait.wait())
        .await
        .expect("the first park must wake")
        .unwrap();
    assert!(
        tokio::time::timeout(ABSENT, wait.wait()).await.is_err(),
        "a latch that was consumed must not release a later park"
    );
    // A park that stays asleep proves nothing by itself: from here an empty latch and
    // a readiness nobody re-reported look identical. The same waiter must still wake
    // for a fresh kick, so the assertion above measured a consumed latch.
    ring.notify().unwrap();
    tokio::time::timeout(EXPECT, wait.wait())
        .await
        .expect("the same waiter must still wake for a fresh kick")
        .unwrap();
}

#[tokio::test]
async fn a_park_given_up_on_leaves_the_kick_for_the_next_park() {
    let (ring, event) = os::event().unwrap();
    let wait = runtime::Wait::new(event).unwrap();

    {
        let _never_polled = wait.wait();
    }
    let abandoned = tokio::time::timeout(ABSENT, wait.wait()).await;
    assert!(abandoned.is_err(), "nothing is pending yet");

    ring.notify().unwrap();
    tokio::time::timeout(EXPECT, wait.wait())
        .await
        .expect("an abandoned park may not consume the kick that follows it")
        .unwrap();
}

#[tokio::test]
async fn one_kick_releases_every_waiter_registered_before_it() {
    let (ring, event) = os::event().unwrap();
    let wait = runtime::Wait::new(event).unwrap();
    let first = async { wait.wait().await.unwrap() };
    let second = async { wait.wait().await.unwrap() };
    let notify = async {
        tokio::task::yield_now().await;
        ring.notify().unwrap();
    };

    tokio::time::timeout(EXPECT, async { tokio::join!(first, second, notify) })
        .await
        .expect("one sticky kick must release the waiters already parked on it");
}
