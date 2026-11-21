//! The suite on a region that is only a buffer: `MockBackend` turns any `[u8; MAX_ADDR]` into a `Map` whose
//! release zeroes the extent, so a whole region — root, directory, allocator header — exists with no kernel
//! object anywhere. That is what makes these the cases that need to interleave rather than to share: two
//! threads racing one rolled-back create, a reap that must stand down while a live mutation is still landing,
//! poison and quarantine decided by which word a dead owner left behind, an admit that refuses a member still
//! mapped, and the release accounting below. Only the cases change state, and `MockBackend::map` bounds every
//! extent it hands out, so no fixture can write past the array. Several drive the directory through states the
//! real vocabulary cannot reach by defining a fixture `Layout` inline (`evering.test.*` schemas), so a change
//! to `init`/`attach`/`Status` lands here before it lands anywhere else.
//!
//! Two things the shape cannot carry. Nothing here constrains placement: the array is as aligned as whatever
//! allocated it and `mock_view` accepts any `start`, so an assertion about an address or a residue in this file
//! proves the host and not the library — the peer-visible version of that claim belongs to `tests::unix`. And
//! `RELEASES` is one process-global counter that a single case installs through `count_release`: the "released
//! exactly once" assertion is sound only because nothing else routes through it, so a second user — or a `Build`
//! that counted releases generally — would leave an assertion that still passes `--test-threads=1` and fails in
//! a normal run.

#![cfg(test)]

mod refusals;
mod talc;

use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::mem::{Access, Build, Map, Request, Source};

const MAX_ADDR: usize = 0x20000;

type MockPageTable = [MockFlags];
type MockFlags = u8;

struct MockBackend<'a>(&'a mut MockPageTable);

unsafe fn release(start: NonNull<u8>, len: usize) -> bool {
    unsafe { core::ptr::write_bytes(start.as_ptr(), 0, len) };
    true
}

static RELEASES: AtomicUsize = AtomicUsize::new(0);

unsafe fn count_release(start: NonNull<u8>, len: usize) -> bool {
    RELEASES.fetch_add(1, Ordering::Relaxed);
    unsafe { release(start, len) }
}

impl MockBackend<'_> {
    fn shared(self, start: usize, size: usize) -> Build {
        self.map(
            start,
            size,
            crate::schema::RegionAdmission::Create(crate::schema::RegionId::new(0, start as u64)),
        )
    }

    fn map(self, start: usize, size: usize, admission: crate::schema::RegionAdmission) -> Build {
        assert!(
            start
                .checked_add(size)
                .is_some_and(|end| end <= self.0.len())
        );
        let pointer = NonNull::new(unsafe { self.0.as_mut_ptr().add(start) }).unwrap();
        let map =
            unsafe { Map::from_raw_parts(pointer, size, Access::READ | Access::WRITE, release) };
        Build::new(map, admission).unwrap()
    }
}

fn mock_view(bk: &mut [u8], start: usize, size: usize) -> Build {
    let bk = MockBackend(bk);
    bk.shared(start, size)
}

/// One buffer offered as a `Source`, so an entrance can be asked for the declaration it will be held to.
///
/// `MockBackend` hands out writable maps and unwraps its own admission, which is the right shape for a
/// member and the wrong one for a reader: the cases below need the same extent under a narrower
/// declaration, and they need to see the refusal rather than panic past it.
#[derive(Clone, Copy)]
struct Borrowed {
    start: *mut u8,
    len: usize,
}

unsafe impl Source for Borrowed {
    type Error = core::convert::Infallible;

    fn map(self, request: Request) -> Result<Map, Self::Error> {
        assert!(
            request.len <= self.len,
            "the fixture extent is shorter than the request"
        );
        // SAFETY: the buffer outlives every view a case makes of it, the request says what the bytes
        // were granted for, and there is nothing to give back — which is what `from_static` declares
        // rather than what `from_raw_parts` would be lied about.
        Ok(unsafe {
            Map::from_static(
                NonNull::new_unchecked(self.start),
                request.len,
                request.access,
            )
        })
    }
}

#[test]
fn area_init() {
    const STEP: usize = 0x2000;
    let mut pt = [0; MAX_ADDR];
    for start in (0..MAX_ADDR).step_by(STEP) {
        let a = mock_view(&mut pt, start, STEP);
        tracing::debug!("{:?}", a.header());
    }
}

