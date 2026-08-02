use crate::Session;
use crate::msg::Repr;
use crate::tests;
use crate::tests::mock::{MAX_ADDR, MockBackend};

type MockAlloc = crate::talc::MapTalc;
type MockSession = Session;
type TestProtocol = ();

#[repr(C)]
struct Large([u64; 2]);
#[repr(transparent)]
#[derive(Debug)]
struct Small(u64);

// Deliberately violates Repr's schema-identity law to prove that exact extent
// admission still prevents a colliding schema from forming the wrong pointer.
unsafe impl Repr for Large {
    const SCHEMA: crate::schema::SchemaKey =
        crate::schema::SchemaKey::new(crate::schema::SchemaId(91), 1);
}
unsafe impl Repr for Small {
    const SCHEMA: crate::schema::SchemaKey =
        crate::schema::SchemaKey::new(crate::schema::SchemaId(91), 1);
}

fn mock_alloc(bk: &mut [u8], start: usize, size: usize) -> MockAlloc {
    let bk = MockBackend(bk);
    bk.shared(start, size).try_into().unwrap()
}

fn mock_session(bk: &mut [u8], start: usize, size: usize) -> MockSession {
    Session::from(MockBackend(bk).shared(start, size)).unwrap()
}

fn peer_session(bk: &mut [u8]) -> MockSession {
    Session::from(MockBackend(bk).map(
        0,
        MAX_ADDR,
        crate::schema::RegionAdmission::Expect(crate::schema::RegionId::new(0, 0)),
    ))
    .unwrap()
}

fn transfer_pool(session: &MockSession) -> crate::Pool {
    session.create_pool(32 * 1024, None).unwrap()
}

#[test]
fn create_channel_returns_one_admitted_role_and_its_peer_port() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session(&mut pt, 0, MAX_ADDR);
    let (channel, _port) = session.create_channel::<()>(3).expect("create channel");
    assert_eq!(channel.id().capacity(), 3);
    let (_tx, _rx) = channel.split();
}

#[test]
fn peer_port_admits_exactly_one_opposite_role() {
    let mut pt = [0; MAX_ADDR];
    let region = crate::schema::RegionId::new(0, 0);
    let first = mock_session(&mut pt, 0, MAX_ADDR);
    let (left, port) = first.create_channel::<()>(3).unwrap();
    let second = Session::from(MockBackend(&mut pt).map(
        0,
        MAX_ADDR,
        crate::schema::RegionAdmission::Expect(region),
    ))
    .unwrap();

    let right = second.adopt(port).expect("adopt peer role");
    let (_left_tx, _left_rx) = left.split();
    let (_right_tx, _right_rx) = right.split();
}

#[test]
fn reinvite_invalidates_the_previous_port_generation() {
    let mut pt = [0; MAX_ADDR];
    let region = crate::schema::RegionId::new(0, 0);
    let first = mock_session(&mut pt, 0, MAX_ADDR);
    let (left, stale) = first.create_channel::<()>(3).unwrap();
    let current = left.invite().expect("replace invitation");
    let second = Session::from(MockBackend(&mut pt).map(
        0,
        MAX_ADDR,
        crate::schema::RegionAdmission::Expect(region),
    ))
    .unwrap();

    let rejected = match second.adopt(stale) {
        Err(rejected) => rejected,
        Ok(_) => panic!("stale Port was admitted"),
    };
    assert!(matches!(rejected, crate::AdmitError::Stale(_)));
    assert!(second.adopt(current).is_ok());
}

#[test]
fn removed_channel_is_terminal_and_cannot_reissue_the_peer_role() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session(&mut pt, 0, MAX_ADDR);
    let (channel, _port) = session.create_channel::<()>(3).unwrap();

    session.remove(channel).expect("remove empty channel");
}

#[test]
fn closed_channel_can_be_removed_after_its_last_local_role_drops() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session(&mut pt, 0, MAX_ADDR);
    let (channel, port) = session.create_channel::<()>(3).unwrap();
    assert!(session.remove(channel).is_ok());
    assert!(session.adopt(port).is_err());
}

