//! What a refusal says when a caller is left holding only its `Display`, and what it costs the
//! caller to hold it. A refusal's report is its own: it names the condition and whose progress
//! changes the answer, and neither the sentence nor the right to be treated as an error depends on
//! whether the value it hands back happens to be printable.
//!
//! The sentences about records are driven on the mock region, through the same `mock_session`
//! harness `talc` uses, because a refusal only counts when a real create made it; the one about a
//! read-only mapping is not, and says so where it stops. A `PoolCreateError` and a
//! `ChannelCreateError` are `Copy`, so those assertions compare the whole refusal rather than match
//! a pattern, which is what keeps the cause and the outer sentence in the same view.

use std::error::Error as _;

use crate::{
    BlockRangeError, ChannelCreateError, DeclineCause, PoolCreateError, ReceiveError, RoomWait,
    SessionError,
    layout::RegionId,
    notify::ProgressError,
    tests::mock::{
        MAX_ADDR,
        talc::{mock_session, peer_session},
    },
};

/// A payload with no `Debug`, which is what makes the dependency visible: the caller's value and the
/// library's diagnosis are two different parts, and only one of them is being reported.
struct Unprintable;

fn says(error: &dyn core::fmt::Display, wanted: &str) {
    let message = error.to_string();
    assert!(
        message.contains(wanted),
        "{message:?} never said {wanted:?}"
    );
}

#[test]
fn a_refused_class_range_names_the_bounds_it_refused() {
    says(&BlockRangeError, "classes");
}

#[test]
fn a_refused_reserve_says_whose_progress_frees_a_block() {
    says(&DeclineCause::TooWide, "wider");
    says(
        &DeclineCause::NoRoom {
            waits: RoomWait::Release,
        },
        "release",
    );
    says(&DeclineCause::Corrupted, "adjudication");
}

#[test]
fn an_empty_receive_says_whose_turn_it_is() {
    says(&ReceiveError::Empty, "sender");
    says(&ReceiveError::Busy, "retry");
    says(&ReceiveError::Closed, "claim");
}

/// The room that is missing and the word that is taken are different waits, and a refusal says which:
/// a sender waiting on a receiver has no reason to spin on a local compare-exchange, and a contention
/// the word still names can report who holds it.
#[test]
fn a_refused_send_says_whose_progress_changes_the_answer() {
    let full = DeclineCause::NoRoom {
        waits: RoomWait::Drain,
    };
    says(&full, "receiver");
    says(&DeclineCause::Busy { member: None }, "another member");
    says(&DeclineCause::Busy { member: Some(3) }, "member 3");
    says(&DeclineCause::Closed, "closed");
}

/// One call answers both ways, which is why the value sits beside the cause rather than in an arm of
/// it: a queue that is full never took the send, while a block whose authority word moved cannot come
/// back.
#[test]
fn a_refused_send_says_whether_the_value_came_back() {
    let full = DeclineCause::NoRoom {
        waits: RoomWait::Drain,
    };
    let returned = full.decline(Unprintable);
    let gone = DeclineCause::Busy { member: None }.declined::<Unprintable>();
    assert!(
        returned.value.is_some(),
        "a send that found no room comes back to the caller"
    );
    assert!(
        gone.value.is_none(),
        "a block another member claimed has nothing left to hand back"
    );
    says(&returned, "receiver");
    says(&gone, "another member");
}

/// Asking again is a strategy exactly where somebody else's act is what is missing. Every other answer
/// is information about the region, and the sharpest of them — a read-only mapping — names an act no
/// member of this region can perform.
#[test]
fn only_waiting_on_somebody_else_is_worth_asking_again() {
    for cause in [
        DeclineCause::Busy { member: None },
        DeclineCause::Busy { member: Some(3) },
        DeclineCause::NoRoom {
            waits: RoomWait::Drain,
        },
        DeclineCause::NoRoom {
            waits: RoomWait::Release,
        },
        DeclineCause::NoRoom {
            waits: RoomWait::Grow,
        },
        DeclineCause::NoRoom {
            waits: RoomWait::Promise,
        },
    ] {
        assert!(cause.is_retryable(), "{cause:?} waits on somebody else");
    }
    for cause in [
        DeclineCause::Closed,
        DeclineCause::TooWide,
        DeclineCause::Overflow,
        DeclineCause::Corrupted,
        DeclineCause::Poisoned,
        DeclineCause::ReadOnly {
            region: RegionId::new(0, 7),
        },
    ] {
        assert!(
            !cause.is_retryable(),
            "{cause:?} is not answered by asking again"
        );
    }
}

