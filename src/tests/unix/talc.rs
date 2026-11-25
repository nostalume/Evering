//! The allocator on real bytes: `talc` driven over a memfd mapping rather than the heap, which is where
//! a block address stops being whatever the allocator returned and becomes a number a peer has to reach.
//! `mock_alloc`, `fd.dup()` and `fd.mapping().at(hint)` are the only makers, and the assertions ask
//! `crate::mem::REGION_ALIGN` instead of assuming a residue, so a mapping that stops arranging the
//! alignment it is asked for fails here rather than in the field.
//!
//! Two things these cases cannot show. Both mappings of one file live in this process, so the ordering
//! that only a second process exposes — a peer mapping while the creator is still publishing its
//! header, or still holding a promise over a slot — is claimed by `tests/recovery_conserves.rs` and
//! `src/tests/unix/recovery.rs`, not by this file. And
//! the class widths used here sit under the platform's geometry cap, so an over-wide class is refused by
//! the mock suite's `ClassTooWide` case and never arrives on a real mapping.

use crate::os::unix::UnixFd;

use crate::mem::{Access, Request, Source};
use crate::place::{RegionAdmission, RegionId};
use crate::tests;

type UnixAlloc = crate::talc::MapTalc;
fn mock_alloc(name: &str, size: usize) -> UnixAlloc {
    let fd = UnixFd::memfd(name, size, false).expect("should create");
    let map = fd
        .map(Request::new(size, Access::WRITE | Access::READ))
        .unwrap();
    crate::mem::Build::new(
        map,
        RegionAdmission::Create(RegionId::new(0x5441_4c43, size as u64)),
    )
    .unwrap()
    .try_into()
    .unwrap()
}

/// A Pool aligns its class payloads against the absolute address its creator's mapping happened to
/// receive, while the directory records them as offsets from that base. `mmap` promises only page
/// alignment and treats a hint as advisory, so a second mapping of the same file used to land on a
/// different residue and read every block off its own alignment.
#[test]
fn a_pool_opens_in_a_second_mapping_of_the_same_region() {
    const SIZE: usize = 1 << 24;
    const REGION: RegionId = RegionId::new(0x0052_4553, 3);
    let fd = UnixFd::memfd("second-mapping", SIZE, false).expect("create shared memory");
    let parent = crate::Session::create(
        fd.dup().expect("duplicate shared-memory handle"),
        Request::new(SIZE, Access::WRITE | Access::READ),
        REGION,
    )
    .expect("create session");

    // Deliberately one page off the residue the parent landed on: an address a peer may be handed
    // without asking, which the mapping has to make usable rather than merely check afterwards.
    let hint = parent.base_addr() + (1 << 30) + 4096;
    let child = crate::Session::open(
        fd.mapping().at(hint),
        Request::new(SIZE, Access::WRITE | Access::READ),
        REGION,
    )
    .expect("join the same region");
    assert_eq!(
        child.base_addr() % crate::mem::REGION_ALIGN,
        parent.base_addr() % crate::mem::REGION_ALIGN,
        "a peer can only reproduce a block address whose alignment it reaches from its own base"
    );

    let pool = parent
        .create_pool(
            4 * 1024 * 1024,
            Some(crate::BlockRange::new(64, 8 * 1024).unwrap()),
        )
        .expect("a pool whose widest class is above page alignment");
    let opened = child
        .open_pool(pool.id())
        .expect("the second mapping opens the same pool");
    let wide = opened
        .as_ref()
        .copy(&[7_u8; 8 * 1024])
        .expect("the widest class is usable from the second mapping");
    assert_eq!(wide[8 * 1024 - 1], 7);
}

#[test]
fn layout_order_is_rejected_at_the_first_unexpected_record() {
    const SIZE: usize = 1 << 19;
    const REGION: RegionId = RegionId::new(0x004f_5244_4552, 1);
    let fd = UnixFd::memfd("layout-order", SIZE, false).expect("create shared memory");
    let created = crate::Session::create(
        fd.dup().expect("duplicate shared-memory handle"),
        Request::new(SIZE, Access::WRITE | Access::READ),
        REGION,
    )
    .expect("record canonical layout order");
    drop(created);

    let map = fd
        .map(Request::new(SIZE, Access::WRITE | Access::READ))
        .unwrap();
    let mut layout =
        crate::mem::Build::new(map, RegionAdmission::Expect(REGION)).expect("map region");
    let conf = crate::talc::Config::new(SIZE);
    let reserve = layout
        .reserve::<crate::talc::Header>()
        .expect("reserve allocator");
    let conf = conf.with_bound(reserve.remaining_after());

    let error = reserve
        .commit(conf)
        .err()
        .expect("reversed layout order must reject");

    assert!(matches!(
        error,
        crate::mem::Error::LayoutMismatch(crate::header::LayoutField::Magic)
    ));
    assert!(matches!(
        layout.push::<crate::dir::Header>(()),
        Err(crate::mem::Error::PoisonedComposition)
    ));
}