#[test]
fn admitted_channel_receiver_reconstructs_pool_transfer() {
    let mut pt = [0; MAX_ADDR];
    let region = crate::schema::RegionId::new(0, 0);
    let first = mock_session(&mut pt, 0, MAX_ADDR);
    let first_pool = first.create_pool(32 * 1024, None).unwrap();
    let pool_id = first_pool.id();
    let (left, port) = first.create_channel::<()>(3).unwrap();
    let second = Session::from(MockBackend(&mut pt).map(
        0,
        MAX_ADDR,
        crate::schema::RegionAdmission::Expect(region),
    ))
    .unwrap();
    let second_pool = second.open_pool(pool_id).unwrap();
    let right = second.adopt(port).unwrap();
    let (send, _) = left.split();
    let (_, recv) = right.split();

    send.try_send(first_pool.as_ref().put(17_u64).unwrap().transfer(()))
        .unwrap();
    let (_, value): ((), crate::Block<'_, u64>) =
        match recv.claim().unwrap().adopt(second_pool.as_ref()) {
            Ok(value) => value,
            Err(_) => panic!("admitted receiver rejected matching Pool transfer"),
        };
    assert_eq!(*value, 17);
}

#[test]
fn empty_pool_transfer_has_no_shared_allocation_to_adopt() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session(&mut pt, 0, MAX_ADDR);
    let pool = session.create_pool(32 * 1024, None).unwrap();
    let (left, port) = session.create_channel::<()>(1).unwrap();
    let peer = peer_session(&mut pt);
    let peer_pool = peer.open_pool(pool.id()).unwrap();
    let right = peer.adopt(port).unwrap();
    let (send, _) = left.split();
    let (_, recv) = right.split();

    send.try_send(pool.as_ref().copy::<u8>(&[]).unwrap().transfer(()))
        .unwrap();
    let (_, value) = recv
        .claim()
        .unwrap()
        .adopt::<[u8]>(peer_pool.as_ref())
        .unwrap();
    assert!(value.is_empty());
    drop(value);

    send.try_send(pool.as_ref().copy::<u8>(&[]).unwrap().transfer(()))
        .unwrap();
    recv.claim().unwrap().discard(peer_pool.as_ref()).unwrap();

    send.try_send(pool.as_ref().copy::<u8>(&[]).unwrap().transfer(()))
        .unwrap();
    drop((send, recv, right));
    session.remove(left).unwrap();
    drop((peer_pool, peer));
}

#[test]
fn zero_generation_cannot_authorize_nonempty_pool_bytes() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session(&mut pt, 0, MAX_ADDR);
    let pool = session.create_pool(32 * 1024, None).unwrap();
    let (mut allocation, mut token) = pool
        .as_ref()
        .copy(&[1_u8][..])
        .unwrap()
        .transfer(())
        .into_parts();
    allocation.detach().unwrap();
    token.token.generation = 0;
    assert!(pool.as_ref().adopt::<(), [u8]>(&token).is_err());
}

#[test]
fn pool_owns_initialization_release_and_exact_failure_values() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session(&mut pt, 0, MAX_ADDR);
    let pool = session.create_pool(64 * 1024, None).unwrap();
    let pool = pool.as_ref();

    let block = pool.reserve::<u64>().unwrap().write(41);
    assert_eq!(*block, 41);
    drop(block);
    assert_eq!(*pool.put(43_u64).unwrap(), 43);

    let bytes = pool.copy(b"evering").unwrap();
    assert_eq!(&*bytes, b"evering");
    let mut vacant = pool.reserve_bytes(8).unwrap();
    vacant.as_uninit().iter_mut().for_each(|byte| {
        byte.write(7);
    });
    let bytes = unsafe { vacant.assume_init(5) }.unwrap();
    assert_eq!(&*bytes, &[7; 5]);

    let vacant = pool.reserve_bytes(1).unwrap();
    let mut vacant = match unsafe { vacant.assume_init(2) } {
        Err(vacant) => vacant,
        Ok(_) => panic!("out-of-range completion was admitted"),
    };
    vacant.as_uninit()[0].write(3);
    assert_eq!(&*unsafe { vacant.assume_init(1) }.unwrap(), &[3]);
}

#[test]
fn explicit_pool_range_never_silently_shrinks() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session(&mut pt, 0, MAX_ADDR);
    let range = crate::BlockRange::new(64, 64 * 1024).unwrap();
    let error = match session.create_pool(64 * 1024, Some(range)) {
        Err(error) => error,
        Ok(_) => panic!("undersized Pool extent was accepted"),
    };
    assert!(matches!(
        error,
        crate::PoolCreateError::RequiredExtent { .. }
    ));
    assert!(session.create_pool(64 * 1024, None).is_ok());
}