/// The refusal keeps its words and its standing as an error whatever rides back on it, because a
/// caller's `fn main() -> Result<(), Box<dyn Error>>` has to be able to hold the refusal it was
/// handed even when the payload is a value the caller never meant to print.
#[test]
fn a_refusal_is_reportable_whatever_it_carries_back() {
    let full = DeclineCause::NoRoom {
        waits: RoomWait::Drain,
    };
    let shortage = DeclineCause::NoRoom {
        waits: RoomWait::Release,
    };
    let refused: Vec<Box<dyn std::error::Error>> = vec![
        Box::new(shortage.decline(Unprintable)),
        Box::new(DeclineCause::TooWide.decline(Unprintable)),
        Box::new(full.decline(Unprintable)),
        Box::new(DeclineCause::Busy { member: None }.declined::<Unprintable>()),
        Box::new(ReceiveError::Closed),
        Box::new(BlockRangeError),
    ];
    says(&refused[0], "release");
    says(&refused[1], "wider");
    says(&refused[3], "another member");
    assert_eq!(refused.len(), 6);
    assert!(
        refused[2].source().is_some(),
        "a decline chains its cause, so the report survives the box"
    );
}

/// The two cause slots are the case where the carried value *is* the reason, so the dependency belongs
/// in the bound rather than in a format string: the outer sentence states this layer's judgement alone,
/// and the provider's own words arrive typed, from `source()`, where a caller can keep or print them.
#[test]
fn a_wrapped_cause_arrives_typed_rather_than_quoted() {
    let wait = ProgressError::Wait(std::io::Error::other("the latch was poisoned"));
    let source = SessionError::Source(std::io::Error::other("the name is already taken"));
    says(&wait, "waiting on the progress source failed");
    says(&source, "the provider refused the mapping");
    assert_eq!(
        wait.source().expect("the cause is the report").to_string(),
        "the latch was poisoned"
    );
    assert_eq!(
        source
            .source()
            .expect("the cause is the report")
            .to_string(),
        "the name is already taken"
    );
    assert!(ProgressError::<std::io::Error>::Busy.source().is_none());
    assert!(SessionError::<std::io::Error>::Closed.source().is_none());
    let boxed: Box<dyn std::error::Error> = Box::new(wait);
    says(&boxed, "progress source");
}

/// A record is something the region has to write, so the refusal behind a failed create is one of
/// this table's causes — and the outer error used to answer every one of them with the same flat
/// "storage failed". Here the same poisoned mutation word declines a pool's record and a channel's
/// record, and each hands the word back typed at `source()`, which is the only fact telling the
/// caller that recovery, not patience, is what would change the answer.
#[test]
fn a_refused_record_hands_back_the_refusal_it_was_built_from() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session(&mut pt, 0, MAX_ADDR);
    let second = peer_session(&mut pt);
    let dead = second.peer();
    session.abandon_heap_for_test(dead.slot());
    let recovery = unsafe { session.assume_dead(dead) }.unwrap();
    assert!(
        recovery.reap().is_ok(),
        "reaping settles the departed member's claim"
    );

    let pool = session
        .create_pool(32 * 1024, None)
        .err()
        .expect("a poisoned mutation word declines the pool's own record");
    assert_eq!(pool, PoolCreateError::Declined(DeclineCause::Poisoned));
    says(&pool, "the pool's own record");
    assert_eq!(
        pool.source()
            .expect("the refusal chains the cause it was built from")
            .to_string(),
        DeclineCause::Poisoned.to_string()
    );

    let channel = session
        .create_channel::<()>(1)
        .err()
        .expect("the same word declines the channel's record");
    assert_eq!(
        channel,
        ChannelCreateError::Declined(DeclineCause::Poisoned)
    );
    says(&channel, "the channel's record");
    assert_eq!(
        channel
            .source()
            .expect("the refusal chains the cause it was built from")
            .to_string(),
        DeclineCause::Poisoned.to_string()
    );
}

/// The arm this package exists to stop dropping. A mapping that cannot be written already knew why
/// the record was not admitted, and the sentence survives the outer type with the region in it, so
/// the caller can tell "the extent is full" from "nothing in this mapping can ever make room".
///
/// It is asserted at the projection rather than driven through a create: a session is itself an
/// admission that writes, so no session exists over a read-only mapping to decline. Making one would
/// mean breaking the entrance rule that refuses the wider request first.
#[test]
fn a_refused_write_names_the_region_and_the_admission_it_blocks() {
    let region = RegionId::new(0, 7);
    let cause = crate::dir::Error::Admission(crate::mem::ProjectionError::ReadOnly)
        .declined(region)
        .expect("a read-only mapping is declined, not retried");
    assert_eq!(cause, DeclineCause::ReadOnly { region });
    assert!(
        !cause.is_retryable(),
        "no act of a member of this mapping turns a read-only address space into a writable one"
    );
    let refused: Vec<Box<dyn std::error::Error>> = vec![
        Box::new(PoolCreateError::Declined(cause)),
        Box::new(ChannelCreateError::Declined(cause)),
    ];
    says(&refused[0], "the pool's own record");
    says(&refused[1], "the channel's record");
    for error in refused {
        let leaf = error
            .source()
            .expect("the outer refusal keeps the region's own answer");
        let message = leaf.to_string();
        assert!(message.contains("read-only"), "{message:?}");
        assert!(message.contains("write"), "{message:?}");
        assert!(
            message.contains(&format!("{region:?}")),
            "{message:?} never named the region the write belongs to"
        );
    }
}