#[test]
fn directory_create_publishes_only_an_initialized_talc_allocation() {
    use crate::header::{Layout, Status};
    use crate::schema::{LayoutContext, SchemaKey, SharedSchema, schema_id};

    struct Created(u32);
    struct Reject;

    impl SharedSchema for Created {
        const SCHEMA: SchemaKey = SchemaKey::new(schema_id("evering.test.created"), 1);
    }

    unsafe impl Layout for Created {
        type Config = u32;
        type Info = ();

        const MAGIC: crate::header::Magic = 0xC17;

        fn info(_: &Self::Config, _: LayoutContext) {}

        unsafe fn init(destination: *mut Self, value: Self::Config) -> Status {
            unsafe { destination.write(Self(value)) };
            Status::Initialized
        }

        fn attach(&self, value: &Self::Config) -> Status {
            if self.0 == *value {
                Status::Initialized
            } else {
                Status::Corrupted
            }
        }
    }

    impl SharedSchema for Reject {
        const SCHEMA: SchemaKey = SchemaKey::new(schema_id("evering.test.reject"), 1);
    }

    unsafe impl Layout for Reject {
        type Config = ();
        type Info = ();

        const MAGIC: crate::header::Magic = 0xBAD;

        fn info(_: &Self::Config, _: LayoutContext) {}

        unsafe fn init(_: *mut Self, _: Self::Config) -> Status {
            Status::Corrupted
        }

        fn attach(&self, _: &Self::Config) -> Status {
            Status::Corrupted
        }
    }

    let mut page_table = [0; MAX_ADDR];
    let mut area = MockBackend(&mut page_table).shared(0, MAX_ADDR);
    let directory = area.push::<crate::dir::Header>(()).unwrap();
    let reserve = area.reserve::<crate::talc::Header>().unwrap();
    let conf = crate::talc::Config::new(MAX_ADDR).with_bound(reserve.remaining_after());
    let heap = crate::talc::MapTalc::from_handle(reserve.commit(conf).unwrap());

    assert!(matches!(
        directory.create::<Reject>(&heap, ()),
        Err(crate::dir::Error::Admission(_))
    ));
    let (id, created) = directory.create::<Created>(&heap, 91).unwrap();
    assert_eq!(created.inner.0, 91);
    drop(created);
    let reopened = directory.open(id, 91).unwrap();
    assert_eq!(reopened.inner.0, 91);
    if let Err((error, _)) = directory.remove_in(
        &heap,
        id,
        reopened,
        core::alloc::Layout::new::<crate::header::RcHeader<Created>>(),
    ) {
        panic!("remove failed: {error:?}");
    }
    assert!(matches!(
        directory.open(id, 91),
        Err(crate::dir::Error::Stale)
    ));

    let dead = directory.peer().slot().wrapping_add(1);
    directory.abandon_released_for_test(id, dead);
    assert_eq!(
        directory.recover(&heap, dead),
        Ok(crate::dir::Recovered::Released)
    );
    directory.abandon_held_reservation_for_test(dead);
    assert_eq!(
        directory.recover(&heap, dead),
        Ok(crate::dir::Recovered::RolledBack)
    );

    let layout = core::alloc::Layout::new::<crate::header::RcHeader<Created>>();
    let (before_close, mapped) = directory.create::<Created>(&heap, 93).unwrap();
    directory.abandon_prepared_remove_for_test(&heap, before_close, mapped, layout, dead, false);
    assert_eq!(
        directory.recover(&heap, dead),
        Ok(crate::dir::Recovered::RolledBack)
    );
    assert_eq!(directory.open(before_close, 93).unwrap().inner.0, 93);

    let (after_close, mapped) = directory.create::<Created>(&heap, 94).unwrap();
    directory.abandon_prepared_remove_for_test(&heap, after_close, mapped, layout, dead, true);
    assert_eq!(
        directory.recover(&heap, dead),
        Ok(crate::dir::Recovered::Released)
    );
    assert!(matches!(
        directory.open(after_close, 94),
        Err(crate::dir::Error::Stale)
    ));

    let (_, replacement) = directory.create::<Created>(&heap, 92).unwrap();
    assert_eq!(replacement.inner.0, 92);

    let (vacant, mapped) = directory.create::<Created>(&heap, 95).unwrap();
    drop(mapped);
    directory.abandon_vacant_remove_for_test(vacant, dead);
    heap.abandon_mutation_for_test(dead);
    assert_eq!(
        directory.recover(&heap, dead),
        Ok(crate::dir::Recovered::Released)
    );
    assert!(heap.finish_recovery(dead));
    assert!(heap.finish_recovery(dead));
    assert_eq!(
        heap.allocate(core::alloc::Layout::new::<u64>()),
        Err(crate::mem::DeclineCause::Poisoned)
    );
}

#[test]
fn recovery_stands_down_when_a_live_mutation_finishes_inside_its_clear() {
    use crate::header::{Layout, Status};
    use crate::schema::{LayoutContext, SchemaKey, SharedSchema, schema_id};

    struct Entry(u32);

    impl SharedSchema for Entry {
        const SCHEMA: SchemaKey = SchemaKey::new(schema_id("evering.test.standdown"), 1);
    }

    unsafe impl Layout for Entry {
        type Config = u32;
        type Info = ();

        const MAGIC: crate::header::Magic = 0xC18;

        fn info(_: &Self::Config, _: LayoutContext) {}

        unsafe fn init(destination: *mut Self, value: Self::Config) -> Status {
            unsafe { destination.write(Self(value)) };
            Status::Initialized
        }

        fn attach(&self, value: &Self::Config) -> Status {
            if self.0 == *value {
                Status::Initialized
            } else {
                Status::Corrupted
            }
        }
    }

    let mut page_table = [0; MAX_ADDR];
    let mut area = MockBackend(&mut page_table).shared(0, MAX_ADDR);
    let directory = area.push::<crate::dir::Header>(()).unwrap();
    let reserve = area.reserve::<crate::talc::Header>().unwrap();
    let conf = crate::talc::Config::new(MAX_ADDR).with_bound(reserve.remaining_after());
    let heap = crate::talc::MapTalc::from_handle(reserve.commit(conf).unwrap());
    let layout = core::alloc::Layout::new::<crate::header::RcHeader<Entry>>();

    let dead = directory.peer().slot().wrapping_add(1);
    let live = directory.peer().slot().wrapping_add(2);
    let (id, mapped) = directory.create::<Entry>(&heap, 96).unwrap();
    directory.abandon_prepared_remove_for_test(&heap, id, mapped, layout, dead, false);
    // A live participant has since taken the heap for its own mutation, so the word a reaper of
    // the dead slot tries to clear is not the one it expects.
    assert!(heap.clear_dead_owner(dead));
    heap.abandon_mutation_for_test(live);

    crate::dir::stand_down_window_for_test(live);
    assert_eq!(
        directory.recover(&heap, dead),
        Err(crate::dir::Error::Stale),
        "a heap that turned clean inside the window belongs to a mutation this reaper never saw"
    );
    assert_eq!(
        directory.recover(&heap, dead),
        Ok(crate::dir::Recovered::RolledBack),
        "standing down changed nothing, so the entry is still waiting for its repair"
    );
    assert_eq!(directory.open(id, 96).unwrap().inner.0, 96);
}