#[test]
fn pool_reclaims_only_the_dead_participant_owner() {
    let mut pt = [0; MAX_ADDR];
    let region = crate::schema::RegionId::new(0, 0);
    let first = mock_session(&mut pt, 0, MAX_ADDR);
    let first_pool = first.create_pool(64 * 1024, None).unwrap();
    let id = first_pool.id();
    let second = Session::from(MockBackend(&mut pt).map(
        0,
        MAX_ADDR,
        crate::schema::RegionAdmission::Expect(region),
    ))
    .unwrap();
    let peer = second.peer();
    let pool = second.open_pool(id).unwrap();
    let block = pool.as_ref().put(17_u64).unwrap();
    core::mem::forget(block);
    core::mem::forget(pool);
    core::mem::forget(second);

    first.abandon_heap_for_test(peer.slot());
    let recovery = unsafe { first.assume_dead(peer) }.unwrap();
    assert!(recovery.reap().is_ok());
    let error = match first.heap().put(1_u8) {
        Err(error) => error.error,
        Ok(_) => panic!("poison admitted heap mutation"),
    };
    assert_eq!(error, crate::talc::MutationError::Poisoned);
    assert_eq!(*first_pool.as_ref().put(19_u64).unwrap(), 19);
}

#[test]
fn corrupt_transfer_evidence_retains_the_dead_participant() {
    let mut pt = [0; MAX_ADDR];
    let first = mock_session(&mut pt, 0, MAX_ADDR);
    let pool_id = first.create_pool(64 * 1024, None).unwrap().id();
    let (channel, port) = first.create_channel::<()>(1).unwrap();
    let second = peer_session(&mut pt);
    let dead = second.peer();
    let pool = second.open_pool(pool_id).unwrap();
    let peer_channel = second.adopt(port).unwrap();
    let (send, _) = peer_channel.split();
    let mut transfer = pool.as_ref().put(17_u64).unwrap().transfer(());
    transfer.token.token.pool.offset = u64::MAX;
    let staged = send.reserve().unwrap().stage(transfer);
    core::mem::forget(staged);
    core::mem::forget(send);
    core::mem::forget(peer_channel);
    core::mem::forget(pool);
    core::mem::forget(second);

    first.abandon_heap_for_test(dead.slot());
    let recovery = unsafe { first.assume_dead(dead) }.unwrap();
    assert!(recovery.reap().is_err());
    let replacement = peer_session(&mut pt);
    assert_ne!(replacement.peer().slot(), dead.slot());
    core::mem::forget(channel);
}

#[test]
fn pool_errors_keep_values_and_concurrent_claims_unique() {
    #[repr(C, align(128))]
    #[derive(Debug, PartialEq, Eq)]
    struct OverAligned(u64);

    unsafe impl Repr for OverAligned {
        const SCHEMA: crate::schema::SchemaKey =
            crate::schema::SchemaKey::new(crate::schema::SchemaId(0x504f_4f4c_414c_4947), 1);
    }

    let mut pt = [0; MAX_ADDR];
    let session = mock_session(&mut pt, 0, MAX_ADDR);
    let range = crate::BlockRange::new(64, 64).unwrap();
    let pool = session.create_pool(32 * 1024, Some(range)).unwrap();
    let pool = pool.as_ref();
    let error = match pool.put(OverAligned(7)) {
        Err(error) => error,
        Ok(_) => panic!("over-aligned value entered a 64-byte class"),
    };
    assert_eq!(
        error,
        crate::PoolReserveError::BlockTooLarge(OverAligned(7))
    );

    std::thread::scope(|scope| {
        let block = pool.put(5_u64).unwrap();
        scope.spawn(move || assert_eq!(*block, 5));
        for _ in 0..4 {
            scope.spawn(|| {
                for value in 0..1000_u64 {
                    assert_eq!(*pool.put(value).unwrap(), value);
                }
            });
        }
    });

    let mut blocks = Vec::new();
    loop {
        match pool.put(99_u64) {
            Ok(block) => blocks.push(block),
            Err(error) => {
                assert_eq!(error, crate::PoolReserveError::Unavailable(99));
                break;
            }
        }
    }
    assert_eq!(*pool.put(()).unwrap(), ());
    assert!(pool.copy::<u8>(&[]).unwrap().is_empty());
    drop(blocks.pop());
    assert_eq!(*pool.put(101_u64).unwrap(), 101);
}

