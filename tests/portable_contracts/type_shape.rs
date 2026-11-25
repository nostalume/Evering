//! Two claims the type system answers with nothing running: what a shared box's type records — a mapping
//! lifetime and a value, and nothing a holder could misuse to keep it alive past the session — and that a
//! session selects its protocol per channel, so two schemas share one region without a second session and
//! without a key collision. The third section adds the shape of a `?Sized` payload: the metadata a slice
//! carries and the refusal that comes back when it describes the wrong kind of value are names a caller
//! has to be able to spell, not types it can only receive.

use evering::layout::{Metadata, MetadataError, Shape};
use evering::{Channel, PBox, Port, Session, layout::Repr, layout::SharedSchema};

fn accepts_heap_box(_: PBox<'_, u64>) {}

#[test]
fn shared_box_has_only_mapping_lifetime_and_value_in_its_type() {
    let _ = accepts_heap_box;
}

#[repr(C)]
struct Left(u32);

#[repr(C)]
struct Right(u64);

impl SharedSchema for Left {
    const SCHEMA: evering::layout::SchemaKey =
        evering::layout::SchemaKey::new(evering::layout::SchemaId(0x6538_6c65_6674), 1);
}
unsafe impl Repr for Left {}

impl SharedSchema for Right {
    const SCHEMA: evering::layout::SchemaKey =
        evering::layout::SchemaKey::new(evering::layout::SchemaId(0x0065_3872_6967_6874), 1);
}
unsafe impl Repr for Right {}

fn creates_two_protocols(session: &Session) {
    let _: Result<(Channel<Left>, Port<Left>), _> = session.create_channel::<Left>(8);
    let _: Result<(Channel<Right>, Port<Right>), _> = session.create_channel::<Right>(8);
}

#[test]
fn session_protocol_is_selected_per_channel() {
    let _ = creates_two_protocols;
}

/// The bound a `?Sized` payload asks for, with both types it answers in written out rather than
/// inferred: this is the shape of the code a caller of `Channel::adopt` writes when it keeps the
/// extent of a slice rather than the slice.
fn reads_the_extent_of_a_sized_payload<T: Repr + Shape + ?Sized>(
    at: *const T,
) -> Result<core::alloc::Layout, MetadataError> {
    let metadata: Metadata = Shape::metadata(at);
    T::layout(metadata)
}

#[test]
fn metadata_answers_for_slices_and_refuses_the_other_kind() {
    let bytes = <[u8] as Shape>::layout(Metadata::Slice(3)).unwrap();
    assert_eq!((bytes.size(), bytes.align()), (3, 1));
    assert_eq!(
        <u64 as Shape>::layout(Metadata::Sized).unwrap().size(),
        8,
        "a sized value's metadata is the empty one, which is the whole answer [`reads_the_extent_of_a_sized_payload`] \
         can get without a pointer"
    );
    assert_eq!(
        <u64 as Shape>::layout(Metadata::Slice(1)).err(),
        Some(MetadataError::WrongKind),
        "a count where the type has none is the kind question, not a capacity one"
    );
    assert_eq!(
        <[u64] as Shape>::layout(Metadata::Slice(usize::MAX)).err(),
        Some(MetadataError::Overflow),
        "the refusal has to say the count is unnameable rather than pretend the payload is the wrong type"
    );
    let _ = reads_the_extent_of_a_sized_payload::<[u8]>;
}
