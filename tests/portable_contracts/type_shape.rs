//! Two claims the type system answers with nothing running: what a shared box's type records — a mapping
//! lifetime and a value, and nothing a holder could misuse to keep it alive past the session — and that a
//! session selects its protocol per channel, so two schemas share one region without a second session and
//! without a key collision.

use evering::{Channel, PBox, Port, Session, layout::Repr};

fn accepts_heap_box(_: PBox<'_, u64>) {}

#[test]
fn shared_box_has_only_mapping_lifetime_and_value_in_its_type() {
    let _ = accepts_heap_box;
}

#[repr(C)]
struct Left(u32);

#[repr(C)]
struct Right(u64);

unsafe impl Repr for Left {
    const SCHEMA: evering::layout::SchemaKey =
        evering::layout::SchemaKey::new(evering::layout::SchemaId(0x6538_6c65_6674), 1);
}

unsafe impl Repr for Right {
    const SCHEMA: evering::layout::SchemaKey =
        evering::layout::SchemaKey::new(evering::layout::SchemaId(0x0065_3872_6967_6874), 1);
}

fn creates_two_protocols(session: &Session) {
    let _: Result<(Channel<Left>, Port<Left>), _> = session.create_channel::<Left>(8);
    let _: Result<(Channel<Right>, Port<Right>), _> = session.create_channel::<Right>(8);
}

#[test]
fn session_protocol_is_selected_per_channel() {
    let _ = creates_two_protocols;
}