#[test]
fn pool_geometry_is_queryable_aligned_and_nonoverlapping() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session(&mut pt, 0, MAX_ADDR);
    let pool = session.create_pool(64 * 1024, None).unwrap();
    let range = pool.range();
    let mut previous = 0;
    for index in 0.. {
        let Some(class) = pool.class(index) else {
            break;
        };
        assert!(class.bytes.is_power_of_two());
        assert!(class.bytes > previous && class.slots >= 64);
        previous = class.bytes;
    }
    assert_eq!((range.min(), range.max()), (64, previous));

    let pool = pool.as_ref();
    let small = pool.copy(&[1_u8; 64]).unwrap();
    let large = pool.copy(&[2_u8; 128]).unwrap();
    let small = small.as_ptr() as usize..small.as_ptr() as usize + small.len();
    let large = large.as_ptr() as usize..large.as_ptr() as usize + large.len();
    assert_eq!(small.start % 64, 0);
    assert_eq!(large.start % 128, 0);
    assert!(small.end <= large.start || large.end <= small.start);
}

#[test]
fn invalid_pool_geometry_does_not_publish_a_directory_entry() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session(&mut pt, 0, MAX_ADDR);
    assert!(crate::BlockRange::new(usize::MAX, usize::MAX).is_err());
    assert!(matches!(
        session.create_pool(1, None),
        Err(crate::PoolCreateError::RequiredExtent { .. })
    ));
    assert!(session.create_pool(32 * 1024, None).is_ok());
}

#[test]
fn heap_owns_values_without_exposing_allocator_plumbing() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session(&mut pt, 0, MAX_ADDR);
    let heap = session.heap();

    let value = heap.put(17_u64).expect("allocate value");
    assert_eq!(*value, 17);
}

#[test]
fn session_channel_count_is_bounded_by_shared_memory_not_a_type_constant() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session(&mut pt, 0, MAX_ADDR);

    let (first, _) = session.create_channel::<()>(1).expect("first channel");
    let (second, _) = session
        .create_channel::<()>(1)
        .expect("a directory-backed session is not limited to one channel");

    assert_ne!(first.id(), second.id());
}

#[test]
fn session_rejects_zero_capacity_without_panicking_or_publishing() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session(&mut pt, 0, MAX_ADDR);

    assert!(session.create_channel::<()>(0).is_err());
    assert!(session.create_channel::<()>(1).is_ok());
}

#[test]
fn duplex_sides_share_each_physical_direction_and_close_gate() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session(&mut pt, 0, MAX_ADDR);
    let pool = session.create_pool(32 * 1024, None).unwrap();
    let pool_id = pool.id();
    let (left, port) = session.create_channel::<()>(3).unwrap();
    let peer = peer_session(&mut pt);
    let peer_pool = peer.open_pool(pool_id).unwrap();
    let right = peer.adopt(port).unwrap();
    let (left_send, left_recv) = left.split();
    let (right_send, right_recv) = right.split();

    left_send
        .try_send(pool.as_ref().put(7_u64).unwrap().transfer(()))
        .unwrap();
    let (_, value) = right_recv
        .claim()
        .unwrap()
        .adopt::<u64>(peer_pool.as_ref())
        .unwrap();
    assert_eq!(*value, 7);

    left_send.close();
    assert!(matches!(
        right_recv.claim(),
        Err(crate::ReceiveError::Closed)
    ));
    right_send.close();
    assert!(matches!(
        left_recv.claim(),
        Err(crate::ReceiveError::Closed)
    ));
}