#[test]
fn recovery_refuses_a_staged_span_outside_the_region() {
    use crate::header::{Layout, Status};
    use crate::schema::{LayoutContext, SchemaKey, SharedSchema, schema_id};

    struct Staged(u32);

    impl SharedSchema for Staged {
        const SCHEMA: SchemaKey = SchemaKey::new(schema_id("evering.test.stagedspan"), 1);
    }

    unsafe impl Layout for Staged {
        type Config = u32;
        type Info = ();

        const MAGIC: crate::header::Magic = 0xC19;

        fn info(_: &Self::Config, _: LayoutContext) {}

        unsafe fn init(destination: *mut Self, value: Self::Config) -> Status {
            unsafe { destination.write(Self(value)) };
            Status::Initialized
        }

        fn attach(&self, value: &Self::Config) -> Status {
            if self.0 == *value {
                Status::Initialized
            } else {
                Status::Corrupted
            }
        }
    }

    // The region is the first half of the buffer, so the tail is memory the region has no claim on.
    // A reaper that takes the span a dead peer staged on faith reads and writes there, and this test
    // can see it happen.
    const REGION: usize = MAX_ADDR / 2;
    let mut page_table = [0; MAX_ADDR];
    let mut area = MockBackend(&mut page_table).shared(0, REGION);
    let directory = area.push::<crate::dir::Header>(()).unwrap();
    let reserve = area.reserve::<crate::talc::Header>().unwrap();
    let conf = crate::talc::Config::new(REGION).with_bound(reserve.remaining_after());
    let heap = crate::talc::MapTalc::from_handle(reserve.commit(conf).unwrap());
    let layout = core::alloc::Layout::new::<crate::header::RcHeader<Staged>>();
    let dead = directory.peer().slot().wrapping_add(1);
    let (id, mapped) = directory.create::<Staged>(&heap, 96).unwrap();
    directory.abandon_prepared_remove_for_test(&heap, id, mapped, layout, dead, false);

    // The staged span now names a block past the end of the region, and the bytes there are filled
    // so the member reads as closed — the branch that hands the block to the allocator.
    page_table[REGION..].fill(0xff);
    directory.forge_pending_span_for_test(crate::talc::Meta::span_at_for_test(
        REGION + 0x400,
        layout.size(),
    ));

    assert_eq!(
        directory.recover(&heap, dead),
        Err(crate::dir::Error::Admission(
            crate::mem::ProjectionError::OutOfBounds
        )),
        "the span a dead peer staged is peer-supplied input, so a reap of one that leaves the \
         region has to refuse rather than act on it"
    );
    assert!(
        page_table[REGION..].iter().all(|&byte| byte == 0xff),
        "a refusal must not have touched the memory the forged span named"
    );

    // The other half of the same rule: an in-region span the allocator cannot read its own header
    // at. `deallocate` dereferences a `Tag` just below the pointer it is handed, so an address that
    // is not aligned for it faults inside the allocator rather than returning.
    directory.forge_pending_span_for_test(crate::talc::Meta::span_at_for_test(0x401, 96));
    assert_eq!(
        directory.recover(&heap, dead),
        Err(crate::dir::Error::Admission(
            crate::mem::ProjectionError::Misaligned
        )),
        "a staged span inside the region still has to be an address the allocator can free through"
    );
    assert!(
        matches!(directory.open(id, 96), Err(crate::dir::Error::Stale)),
        "a refused reap leaves the member exactly where the dead peer left it: still closing, \
         waiting for a participant that can adjudicate it"
    );
    assert!(
        heap.allocate(core::alloc::Layout::new::<u64>()).is_ok(),
        "refusing had to hand the heap back rather than leave a poisoned mutation nobody corrupted"
    );
}

