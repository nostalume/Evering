//! The two frame tags a handoff is spelled with, read through the path a porter reads rather than
//! through `src`.
//!
//! The bytes are the whole claim, so nothing runs and no process is forked: which field follows a
//! tag, and how a frame is sized, belongs to the layout publication rather than to this leg.

#[test]
fn the_two_frame_tags_are_published_and_differ() {
    assert_eq!(evering::process::HANDOFF_MAGIC, *b"EVR1");
    assert_eq!(evering::process::ARRIVAL_MAGIC, *b"EVA1");
    assert_ne!(
        evering::process::HANDOFF_MAGIC,
        evering::process::ARRIVAL_MAGIC,
        "a reader that took a receipt for an offer would adopt nothing and report it as success"
    );
}
