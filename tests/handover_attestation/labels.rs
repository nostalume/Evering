//! The frame's published vocabulary: the two tags a handoff is spelled with, and the front those tags
//! sit in.
//!
//! The bytes are the whole claim, so nothing runs and no process is forked. What this leg can pin from
//! outside `src` is the numbers themselves — which offsets tile the width, and that the published
//! bounds still fit the field width they are stamped into. Whether a host's reader accepts a frame the
//! other host's writer built is not answerable here: a frame never crosses a host boundary, which is
//! exactly why both writers and both readers go through the same two functions in `process`.

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

#[test]
fn the_frame_front_is_published_and_tiles_its_width() {
    use evering::process::{FRAME_COUNT, FRAME_HEADER, FRAME_LEN, FRAME_TAG};
    assert_eq!(FRAME_TAG, 0..4);
    assert_eq!(FRAME_LEN, 4..6);
    assert_eq!(FRAME_COUNT, 6..8);
    assert_eq!(
        FRAME_TAG.len() + FRAME_LEN.len() + FRAME_COUNT.len(),
        FRAME_HEADER,
        "a porter that reads the three offsets has to land on the byte the payload starts at, and \
         nothing before it may be unaccounted for"
    );
    assert!(
        evering::process::MAX_BOOTSTRAP < 1 << (FRAME_LEN.len() * 8)
            && evering::process::MAX_RESOURCES < 1 << (FRAME_COUNT.len() * 8),
        "both counts are stamped into the bytes those offsets name, so a bound wider than the field \
         would be truncated on the wire rather than refused"
    );
}