#[test]
fn recovery_hands_both_authorities_back_when_a_staged_block_cannot_be_freed() {
    let mut page_table = [0; MAX_ADDR];
    let mut area = MockBackend(&mut page_table).shared(0, MAX_ADDR);
    let directory = area.push::<crate::dir::Header>(()).unwrap();
    let reserve = area.reserve::<crate::talc::Header>().unwrap();
    let conf = crate::talc::Config::new(MAX_ADDR).with_bound(reserve.remaining_after());
    let heap = crate::talc::MapTalc::from_handle(reserve.commit(conf).unwrap());
    let dead = directory.peer().slot().wrapping_add(1);

    // A staged CREATE is settled by freeing the block the dead peer had just been given, and the
    // span naming it is the dead peer's own bytes. This one lies past the end of the region, and the
    // arm that frees it has already taken the heap and the transaction word by then.
    directory.abandon_allocated_create_for_test(
        dead,
        crate::talc::Meta::span_at_for_test(MAX_ADDR + 0x400, 96),
    );
    heap.abandon_mutation_for_test(dead);
    assert!(
        heap.clear_dead_owner(dead),
        "the dead peer's mutation is the one this reap is settling"
    );

    assert_eq!(
        directory.recover(&heap, dead),
        Err(crate::dir::Error::Admission(
            crate::mem::ProjectionError::OutOfBounds
        )),
        "freeing through a span the region cannot reach has to be refused, not attempted"
    );
    assert!(
        heap.allocate(core::alloc::Layout::new::<u64>()).is_ok(),
        "a refusal comes back with the heap usable: the mutation was handed over, never corrupted"
    );
    assert_eq!(
        directory.recover(&heap, dead),
        Err(crate::dir::Error::Stale),
        "the transaction word went back clean too, so the region stays workable even though this \
         member is left for a participant that can adjudicate it"
    );
}

#[test]
fn ambiguous_dead_allocator_mutation_is_poisoned_and_its_entry_quarantined() {
    let mut page_table = [0; MAX_ADDR];
    let mut area = MockBackend(&mut page_table).shared(0, MAX_ADDR);
    let directory = area.push::<crate::dir::Header>(()).unwrap();
    let reserve = area.reserve::<crate::talc::Header>().unwrap();
    let conf = crate::talc::Config::new(MAX_ADDR).with_bound(reserve.remaining_after());
    let heap = crate::talc::MapTalc::from_handle(reserve.commit(conf).unwrap());
    let meta = heap
        .allocate(core::alloc::Layout::new::<u64>())
        .expect("test allocation");
    let dead = directory.peer().slot().wrapping_add(1);

    directory.abandon_allocated_create_for_test(dead, meta);
    heap.abandon_mutation_for_test(dead);

    assert!(!heap.finish_recovery(dead.wrapping_add(1)));
    assert_eq!(
        heap.allocate(core::alloc::Layout::new::<u64>()),
        Err(crate::mem::DeclineCause::Contested { member: Some(dead) })
    );

    assert_eq!(
        directory.recover(&heap, dead),
        Ok(crate::dir::Recovered::Quarantined)
    );
    assert_eq!(
        heap.allocate(core::alloc::Layout::new::<u64>()),
        Err(crate::mem::DeclineCause::Poisoned)
    );
    assert!(heap.finish_recovery(dead));
    assert!(heap.finish_recovery(dead));
}

#[test]
fn directory_resumes_a_rolled_back_create_once_when_two_threads_race_it() {
    use crate::header::{Layout, Status};
    use crate::schema::{LayoutContext, SchemaKey, SharedSchema, schema_id};

    #[derive(Debug)]
    struct Resumed(u32);

    impl SharedSchema for Resumed {
        const SCHEMA: SchemaKey = SchemaKey::new(schema_id("evering.test.resumed"), 1);
    }

    unsafe impl Layout for Resumed {
        type Config = u32;
        type Info = ();

        const MAGIC: crate::header::Magic = 0x611;

        fn info(_: &Self::Config, _: LayoutContext) {}

        unsafe fn init(destination: *mut Self, value: Self::Config) -> Status {
            unsafe { destination.write(Self(value)) };
            Status::Initialized
        }

        fn attach(&self, value: &Self::Config) -> Status {
            if self.0 == *value {
                Status::Initialized
            } else {
                Status::Corrupted
            }
        }
    }

    let mut page_table = [0; MAX_ADDR];
    let mut area = MockBackend(&mut page_table).shared(0, MAX_ADDR);
    let directory = area.push::<crate::dir::Header>(()).unwrap();
    let reserve = area.reserve::<crate::talc::Header>().unwrap();
    let conf = crate::talc::Config::new(MAX_ADDR).with_bound(reserve.remaining_after());
    let heap = crate::talc::MapTalc::from_handle(reserve.commit(conf).unwrap());
    let meta = heap
        .allocate(core::alloc::Layout::new::<u64>())
        .expect("span the create is rolling back");
    let mine = directory.peer().slot();
    directory.abandon_rolling_back_create_for_test(mine, meta);
    crate::dir::open_resume_window_for_test();

    let loser = {
        let directory = &directory;
        let heap = &heap;
        std::thread::scope(|scope| {
            let parked = scope.spawn(move || directory.create::<Resumed>(heap, 7));
            let mut spins = 0u32;
            while !crate::dir::resume_window_parked_for_test() {
                spins += 1;
                assert!(spins < 5_000_000, "the resumer never reached its window");
                std::thread::yield_now();
            }
            let winner = directory.create::<Resumed>(heap, 8);
            assert!(
                matches!(winner, Err(crate::dir::Error::RolledBack)),
                "the resumer that finishes the rollback says so, got {winner:?}"
            );
            crate::dir::close_resume_window_for_test();
            parked
                .join()
                .expect("the second resumer must not touch a span the first already freed")
        })
    };
    assert!(
        matches!(loser, Err(crate::dir::Error::Stale)),
        "a rollback somebody else already finished is stale, not a second rollback: got {loser:?}"
    );

    let first = heap
        .allocate(core::alloc::Layout::new::<u64>())
        .expect("heap still admits after the refused resume");
    let second = heap
        .allocate(core::alloc::Layout::new::<u64>())
        .expect("heap still admits twice");
    assert_ne!(
        heap.pointer(&first).as_ptr(),
        heap.pointer(&second).as_ptr(),
        "the rolled-back span reached the free list twice"
    );
}

