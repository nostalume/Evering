//! Who belongs to a family and in what order: arm identity, the exact bounded
//! membership a registered family has, the one baseline a schedule shares across
//! policies, and the window a stream may hold in flight. `condition`, `cell` and
//! `contrast` build the schedule the other areas measure against.

use crate::{family, model};

#[test]
fn family_identity_is_explicit_unique_and_closed() {
    let core = family::find("core-ipc").unwrap();
    assert_eq!((core.key, core.revision), ("core-ipc", 3));
    assert_eq!(
        core.mode("screening"),
        Some((std::time::Duration::from_secs(180), 3))
    );
    assert_eq!(core.mode("unknown"), None);
    assert!(family::find("unknown").is_none());
}

#[cfg(unix)]
#[test]
fn local_family_owns_its_exact_two_arm_matrix() {
    let local = family::find("local-ipc-unix").unwrap();
    assert_eq!((local.key, local.revision), ("local-ipc-unix", 3));
    assert_eq!(local.baseline.key, "uds/readiness");
    assert_eq!(
        local.mode("pilot"),
        Some((std::time::Duration::from_secs(45), 1))
    );
    assert_eq!(
        local.mode("screening"),
        Some((std::time::Duration::from_secs(60), 3))
    );
    assert_eq!(
        local.mode("focused"),
        Some((std::time::Duration::from_secs(90), 15))
    );
    for mode in ["screening", "focused"] {
        let members = (local.members)(mode).unwrap();
        assert_eq!(members.len(), 6);
        for payload in [0, 1024, 64 * 1024] {
            let pair: Vec<_> = members
                .iter()
                .filter(|(cell, _)| cell.payload == payload)
                .collect();
            assert_eq!(pair.len(), 2);
            assert!(
                pair.iter()
                    .all(|(cell, _)| (cell.capacity, cell.in_flight) == (8, 8))
            );
            assert!(pair.iter().all(|(cell, _)| cell.memory == model::MEMORY));
        }
    }
}

#[cfg(unix)]
#[test]
fn registered_evering_cells_fit_the_frozen_pool() {
    use crate::evering_arm;
    use ::evering::{
        Session,
        mapping::{Access, Request},
        os::unix::UnixFd,
    };

    let source = UnixFd::memfd("evering-study-pool", model::MEMORY as usize, false).unwrap();
    let session = Session::create(
        source.borrow(),
        Request::new(model::MEMORY as usize, Access::READ | Access::WRITE),
        evering_arm::REGION,
    )
    .unwrap();
    let pool = session
        .create_pool(evering_arm::POOL_EXTENT, Some(evering_arm::pool_range()))
        .unwrap();
    assert_eq!(pool.range(), evering_arm::pool_range());

    for (condition, _) in (family::CORE.members)("screening")
        .unwrap()
        .into_iter()
        .chain((family::LOCAL.members)("screening").unwrap())
        .filter(|(_, arm)| arm.key.starts_with("evering/"))
    {
        let bytes = vec![0_u8; condition.payload as usize];
        let live: Vec<_> = (0..condition.capacity.min(condition.in_flight))
            .map(|_| pool.as_ref().copy(&bytes).unwrap())
            .collect();
        assert_eq!(
            live.len() as u64,
            condition.capacity.min(condition.in_flight)
        );
    }
}

pub(super) fn condition() -> model::Condition {
    model::Condition {
        payload: 64,
        capacity: 8,
        in_flight: 3,
        memory: model::MEMORY,
    }
}

pub(super) fn cell(arm: &'static family::Arm) -> model::Cell {
    condition().cell(arm)
}

pub(super) fn contrast(arm: &'static family::Arm) -> (model::Condition, &'static family::Arm) {
    (condition(), arm)
}

#[test]
fn schedule_shares_one_baseline_across_evering_policies() {
    let members = [
        contrast(&family::BUSY),
        contrast(&family::NOTIFIED),
        (condition(), &family::STREAM),
    ];
    let scheduled = model::schedule(&members, 3, 7);
    for block in 0..3 {
        let arms: Vec<_> = scheduled
            .iter()
            .filter(|trial| trial.block == block)
            .map(|trial| trial.arm.key)
            .collect();
        assert_eq!(arms.len(), 3);
        assert_eq!(
            arms.iter()
                .filter(|arm| **arm == family::STREAM.key)
                .count(),
            1
        );
        assert!(arms.contains(&family::BUSY.key));
        assert!(arms.contains(&family::NOTIFIED.key));
    }
}

#[test]
fn registered_families_have_exact_bounded_membership() {
    let screening = (family::CORE.members)("screening").unwrap();
    let focused = (family::CORE.members)("focused").unwrap();
    assert_eq!((screening.len(), focused.len()), (36, 10));
    assert_eq!(model::schedule(&screening, 3, 7).len(), 108);
    assert_eq!(model::schedule(&focused, 15, 7).len(), 150);
    assert!(
        screening
            .iter()
            .chain(&focused)
            .all(|(cell, _)| cell.memory == model::MEMORY)
    );
}

#[test]
fn schedule_randomizes_arm_order_without_changing_membership() {
    let members = [contrast(&family::ADAPTIVE), (condition(), &family::STREAM)];
    let first = model::schedule(&members, 8, 7);
    let second = model::schedule(&members, 8, 8);
    assert_eq!(first.len(), second.len());
    assert_ne!(
        first.iter().map(|entry| entry.arm.key).collect::<Vec<_>>(),
        second.iter().map(|entry| entry.arm.key).collect::<Vec<_>>()
    );
}

#[test]
fn stream_window_obeys_capacity_and_in_flight() {
    assert_eq!(model::window(100, 1, 64), 1);
    assert_eq!(model::window(100, 8, 64), 8);
    assert_eq!(model::window(3, 8, 64), 3);
}
