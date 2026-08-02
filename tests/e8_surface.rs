use evering::{Channel, Port, Session, layout::Repr};

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