#[test]
fn transfer_admission_keeps_the_claim_until_pool_authority_moves() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session(&mut pt, 0, MAX_ADDR);
    let pool = session.create_pool(32 * 1024, None).unwrap();
    let pool_id = pool.id();
    let other = transfer_pool(&session);
    let (left, port) = session.create_channel::<()>(1).unwrap();
    let peer = peer_session(&mut pt);
    let peer_pool = peer.open_pool(pool_id).unwrap();
    let right = peer.adopt(port).unwrap();
    let (send, _) = left.split();
    let (_, recv) = right.split();

    send.try_send(pool.as_ref().put(17_u64).unwrap().transfer(()))
        .unwrap();
    let failed = match recv.claim().unwrap().adopt::<u64>(other.as_ref()) {
        Err(failed) => failed,
        Ok(_) => panic!("another Pool admitted the transfer"),
    };
    let received = match failed {
        crate::AdoptError::Pool(received) => received,
        _ => panic!("wrong Pool rejection"),
    };
    let failed = match received.adopt::<u32>(peer_pool.as_ref()) {
        Err(failed) => failed,
        Ok(_) => panic!("wrong runtime type admitted the transfer"),
    };
    let received = match failed {
        crate::AdoptError::Type(received) => received,
        _ => panic!("wrong type rejection"),
    };
    let (_, value) = received.adopt::<u64>(peer_pool.as_ref()).unwrap();
    assert_eq!(*value, 17);

    let mut stale = pool.as_ref().put(19_u64).unwrap().transfer(());
    stale.token.token.generation += 1;
    send.try_send(stale).unwrap();
    let failed = match recv.claim().unwrap().adopt::<u64>(peer_pool.as_ref()) {
        Err(failed) => failed,
        Ok(_) => panic!("stale participant generation admitted the transfer"),
    };
    assert!(matches!(failed, crate::AdoptError::Owned(_)));
    drop(failed);
    assert!(matches!(
        send.try_send(pool.as_ref().put(23_u64).unwrap().transfer(())),
        Err(crate::TrySendError::Full(_) | crate::TrySendError::Busy(_))
    ));
}

#[test]
fn remove_drains_both_directions_and_reclaims_the_dynamic_layout() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session(&mut pt, 0, MAX_ADDR);
    let pool = session.create_pool(32 * 1024, None).unwrap();
    let pool_id = pool.id();
    let (channel, port) = session.create_channel::<()>(3).unwrap();
    let removed = channel.id();
    let peer_session = peer_session(&mut pt);
    let peer_pool = peer_session.open_pool(pool_id).unwrap();
    let peer = peer_session.adopt(port).unwrap();
    let (left, _) = channel.split();
    let (right, _) = peer.split();
    left.try_send(pool.as_ref().put(11_u64).unwrap().transfer(()))
        .unwrap();
    right
        .try_send(peer_pool.as_ref().put(13_u64).unwrap().transfer(()))
        .unwrap();
    drop((left, right));

    drop(peer);
    assert!(session.remove(channel).is_ok());
    let (replacement, _) = session.create_channel::<()>(3).unwrap();
    assert_eq!(replacement.id().entry(), removed.entry());
    assert_eq!(replacement.id().generation(), removed.generation() + 1);
}

#[test]
fn remove_resolves_each_queued_pool_identity() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session(&mut pt, 0, MAX_ADDR);
    let range = crate::BlockRange::new(64, 64).unwrap();
    let first = session.create_pool(32 * 1024, Some(range)).unwrap();
    let second = session.create_pool(32 * 1024, Some(range)).unwrap();
    let (channel, port) = session.create_channel::<()>(2).unwrap();
    let peer_session = peer_session(&mut pt);
    let peer_first = peer_session.open_pool(first.id()).unwrap();
    let peer_second = peer_session.open_pool(second.id()).unwrap();
    let peer = peer_session.adopt(port).unwrap();
    let (send, _) = channel.split();
    let (peer_send, _) = peer.split();

    send.try_send(first.as_ref().put(11_u64).unwrap().transfer(()))
        .unwrap();
    peer_send
        .try_send(peer_second.as_ref().put(13_u64).unwrap().transfer(()))
        .unwrap();
    drop((send, peer_send, peer, peer_first, peer_second));

    session.remove(channel).unwrap();
    for pool in [&first, &second] {
        let slots = pool.class(0).unwrap().slots;
        let mut blocks = Vec::with_capacity(slots);
        while let Ok(block) = pool.as_ref().put(1_u64) {
            blocks.push(block);
        }
        assert_eq!(blocks.len(), slots);
    }
}

#[test]
fn remove_returns_the_channel_unchanged_while_a_local_endpoint_exists() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session(&mut pt, 0, MAX_ADDR);
    let (channel, _) = session.create_channel::<()>(2).unwrap();
    let endpoint = channel.split().0;
    let channel = match session.remove(channel).unwrap_err() {
        crate::RemoveError::Busy(channel) | crate::RemoveError::Evidence(channel) => channel,
    };
    drop(endpoint);
    assert!(session.remove(channel).is_ok());
}

#[test]
fn remove_returns_the_channel_while_the_peer_role_is_owned() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session(&mut pt, 0, MAX_ADDR);
    let (channel, port) = session.create_channel::<()>(2).unwrap();
    let peer_session = peer_session(&mut pt);
    let peer = peer_session.adopt(port).unwrap();

    let channel = match session.remove(channel).unwrap_err() {
        crate::RemoveError::Busy(channel) | crate::RemoveError::Evidence(channel) => channel,
    };
    drop(peer);
    assert!(session.remove(channel).is_ok());
}