#[test]
fn recovery_refuses_a_second_reaper_of_a_claimed_remove_before_it_reads_the_span() {
    use crate::header::{Layout, Status};
    use crate::schema::{LayoutContext, SchemaKey, SharedSchema, schema_id};

    struct Reaped(u32);

    impl SharedSchema for Reaped {
        const SCHEMA: SchemaKey = SchemaKey::new(schema_id("evering.test.reaped"), 1);
    }

    unsafe impl Layout for Reaped {
        type Config = u32;
        type Info = ();

        const MAGIC: crate::header::Magic = 0xD26;

        fn info(_: &Self::Config, _: LayoutContext) {}

        unsafe fn init(destination: *mut Self, value: Self::Config) -> Status {
            unsafe { destination.write(Self(value)) };
            Status::Initialized
        }

        fn attach(&self, value: &Self::Config) -> Status {
            if self.0 == *value {
                Status::Initialized
            } else {
                Status::Corrupted
            }
        }
    }

    let mut page_table = [0; MAX_ADDR];
    let mut area = MockBackend(&mut page_table).shared(0, MAX_ADDR);
    let directory = area.push::<crate::dir::Header>(()).unwrap();
    let reserve = area.reserve::<crate::talc::Header>().unwrap();
    let conf = crate::talc::Config::new(MAX_ADDR).with_bound(reserve.remaining_after());
    let heap = crate::talc::MapTalc::from_handle(reserve.commit(conf).unwrap());
    let layout = core::alloc::Layout::new::<crate::header::RcHeader<Reaped>>();
    // The reap windows are process-wide and a reap from another test runs in parallel with this one, so
    // they are keyed on a slot no other reap here uses.
    let dead = directory.peer().slot().wrapping_add(9);
    let (id, mapped) = directory.create::<Reaped>(&heap, 96).unwrap();
    directory.abandon_prepared_remove_for_test(&heap, id, mapped, layout, dead, false);
    let mine = directory.peer().slot();
    crate::dir::open_reap_windows_for_test(dead);

    let (loser, winner) = {
        let directory = &directory;
        let heap = &heap;
        std::thread::scope(|scope| {
            // Both threads reap the same dead participant. The first stops between the entry gates and
            // the claim, so the second is the one that takes the claim and reads the staged span.
            let blocked = scope.spawn(move || directory.recover(heap, dead));
            let mut spins = 0u32;
            while !crate::dir::reap_gate_parked_for_test() {
                spins += 1;
                assert!(
                    spins < 5_000_000,
                    "the first reaper never reached the claim"
                );
                std::thread::yield_now();
            }
            let holder = scope.spawn(move || directory.recover(heap, dead));
            let mut spins = 0u32;
            while !crate::dir::reap_held_parked_for_test() {
                spins += 1;
                assert!(
                    spins < 5_000_000,
                    "the second reaper never claimed and read the span"
                );
                std::thread::yield_now();
            }
            assert_eq!(
                crate::dir::take_reap_reads_for_test(),
                1,
                "the reaper holding the claim reads the staged cell once"
            );
            crate::dir::release_reap_gate_for_test();
            let refused = blocked
                .join()
                .expect("a reaper that lost the claim must not touch the staged span");
            let reads = crate::dir::take_reap_reads_for_test();
            crate::dir::release_reap_held_for_test();
            let finished = holder
                .join()
                .expect("the reaper holding the claim finishes what it read");
            assert_eq!(
                reads, 0,
                "the refused reaper read the staged cell, so it held a span it never saw published"
            );
            (refused, finished)
        })
    };
    assert_eq!(
        loser,
        Err(crate::dir::Error::Busy(mine)),
        "the claim is the only door to the cell, so the second reaper is refused by it"
    );
    assert_eq!(
        winner,
        Ok(crate::dir::Recovered::RolledBack),
        "the span was still open, so the reap that held the claim publishes the member back"
    );
    assert_eq!(
        directory.recover(&heap, dead),
        Err(crate::dir::Error::Stale),
        "a refused reap leaves the transaction for the one that holds it, and once it is done nothing is left"
    );
    assert_eq!(
        directory.open(id, 96).unwrap().inner.0,
        96,
        "the member the refused reap never read is still the one the winning reap published"
    );
}

#[test]
fn directory_recovery_rejects_authority_from_another_region() {
    let mut page_table = [0; MAX_ADDR];
    let mut area = MockBackend(&mut page_table).shared(0, MAX_ADDR);
    let directory = area.push::<crate::dir::Header>(()).unwrap();
    let reserve = area.reserve::<crate::talc::Header>().unwrap();
    let conf = crate::talc::Config::new(MAX_ADDR).with_bound(reserve.remaining_after());
    let heap = crate::talc::MapTalc::from_handle(reserve.commit(conf).unwrap());
    let dead = directory.peer().slot().wrapping_add(1);
    directory.abandon_held_reservation_for_test(dead);

    let mut other_page_table = [0; MAX_ADDR];
    let mut other = MockBackend(&mut other_page_table).shared(1, MAX_ADDR - 1);
    let other_directory = other.push::<crate::dir::Header>(()).unwrap();
    assert_eq!(
        other_directory.recover(&heap, dead),
        Err(crate::dir::Error::Stale)
    );

    assert_eq!(
        directory.recover(&heap, dead),
        Ok(crate::dir::Recovered::RolledBack)
    );
}

