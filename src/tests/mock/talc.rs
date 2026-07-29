use crate::msg::Repr;
use crate::perlude::talc::{Session, SessionBy};
use crate::tests;
use crate::tests::mock::{MAX_ADDR, MockBackend};

type MockAlloc = crate::talc::MapTalc;
type MockSession<H> = Session<H>;
type TestProtocol = ();

#[repr(C)]
struct Large([u64; 2]);
#[repr(transparent)]
#[derive(Debug)]
struct Small(u64);

// Deliberately violates Repr's schema-identity law to prove that exact extent
// admission still prevents a colliding schema from forming the wrong pointer.
unsafe impl Repr for Large {
    const SCHEMA: crate::SchemaKey = crate::SchemaKey::new(crate::SchemaId(91), 1);
}
unsafe impl Repr for Small {
    const SCHEMA: crate::SchemaKey = crate::SchemaKey::new(crate::SchemaId(91), 1);
}

fn mock_alloc(bk: &mut [u8], start: usize, size: usize) -> MockAlloc {
    let bk = MockBackend(bk);
    bk.shared(start, size).try_into().unwrap()
}

fn mock_session<H: Repr>(bk: &mut [u8], start: usize, size: usize) -> MockSession<H> {
    SessionBy::from(MockBackend(bk).shared(start, size)).unwrap()
}

#[test]
fn heap_moves_and_reconstructs_without_allocator_plumbing() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session::<()>(&mut pt, 0, MAX_ADDR);
    let heap = session.heap();

    let record = heap.put(17_u64).expect("allocate value").pack(());
    let (_, value) = heap.open::<(), u64>(record).unwrap();
    assert_eq!(*value, 17);
}

#[test]
fn session_channel_count_is_bounded_by_shared_memory_not_a_type_constant() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session::<()>(&mut pt, 0, MAX_ADDR);

    let first = session.prepare(1).expect("first channel");
    let second = session
        .prepare(1)
        .expect("a directory-backed session is not limited to one channel");

    assert!(session.acquire(first).is_some());
    assert!(session.acquire(second).is_some());
}

#[test]
fn session_rejects_zero_capacity_without_panicking_or_publishing() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session::<()>(&mut pt, 0, MAX_ADDR);

    assert!(session.prepare(0).is_none());
    assert!(session.prepare(1).is_some());
}

#[test]
fn duplex_sides_share_each_physical_direction_and_close_gate() {
    use crate::channel::{QueueChannel, TryRecvError};

    let mut pt = [0; MAX_ADDR];
    let session = mock_session::<()>(&mut pt, 0, MAX_ADDR);
    let id = session.prepare(3).unwrap();
    let view = session.acquire(id).unwrap();
    let (left_send, left_recv) = view.clone().lsplit();
    let (right_send, right_recv) = view.rsplit();

    left_send
        .try_send(session.heap().put(7_u64).unwrap().pack(()))
        .unwrap();
    let record = right_recv.try_recv().unwrap();
    assert_eq!(*session.heap().open::<(), u64>(record).unwrap().1, 7);

    left_send.close();
    assert!(matches!(
        right_recv.try_recv(),
        Err(TryRecvError::Disconnected)
    ));
    right_send.close();
    assert!(matches!(
        left_recv.try_recv(),
        Err(TryRecvError::Disconnected)
    ));
}

#[test]
fn remove_drains_both_directions_and_reclaims_the_dynamic_layout() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session::<()>(&mut pt, 0, MAX_ADDR);
    let id = session.prepare(3).unwrap();
    let view = session.acquire(id).unwrap();
    let (left, _) = view.clone().lsplit();
    let (right, _) = view.clone().rsplit();
    left.try_send(session.heap().put(11_u64).unwrap().pack(()))
        .unwrap();
    right
        .try_send(session.heap().put(13_u64).unwrap().pack(()))
        .unwrap();
    drop((left, right));

    assert!(session.remove(id, view).is_ok());
    assert!(session.acquire(id).is_none());
    assert!(session.prepare(3).is_some());
}

#[test]
fn remove_returns_the_view_unchanged_while_a_local_endpoint_exists() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session::<()>(&mut pt, 0, MAX_ADDR);
    let id = session.prepare(2).unwrap();
    let view = session.acquire(id).unwrap();
    let endpoint = view.clone().lsplit().0;
    let (view, record) = session.remove(id, view).unwrap_err();
    assert!(record.is_none());
    drop(endpoint);
    assert!(session.remove(id, view).is_ok());
}

