//! What a refusal says when a caller is left holding only its `Display`, and what it costs the
//! caller to hold it. A refusal's report is its own: it names the condition and whose progress
//! changes the answer, and neither the sentence nor the right to be treated as an error depends on
//! whether the value it hands back happens to be printable.

use crate::{BlockRangeError, PoolReserveError, ReceiveError, TrySendError};

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
fn a_refused_class_range_names_the_bounds_it_rejected() {
    says(&BlockRangeError, "classes");
}

#[test]
fn a_refused_reserve_says_whose_progress_frees_a_block() {
    says(&PoolReserveError::BlockTooLarge(()), "largest class");
    says(&PoolReserveError::Unavailable(()), "claimed");
    says(&PoolReserveError::Corrupted(()), "authority");
}

#[test]
fn an_empty_receive_says_whose_turn_it_is() {
    says(&ReceiveError::Empty, "sender");
    says(&ReceiveError::Busy, "retry");
    says(&ReceiveError::Closed, "claim");
}

#[test]
fn a_refused_send_says_whether_the_value_came_back() {
    says(&TrySendError::Full(()), "receiver");
    says(&TrySendError::Busy(()), "retry");
    says(&TrySendError::Disconnected(()), "came back");
    says(&TrySendError::<()>::Authority, "nothing was sent");
}

/// The refusal keeps its words and its standing as an error whatever rides back on it, because a
/// caller's `fn main() -> Result<(), Box<dyn Error>>` has to be able to hold the refusal it was
/// handed even when the payload is a value the caller never meant to print.
#[test]
fn a_refusal_is_reportable_whatever_it_carries_back() {
    let refused: Vec<Box<dyn std::error::Error>> = vec![
        Box::new(PoolReserveError::Unavailable(Unprintable)),
        Box::new(PoolReserveError::BlockTooLarge(Unprintable)),
        Box::new(TrySendError::Full(Unprintable)),
        Box::new(TrySendError::<Unprintable>::Authority),
        Box::new(ReceiveError::Closed),
        Box::new(BlockRangeError),
    ];
    says(&refused[0], "claimed");
    says(&refused[3], "nothing was sent");
    assert_eq!(refused.len(), 6);
}