#[test]
fn directory_grows_past_its_first_slab_and_reopens_every_layout() {
    use crate::header::{Layout, Status};
    use crate::schema::{LayoutContext, SchemaKey, SharedSchema, schema_id};

    struct Managed(u32);

    impl SharedSchema for Managed {
        const SCHEMA: SchemaKey = SchemaKey::new(schema_id("evering.test.grown"), 1);
    }

    unsafe impl Layout for Managed {
        type Config = u32;
        type Info = ();

        const MAGIC: crate::header::Magic = 0x610;

        fn info(_: &Self::Config, _: LayoutContext) {}

        unsafe fn init(destination: *mut Self, value: Self::Config) -> Status {
            unsafe { destination.write(Self(value)) };
            Status::Initialized
        }

        fn attach(&self, value: &Self::Config) -> Status {
            if self.0 == *value {
                Status::Initialized
            } else {
                Status::Corrupted
            }
        }
    }

    let mut page_table = [0; MAX_ADDR];
    let mut area = MockBackend(&mut page_table).shared(0, MAX_ADDR);
    let directory = area.push::<crate::dir::Header>(()).unwrap();
    let reserve = area.reserve::<crate::talc::Header>().unwrap();
    let conf = crate::talc::Config::new(MAX_ADDR).with_bound(reserve.remaining_after());
    let heap = crate::talc::MapTalc::from_handle(reserve.commit(conf).unwrap());
    let mut ids = Vec::new();

    for value in 0..65 {
        let (id, mapped) = directory.create::<Managed>(&heap, value).unwrap();
        assert_eq!(mapped.inner.0, value);
        ids.push(id);
    }
    let last = ids[64];
    for (value, id) in ids.into_iter().enumerate() {
        assert_eq!(
            directory.open(id, value as u32).unwrap().inner.0,
            value as u32
        );
    }
    let mapped = directory.open(last, 64).unwrap();
    if let Err((error, _)) = directory.remove_in(
        &heap,
        last,
        mapped,
        core::alloc::Layout::new::<crate::header::RcHeader<Managed>>(),
    ) {
        panic!("remove failed: {error:?}");
    }
    assert!(matches!(
        directory.open(last, 64),
        Err(crate::dir::Error::Stale)
    ));
}

#[test]
fn directory_growth_recovery_rolls_back_unlinked_and_retains_linked_slabs() {
    let mut page_table = [0; MAX_ADDR];
    let mut area = MockBackend(&mut page_table).shared(0, MAX_ADDR);
    let directory = area.push::<crate::dir::Header>(()).unwrap();
    let reserve = area.reserve::<crate::talc::Header>().unwrap();
    let conf = crate::talc::Config::new(MAX_ADDR).with_bound(reserve.remaining_after());
    let heap = crate::talc::MapTalc::from_handle(reserve.commit(conf).unwrap());
    let dead = directory.peer().slot().wrapping_add(1);

    directory.abandon_growth_for_test(&heap, dead, false, true);
    assert_eq!(
        directory.recover(&heap, dead),
        Ok(crate::dir::Recovered::RolledBack)
    );
    assert_eq!(directory.slab_count_for_test(), 1);

    directory.abandon_growth_for_test(&heap, dead, true, true);
    assert_eq!(
        directory.recover(&heap, dead),
        Ok(crate::dir::Recovered::Retained)
    );
    assert_eq!(directory.slab_count_for_test(), 2);
}

#[test]
fn directory_growth_with_an_ambiguous_dead_heap_owner_is_leaked_and_poisoned() {
    let mut page_table = [0; MAX_ADDR];
    let mut area = MockBackend(&mut page_table).shared(0, MAX_ADDR);
    let directory = area.push::<crate::dir::Header>(()).unwrap();
    let reserve = area.reserve::<crate::talc::Header>().unwrap();
    let conf = crate::talc::Config::new(MAX_ADDR).with_bound(reserve.remaining_after());
    let heap = crate::talc::MapTalc::from_handle(reserve.commit(conf).unwrap());
    let dead = directory.peer().slot().wrapping_add(1);

    directory.abandon_growth_for_test(&heap, dead, false, false);
    assert_eq!(
        directory.recover(&heap, dead),
        Ok(crate::dir::Recovered::Quarantined)
    );
    assert_eq!(directory.slab_count_for_test(), 1);
    assert_eq!(
        heap.allocate(core::alloc::Layout::new::<u64>()),
        Err(crate::mem::DeclineCause::Poisoned)
    );
}