#[test]
fn session_accepts_a_const_explicit_geometry_covering_its_bound() {
    const GEOMETRY: crate::talc::Geometry = match crate::talc::Geometry::new(MAX_ADDR, 5, 10, 2) {
        Ok(geometry) => geometry,
        Err(_) => panic!("valid test geometry"),
    };

    let mut pt = [0; MAX_ADDR];
    let session =
        SessionBy::<()>::from_geometry(MockBackend(&mut pt).shared(0, MAX_ADDR), GEOMETRY).unwrap();
    let heap = session.heap();
    let record = heap.put(29_u32).unwrap().pack(());
    assert_eq!(*heap.open::<(), u32>(record).unwrap().1, 29);
}

#[test]
fn unknown_route_preserves_record_for_discard() {
    use crate::token::OpenKind;

    let mut pt = [0; MAX_ADDR];
    let session = mock_session::<()>(&mut pt, 0, MAX_ADDR);
    let heap = session.heap();
    let record = heap.put(23_u32).unwrap().pack(());
    let error = heap.open::<(), u64>(record).unwrap_err();
    assert_eq!(error.kind, OpenKind::Unknown);
    assert_eq!(heap.discard(error.record).unwrap(), ());
}

#[test]
fn encoded_bytes_round_trip_with_runtime_schema() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session::<crate::Encoded>(&mut pt, 0, MAX_ADDR);
    let heap = session.heap();
    let schema = crate::SchemaKey::new(crate::SchemaId(0x454e_434f_4445_4401), 3);

    let record = heap.encode(schema, b"evering").unwrap();
    let (header, bytes) = heap.open_encoded(record).unwrap();

    assert_eq!(header.schema(), schema);
    assert_eq!(&*bytes, b"evering");
    assert!("not-a-number".parse::<u64>().is_err());
}

#[test]
fn colliding_schema_cannot_reconstruct_a_different_extent() {
    use crate::mem::TransferError;
    use crate::token::{OpenKind, ReconstructError};

    let mut pt = [0; MAX_ADDR];
    let session = mock_session::<()>(&mut pt, 0, MAX_ADDR);
    let heap = session.heap();
    let record = heap.put(Large([1, 2])).unwrap().pack(());
    let error = heap.open::<(), Small>(record).unwrap_err();
    assert_eq!(
        error.kind,
        OpenKind::Reconstruct(ReconstructError::Transfer(TransferError::WrongExtent))
    );
    heap.discard(error.record).unwrap();
}

#[test]
fn non_lifo_reuse() {
    let mut pt = [0; MAX_ADDR];
    tests::alloc_lines::<8, 1000, 5>(mock_alloc(&mut pt, 0, MAX_ADDR));
}

#[test]
fn boxed_drop_reclaims() {
    let mut pt = [0; MAX_ADDR];
    tests::pbox_droppy::<5000, 1>(mock_alloc(&mut pt, 0, MAX_ADDR));
}

#[test]
fn random_boxes() {
    let mut pt = [0; MAX_ADDR];
    tests::pbox_rand::<500, 1>(mock_alloc(&mut pt, 0, MAX_ADDR));
}

#[test]
fn token_round_trip() {
    let mut pt = [0; MAX_ADDR];
    tests::pbox_token::<2500, 1>(mock_alloc(&mut pt, 0, MAX_ADDR));
}

#[test]
fn exhaustion_recovers_after_release() {
    use crate::boxed::PBoxIn;

    let mut pt = [0; MAX_ADDR];
    let alloc = mock_alloc(&mut pt, 0, MAX_ADDR);
    let mut values = Vec::new();
    while let Ok(value) = PBoxIn::try_new_in([0_u8; 1024], &alloc) {
        values.push(value);
    }
    assert!(
        !values.is_empty(),
        "allocator must admit at least one value"
    );
    drop(values);
    assert!(PBoxIn::try_new_in([0_u8; 1024], &alloc).is_ok());
}

#[test]
fn token_rejects_another_allocator_layout_and_returns_ownership() {
    use crate::mem::TransferError;
    use crate::tests::Info;

    let mut left = [0; MAX_ADDR];
    let mut right = [0; MAX_ADDR];
    let left = mock_alloc(&mut left, 0, MAX_ADDR);
    let right = mock_alloc(&mut right, 64, MAX_ADDR - 64);
    let record = crate::boxed::PBoxIn::new_in(Info::mock(), &left)
        .token_of()
        .pack(());

    let error = record.open::<Info, _>(&right).unwrap_err();
    assert_eq!(
        error.kind,
        crate::token::OpenKind::Reconstruct(crate::token::ReconstructError::Transfer(
            TransferError::WrongAllocator
        ))
    );
    let (_, value) = error.record.open::<Info, _>(&left).unwrap();
    assert!(value.version < 100);
}
