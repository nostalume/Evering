#![cfg(test)]

mod talc;

use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::mem::{Access, Map, MapLayout, MapView};

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
    fn shared(self, start: usize, size: usize) -> MapLayout {
        assert!(
            start
                .checked_add(size)
                .is_some_and(|end| end <= self.0.len())
        );
        let pointer = NonNull::new(unsafe { self.0.as_mut_ptr().add(start) }).unwrap();
        let map =
            unsafe { Map::from_raw_parts(pointer, size, Access::READ | Access::WRITE, release) };
        MapLayout::new(
            map,
            crate::RegionAdmission::Create(crate::RegionId::new(0, start as u64)),
        )
        .unwrap()
    }
}

type MockMapView = MapView;
fn mock_view(bk: &mut [u8], start: usize, size: usize) -> MockMapView {
    let bk = MockBackend(bk);
    bk.shared(start, size).try_into().unwrap()
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

        const MAGIC: crate::LayoutMagic = 0xC17;

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

        const MAGIC: crate::LayoutMagic = 0xBAD;

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
    let recovery = directory.recovery_for_test(dead);
    assert_eq!(
        directory.recover(&heap, &recovery),
        Ok(crate::dir::Recovered::Released)
    );
    directory.abandon_held_reservation_for_test(dead);
    assert_eq!(
        directory.recover(&heap, &recovery),
        Ok(crate::dir::Recovered::RolledBack)
    );

    let layout = core::alloc::Layout::new::<crate::header::RcHeader<Created>>();
    let (before_close, mapped) = directory.create::<Created>(&heap, 93).unwrap();
    directory.abandon_prepared_remove_for_test(&heap, before_close, mapped, layout, dead, false);
    assert_eq!(
        directory.recover(&heap, &recovery),
        Ok(crate::dir::Recovered::RolledBack)
    );
    assert_eq!(directory.open(before_close, 93).unwrap().inner.0, 93);

    let (after_close, mapped) = directory.create::<Created>(&heap, 94).unwrap();
    directory.abandon_prepared_remove_for_test(&heap, after_close, mapped, layout, dead, true);
    assert_eq!(
        directory.recover(&heap, &recovery),
        Ok(crate::dir::Recovered::Released)
    );
    assert!(matches!(
        directory.open(after_close, 94),
        Err(crate::dir::Error::Stale)
    ));

    let (_, replacement) = directory.create::<Created>(&heap, 92).unwrap();
    assert_eq!(replacement.inner.0, 92);
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
    let recovery = directory.recovery_for_test(dead);

    assert_eq!(
        directory.recover(&heap, &recovery),
        Ok(crate::dir::Recovered::Quarantined)
    );
    assert_eq!(
        heap.allocate(core::alloc::Layout::new::<u64>()),
        Err(crate::talc::MutationError::Poisoned)
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
    let wrong = other_directory.recovery_for_test(dead);
    assert_eq!(
        directory.recover(&heap, &wrong),
        Err(crate::dir::Error::Stale)
    );

    let recovery = directory.recovery_for_test(dead);
    assert_eq!(
        directory.recover(&heap, &recovery),
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

        const MAGIC: crate::LayoutMagic = 0x610;

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
    let recovery = directory.recovery_for_test(dead);

    directory.abandon_growth_for_test(&heap, dead, false, true);
    assert_eq!(
        directory.recover(&heap, &recovery),
        Ok(crate::dir::Recovered::RolledBack)
    );
    assert_eq!(directory.slab_count_for_test(), 1);

    directory.abandon_growth_for_test(&heap, dead, true, true);
    assert_eq!(
        directory.recover(&heap, &recovery),
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
    let recovery = directory.recovery_for_test(dead);

    directory.abandon_growth_for_test(&heap, dead, false, false);
    assert_eq!(
        directory.recover(&heap, &recovery),
        Ok(crate::dir::Recovered::Quarantined)
    );
    assert_eq!(directory.slab_count_for_test(), 1);
    assert_eq!(
        heap.allocate(core::alloc::Layout::new::<u64>()),
        Err(crate::talc::MutationError::Poisoned)
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
    let recovery = directory.recovery_for_test(dead);

    directory.abandon_held_heap_for_test(&heap, dead);
    assert_eq!(
        directory.recover(&heap, &recovery),
        Ok(crate::dir::Recovered::Quarantined)
    );
    assert_eq!(
        heap.allocate(core::alloc::Layout::new::<u64>()),
        Err(crate::talc::MutationError::Poisoned)
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
        MapLayout::new(
            map,
            crate::RegionAdmission::Create(crate::RegionId::new(1, 1))
        ),
        Err(crate::MapError::UnenoughSpace { .. })
    ));
    assert_eq!(bytes, [0]);
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
    let layout = MapLayout::new(
        map,
        crate::RegionAdmission::Create(crate::RegionId::new(2, 2)),
    )
    .expect("root fits");

    assert!(crate::perlude::talc::SessionBy::<()>::from(layout).is_err());
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