#[test]
fn dead_heap_owner_before_growth_evidence_poison_is_not_mistaken_for_rollback() {
    let mut page_table = [0; MAX_ADDR];
    let mut area = MockBackend(&mut page_table).shared(0, MAX_ADDR);
    let directory = area.push::<crate::dir::Header>(()).unwrap();
    let reserve = area.reserve::<crate::talc::Header>().unwrap();
    let conf = crate::talc::Config::new(MAX_ADDR).with_bound(reserve.remaining_after());
    let heap = crate::talc::MapTalc::from_handle(reserve.commit(conf).unwrap());
    let dead = directory.peer().slot().wrapping_add(1);

    directory.abandon_held_heap_for_test(&heap, dead);
    assert_eq!(
        directory.recover(&heap, dead),
        Ok(crate::dir::Recovered::Quarantined)
    );
    assert_eq!(
        heap.allocate(core::alloc::Layout::new::<u64>()),
        Err(crate::mem::DeclineCause::Poisoned)
    );
}

#[test]
fn region_close_unmaps_once() {
    let mut pt = [0; MAX_ADDR];
    let view = mock_view(&mut pt, 0, 0x2000);

    assert_eq!(view.close(), Ok(()));
    assert!(pt[..0x2000].iter().all(|byte| *byte == 0));
}

#[test]
fn failed_root_admission_releases_the_map() {
    let mut bytes = [0xaa; 1];
    let pointer = NonNull::from(&mut bytes[0]);
    let map =
        unsafe { Map::from_raw_parts(pointer, bytes.len(), Access::READ | Access::WRITE, release) };

    assert!(matches!(
        Build::new(
            map,
            crate::schema::RegionAdmission::Create(crate::schema::RegionId::new(1, 1))
        ),
        Err(crate::mem::Error::UnenoughSpace { .. })
    ));
    assert_eq!(bytes, [0]);
}

#[test]
fn a_read_only_view_answers_the_two_questions_the_entrance_asks_first() {
    use crate::mem::{MemOps, read_root};
    use crate::schema::RegionAdmission;

    #[repr(align(64))]
    struct Storage([u8; 0x2000]);

    let mut storage = Storage([0; 0x2000]);
    let writer = mock_view(&mut storage.0, 0, 0x2000);
    // The reader borrows the extent rather than owning it, so it declares nothing to give back; the
    // same base address and residue the writer's map was handed is what this backend gives everyone.
    let reader = unsafe {
        Map::from_static(
            NonNull::new(writer.start_ptr().cast_mut()).unwrap(),
            writer.size(),
            Access::READ,
        )
    };

    match read_root(&reader) {
        Ok(region) => assert_eq!(
            region,
            writer.region_id(),
            "a mapping that may not write still learns which region this root claims"
        ),
        Err(error) => panic!("the read-only view was refused: {error:?}"),
    }

    let refused = Build::new(reader, RegionAdmission::Discover)
        .err()
        .expect("a read-only mapping is admitted nowhere");
    assert!(
        matches!(refused, crate::mem::Error::PermissionDenied { requested }
            if requested == Access::WRITE),
        "the entrance refused for a different reason: {refused:?}"
    );

    // The answer above is read, not assumed: nothing is published in a page nobody wrote to.
    let mut blank = [0u8; 0x2000];
    let unpublished = unsafe {
        Map::from_static(
            NonNull::new(core::ptr::addr_of_mut!(blank))
                .unwrap()
                .cast::<u8>(),
            blank.len(),
            Access::READ,
        )
    };
    let unwritten = read_root(&unpublished).err();
    assert!(
        matches!(unwritten, Some(crate::mem::Error::InvalidHeader)),
        "a root that published nothing was read as one: {unwritten:?}"
    );
}

#[test]
fn a_reader_with_no_slot_answers_what_a_published_record_claims_and_writes_nothing() {
    use crate::header::{Layout, RootHeader, Status};
    use crate::mem::{MemOps, Observer};
    use crate::schema::{LayoutContext, SchemaKey, SharedSchema, schema_id};

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Claimed(u32);

    impl SharedSchema for Claimed {
        const SCHEMA: SchemaKey = SchemaKey::new(schema_id("evering.test.claimed"), 1);
    }

    unsafe impl crate::schema::LayoutInfo for Claimed {}

    #[repr(C)]
    struct Counted(u32);

    impl SharedSchema for Counted {
        const SCHEMA: SchemaKey = SchemaKey::new(schema_id("evering.test.counted"), 1);
    }

    unsafe impl Layout for Counted {
        type Config = u32;
        type Info = Claimed;

        const MAGIC: crate::header::Magic = 0xC04;

        fn info(conf: &Self::Config, _: LayoutContext) -> Claimed {
            Claimed(*conf)
        }

        unsafe fn init(destination: *mut Self, value: Self::Config) -> Status {
            unsafe { destination.write(Self(value)) };
            Status::Initialized
        }

        fn attach(&self, value: &Self::Config) -> Status {
            if self.0 == *value {
                Status::Initialized
            } else {
                Status::Corrupted
            }
        }
    }

    #[repr(align(64))]
    struct Storage([u8; 0x2000]);
    const HEAD: usize = core::mem::size_of::<RootHeader>();

    let mut storage = Storage([0; 0x2000]);
    let mut writer = mock_view(&mut storage.0, 0, 0x2000);
    let record = writer
        .push::<crate::header::RcHeader<Counted>>(7)
        .expect("a counted record fits the fixture extent");
    let at = record.layout_id().offset as usize;
    let before = {
        let mut bytes = [0u8; HEAD];
        bytes.copy_from_slice(unsafe { core::slice::from_raw_parts(writer.start_ptr(), HEAD) });
        bytes
    };

    let observer = Observer::discover(
        Borrowed {
            start: storage.0.as_mut_ptr(),
            len: 0x2000,
        },
        Request::new(0x2000, Access::READ),
    )
    .expect("a reader that claims no slot was refused");

    assert_eq!(
        observer.region(),
        writer.region_id(),
        "the standpoint names a different region than the root it read claims"
    );
    assert_eq!(
        observer.access(),
        Access::READ,
        "the standpoint reports an access it was not granted"
    );
    let read = observer.read::<Counted>(at);
    assert!(
        matches!(read, Ok(Claimed(7))),
        "the reader was answered with something other than what the record published: {read:?}"
    );
    let elsewhere = observer.read::<Counted>(at + 0x800);
    assert!(
        matches!(
            elsewhere,
            Err(crate::mem::Error::LayoutMismatch(
                crate::header::LayoutField::State
            ))
        ),
        "bytes that publish no record were read as one: {elsewhere:?}"
    );

    // The promise of the entrance is checked where it could have been broken: the root header is the
    // one place a member writes on its way in, and a reader that reached it would show here.
    let after = {
        let mut bytes = [0u8; HEAD];
        bytes.copy_from_slice(unsafe { core::slice::from_raw_parts(writer.start_ptr(), HEAD) });
        bytes
    };
    assert_eq!(
        before, after,
        "the read-only entrance left the root header's bytes changed"
    );
}