#[test]
fn alloc_content() {
    // 2 kb
    const BYTES_SIZE: usize = 20;
    const ALLOC_NUM: usize = 200;
    const NUM: usize = 500;

    const NAME: &str = "alloc";
    const SIZE: usize =
        crate::numeric::max_usize(BYTES_SIZE * ALLOC_NUM, 10000).next_power_of_two();

    let a = mock_alloc(NAME, SIZE);

    tests::alloc_content::<BYTES_SIZE, ALLOC_NUM, NUM>(a);
}

#[test]
fn real_mapping_reuse_and_boxes() {
    const NAME: &str = "talc-reuse";
    const SIZE: usize = 1 << 19;

    tests::alloc_lines::<2048, 200, 5>(mock_alloc(NAME, SIZE));
    tests::pbox_rand::<100, 1>(mock_alloc("talc-rand", SIZE));
}

/// A member slot can be named in the roster before the process that will fill it exists, which makes
/// it a promise two mappings have to agree about: one holds it, the other arrives in it, and neither
/// can be fooled about which is which.
#[test]
fn a_promised_seat_is_arrived_in_by_its_generation_and_by_nobody_else() {
    const SIZE: usize = 1 << 20;
    const REGION: RegionId = RegionId::new(0x0053_E470, 1);
    let fd = UnixFd::memfd("promised-seat", SIZE, false).expect("create shared memory");
    let request = || Request::new(SIZE, Access::WRITE | Access::READ);
    let peer = || fd.dup().expect("duplicate shared-memory handle").mapping();
    let parent = crate::Session::create(peer(), request(), REGION).expect("create session");
    let seat = parent.reserve_seat().expect("promise a member slot");
    let promised = seat.peer();
    assert_eq!(seat.region(), REGION);
    assert_ne!(promised.slot(), parent.peer().slot());

    // The promise keeps its slot out of the ordinary claim, so a peer that joins without one is
    // somebody else.
    let joiner = crate::Session::open(peer(), request(), REGION).expect("join as a new member");
    assert_ne!(joiner.peer(), promised);

    let arrived = crate::Session::open_as(peer(), request(), seat.region(), promised)
        .expect("arrive in the promised slot");
    assert_eq!(arrived.peer(), promised);

    // Arrival consumes the promise: the seat is no longer its holder's to give back, and the member
    // it named is not a second arrival's to take.
    drop(seat);
    assert_eq!(arrived.peer(), promised);
    let displaced = crate::Session::open_as(peer(), request(), REGION, promised);
    assert!(matches!(
        displaced,
        Err(crate::SessionError::ArrivalRefused)
    ));
}

#[test]
fn a_seat_given_back_before_its_arrival_returns_its_slot_to_the_next_join() {
    const SIZE: usize = 1 << 20;
    const REGION: RegionId = RegionId::new(0x0053_E471, 1);
    let fd = UnixFd::memfd("returned-seat", SIZE, false).expect("create shared memory");
    let request = || Request::new(SIZE, Access::WRITE | Access::READ);
    let peer = || fd.dup().expect("duplicate shared-memory handle").mapping();
    let parent = crate::Session::create(peer(), request(), REGION).expect("create session");
    let seat = parent.reserve_seat().expect("promise a member slot");
    let promised = seat.peer();
    drop(seat);

    let later = crate::Session::open(peer(), request(), REGION)
        .expect("join after the promise was given back");
    assert_eq!(later.peer().slot(), promised.slot());
    assert_eq!(
        later.peer().generation(),
        promised.generation() + 1,
        "a promise spends its generation even when nobody arrives in it"
    );

    let late = crate::Session::open_as(peer(), request(), REGION, promised);
    assert!(matches!(late, Err(crate::SessionError::ArrivalRefused)));
}

#[test]
fn a_roster_full_of_promises_refuses_a_join_differently_than_a_roster_full_of_members() {
    const SIZE: usize = 1 << 20;
    const REGION: RegionId = RegionId::new(0x0053_E472, 1);
    let fd = UnixFd::memfd("promised-out", SIZE, false).expect("create shared memory");
    let request = || Request::new(SIZE, Access::WRITE | Access::READ);
    let peer = || fd.dup().expect("duplicate shared-memory handle").mapping();
    let parent = crate::Session::create(peer(), request(), REGION).expect("create session");

    let mut seats = Vec::new();
    while let Ok(seat) = parent.reserve_seat() {
        seats.push(seat);
    }
    assert_eq!(
        seats.len(),
        crate::header::PARTICIPANT_CAPACITY - 1,
        "the member the region was created with holds one slot"
    );
    assert!(matches!(
        parent.reserve_seat(),
        Err(crate::SeatError::NoRoom {
            waits: crate::RoomWait::Promise
        })
    ));

    // Nothing has arrived in any of them, so the region holds no member for anybody to reap: the
    // refusal is one a caller can wait out, which is the answer `RoomWait::Grow` does not give.
    let refused = crate::Session::open(peer(), request(), REGION);
    assert!(matches!(
        refused,
        Err(crate::SessionError::NoRoom {
            waits: crate::RoomWait::Promise
        })
    ));

    let freed = seats.pop().expect("a promise to give back");
    let slot = freed.peer().slot();
    drop(freed);
    let joined = crate::Session::open(peer(), request(), REGION)
        .expect("join into the slot a promise gave back");
    assert_eq!(joined.peer().slot(), slot);
}
