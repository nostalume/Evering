//! The member word as published data: how a process that joins a region without this crate cuts the
//! one `AtomicUsize` it is handed, which is the state a porter used to have to copy out of `src` to
//! match. The encoding and the numbers are the contract; what a state permits is the root's own rule,
//! read from the types in [`evering::dir`] and this crate's re-entry [`schema`].
//!
//! [`schema`]: evering::schema

use evering::layout::{
    MEMBER_DEAD, MEMBER_FREE, MEMBER_GENERATION_MAX, MEMBER_LIVE, MEMBER_RESERVED, MEMBER_RETIRED,
    MEMBER_STATE_BITS, MEMBER_STATE_MASK,
};

#[test]
fn the_member_word_is_cut_as_published() {
    // The states, in the order a porter reads them: a slot, a member, two ways a member stops being
    // one, and the reservation a parent holds while it waits for its child to say which member it is.
    assert_eq!(
        (
            MEMBER_FREE,
            MEMBER_LIVE,
            MEMBER_DEAD,
            MEMBER_RETIRED,
            MEMBER_RESERVED
        ),
        (0, 1, 2, 3, 4)
    );
    assert_eq!(MEMBER_STATE_BITS, 3);
    assert_eq!(MEMBER_STATE_MASK, 0b111);
    assert_eq!(MEMBER_GENERATION_MAX, usize::MAX >> MEMBER_STATE_BITS);
    // The two halves of a word separate: a state never reaches into the generation, and a full
    // generation never reaches into the state.
    let word = (MEMBER_GENERATION_MAX << MEMBER_STATE_BITS) | MEMBER_RESERVED;
    assert_eq!(word & MEMBER_STATE_MASK, MEMBER_RESERVED);
    assert_eq!(word >> MEMBER_STATE_BITS, MEMBER_GENERATION_MAX);
}