#[test]
fn a_full_roster_turns_a_member_away_and_answers_a_reader() {
    use crate::header::PARTICIPANT_CAPACITY;
    use crate::mem::Observer;
    use crate::schema::{RegionAdmission, RegionId};

    #[repr(align(64))]
    struct Storage([u8; 0x2000]);

    let mut storage = Storage([0; 0x2000]);
    let extent = Borrowed {
        start: storage.0.as_mut_ptr(),
        len: 0x2000,
    };
    let request = Request::new(0x2000, Access::READ | Access::WRITE);
    let region = RegionId::new(9, 9);
    let mut members = Vec::new();
    for slot in 0..PARTICIPANT_CAPACITY {
        let admission = if slot == 0 {
            RegionAdmission::Create(region)
        } else {
            RegionAdmission::Expect(region)
        };
        let map = extent.map(request).unwrap();
        members
            .push(Build::new(map, admission).expect("a slot is free while the roster is not full"));
    }
    assert_eq!(members.len(), PARTICIPANT_CAPACITY);

    let exhausted = Build::new(
        extent.map(request).unwrap(),
        RegionAdmission::Expect(region),
    )
    .err();
    assert!(
        matches!(
            exhausted,
            Some(crate::mem::Error::NoRoom {
                waits: crate::RoomWait::Grow
            })
        ),
        "the roster filled without exhausting: {exhausted:?}"
    );

    // The same extent, the same declaration of what may be done with it, and no seat: reading a region
    // is not a request to be a member of it, and this is the refusal that used to be the only answer.
    let observer = Observer::discover(extent, Request::new(0x2000, Access::READ))
        .expect("a full roster refused a reader as well as a member");
    assert_eq!(
        observer.region(),
        region,
        "the reader named a region the exhausted roster does not claim"
    );
    let published = observer.read::<crate::header::Root>(0);
    assert!(
        matches!(published, Ok(())),
        "the root that turned a member away was not readable: {published:?}"
    );
}

#[test]
fn failed_session_composition_detaches_and_releases_once() {
    #[repr(align(64))]
    struct Storage([u8; 4096]);

    RELEASES.store(0, Ordering::Relaxed);
    let mut storage = Storage([0; 4096]);
    let len = core::mem::size_of::<crate::header::RootHeader>()
        + core::mem::align_of::<crate::dir::Header>()
        + core::mem::size_of::<crate::dir::Header>();
    let pointer = NonNull::new(storage.0.as_mut_ptr()).unwrap();
    let map =
        unsafe { Map::from_raw_parts(pointer, len, Access::READ | Access::WRITE, count_release) };
    let layout = Build::new(
        map,
        crate::schema::RegionAdmission::Create(crate::schema::RegionId::new(2, 2)),
    )
    .expect("root fits");

    assert!(crate::Session::from(layout).is_err());
    assert_eq!(RELEASES.load(Ordering::Relaxed), 1);
}

#[test]
fn mapped_drop_releases_its_layout_membership() {
    use crate::header::{AdmitError, AdmitLayout, RcHeader, Root};

    let mut pt = [0; MAX_ADDR];
    let mut layout = MockBackend(&mut pt).shared(0, 0x4000);
    let mapped = layout.push::<RcHeader<Root>>(()).unwrap();
    let pointer = core::ptr::NonNull::from(&*mapped).as_ptr();
    let slot = mapped.peer().slot();
    let ctx = crate::schema::LayoutContext {
        region: mapped.region_id(),
        offset: mapped.layout_id().offset,
        allow_init: false,
    };

    assert_eq!(
        unsafe { <RcHeader<Root> as AdmitLayout>::admit(pointer, (), ctx, Some(slot)) },
        Err(AdmitError::DuplicateMember)
    );
    drop(mapped);
    assert_eq!(
        unsafe { <RcHeader<Root> as AdmitLayout>::admit(pointer, (), ctx, Some(slot)) },
        Ok(())
    );
    unsafe { <RcHeader<Root> as AdmitLayout>::leave(pointer, slot) };
}