#[test]
fn session_accepts_a_const_explicit_geometry_covering_its_bound() {
    const GEOMETRY: crate::talc::Geometry = match crate::talc::Geometry::new(MAX_ADDR, 5, 10, 2) {
        Ok(geometry) => geometry,
        Err(_) => panic!("valid test geometry"),
    };

    let mut pt = [0; MAX_ADDR];
    let session =
        Session::from_geometry(MockBackend(&mut pt).shared(0, MAX_ADDR), GEOMETRY).unwrap();
    let heap = session.heap();
    assert_eq!(*heap.put(29_u32).unwrap(), 29);
}

#[test]
fn heap_preserves_exact_allocation_failures() {
    let mut pt = [0; MAX_ADDR];
    let session = mock_session(&mut pt, 0, MAX_ADDR);
    let heap = session.heap();
    let overflow = match heap.init::<u64>(usize::MAX, |_| 0) {
        Err(error) => error,
        Ok(_) => panic!("overflow was admitted"),
    };
    assert_eq!(overflow, crate::talc::MutationError::LayoutOverflow);

    let mut records = Vec::new();
    loop {
        match heap.copy(&[0_u8; 1024]) {
            Ok(record) => records.push(record),
            Err(error) => {
                assert_eq!(error, crate::talc::MutationError::Exhausted);
                break;
            }
        }
    }
    assert!(!records.is_empty());
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
fn exhaustion_recovers_after_release() {
    use crate::boxed::PBox;

    let mut pt = [0; MAX_ADDR];
    let alloc = mock_alloc(&mut pt, 0, MAX_ADDR);
    let mut values = Vec::new();
    while let Ok(value) = PBox::try_new_in([0_u8; 1024], alloc.as_ref()) {
        values.push(value);
    }
    assert!(
        !values.is_empty(),
        "allocator must admit at least one value"
    );
    drop(values);
    assert!(PBox::try_new_in([0_u8; 1024], alloc.as_ref()).is_ok());
}

#[test]
fn rejected_box_release_returns_the_live_value() {
    use crate::boxed::PBox;

    let mut pt = [0; MAX_ADDR];
    let alloc = mock_alloc(&mut pt, 0, MAX_ADDR);
    let value = PBox::try_new_in(41_u64, alloc.as_ref()).unwrap();
    let dead = 61;
    alloc.abandon_mutation_for_test(dead);

    let value = value.release().expect_err("held mutation rejects release");
    assert_eq!(*value, 41);
    assert!(alloc.clear_dead_owner(dead));
    assert!(value.release().is_ok());
}

#[test]
fn rejected_box_drop_does_not_drop_the_value() {
    use crate::boxed::PBox;
    use core::sync::atomic::{AtomicUsize, Ordering};

    struct Droppy<'a>(&'a AtomicUsize);
    impl Drop for Droppy<'_> {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    let mut pt = [0; MAX_ADDR];
    let alloc = mock_alloc(&mut pt, 0, MAX_ADDR);
    let drops = AtomicUsize::new(0);
    let value = PBox::try_new_in(Droppy(&drops), alloc.as_ref()).unwrap();
    let dead = 61;
    alloc.abandon_mutation_for_test(dead);
    drop(value);

    assert_eq!(drops.load(Ordering::Relaxed), 0);
    assert!(alloc.clear_dead_owner(dead));
}

#[test]
fn zero_sized_box_release_does_not_mutate_talc() {
    use crate::boxed::PBox;
    use core::sync::atomic::{AtomicUsize, Ordering};

    static DROPS: AtomicUsize = AtomicUsize::new(0);
    struct Zst;
    impl Drop for Zst {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    let mut pt = [0; MAX_ADDR];
    let alloc = mock_alloc(&mut pt, 0, MAX_ADDR);
    DROPS.store(0, Ordering::Relaxed);
    let dead = 61;
    alloc.abandon_mutation_for_test(dead);

    assert!(
        PBox::try_new_in(Zst, alloc.as_ref())
            .unwrap()
            .release()
            .is_ok()
    );
    assert_eq!(DROPS.load(Ordering::Relaxed), 1);
    assert!(alloc.clear_dead_owner(dead));
}
