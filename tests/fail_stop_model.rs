//! Deterministic state-model oracles for fail-stop shared-memory protocols.
//!
//! These tests deliberately contain no production implementation. They freeze
//! schedules and ownership outcomes before the corresponding owners are
//! replaced.

#[derive(Debug, PartialEq, Eq)]
struct Directory<const N: usize> {
    owner: Option<u8>,
    live: [bool; N],
}

impl<const N: usize> Directory<N> {
    fn claim(&mut self, owner: u8) -> bool {
        if self.owner.is_some() {
            return false;
        }
        self.owner = Some(owner);
        true
    }

    fn allocate(&mut self, owner: u8) -> Option<usize> {
        (self.owner == Some(owner))
            .then(|| self.live.iter().position(|live| !live))
            .flatten()
            .inspect(|index| self.live[*index] = true)
    }

    fn commit(&mut self, owner: u8) -> bool {
        if self.owner != Some(owner) {
            return false;
        }
        self.owner = None;
        true
    }
}

#[test]
fn one_bounded_directory_transaction_excludes_the_aba_schedule() {
    let mut directory = Directory {
        owner: None,
        live: [false; 3],
    };

    assert!(directory.claim(1));
    assert!(!directory.claim(2), "contender returns without mutation");
    assert_eq!(directory.allocate(2), None);
    assert_eq!(directory.allocate(1), Some(0));
    assert!(directory.commit(1));

    assert!(directory.claim(2));
    assert_eq!(directory.allocate(2), Some(1));
    assert!(directory.commit(2));
    assert_eq!(directory.live, [true, true, false]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Owner {
    slot: u8,
    generation: u16,
}

const SOURCE_OWNER: Owner = Owner {
    slot: 3,
    generation: 7,
};
const REAPER_OWNER: Owner = Owner {
    slot: 5,
    generation: 2,
};

const EMPTY: u16 = 0;
const PRODUCER: u16 = 1;
const AVAILABLE: u16 = 2;
const CONSUMER: u16 = 3;
const BASE_MASK: u16 = 0b11;
const COMPLETE: u16 = 1 << 2;
const SOURCE_SHIFT: u32 = 3;
const SOURCE_MASK: u16 = 0x3f << SOURCE_SHIFT;
const REAPER_SHIFT: u32 = 9;
const REAPER_MASK: u16 = 0x3f << REAPER_SHIFT;
const RESERVED: u16 = 1 << 15;
const PARTICIPANTS: u16 = usize::BITS as u16 - 1;

fn canonical(control: u16) -> bool {
    if control & RESERVED != 0 {
        return false;
    }
    let source = (control & SOURCE_MASK) >> SOURCE_SHIFT;
    let reaper = (control & REAPER_MASK) >> REAPER_SHIFT;
    match control & BASE_MASK {
        EMPTY => control == EMPTY,
        PRODUCER | CONSUMER => source != 0 && source <= PARTICIPANTS && reaper <= PARTICIPANTS,
        AVAILABLE => source == 0 && reaper == 0,
        _ => false,
    }
}

fn owned(base: u16, complete: bool, owner: u8) -> u16 {
    base | (u16::from(complete) * COMPLETE) | ((owner as u16 + 1) << SOURCE_SHIFT)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueueRecovery {
    None,
    Busy(u8),
    Corrupted,
    Recovered { control: u16, turn: usize },
}

fn recover_queue(
    control: u16,
    dead: u8,
    live: u8,
    turn: usize,
    cursor: usize,
    one_lap: usize,
) -> QueueRecovery {
    if dead as u16 >= PARTICIPANTS || live as u16 >= PARTICIPANTS || !canonical(control) {
        return QueueRecovery::Corrupted;
    }
    if !matches!(control & BASE_MASK, PRODUCER | CONSUMER) {
        return QueueRecovery::None;
    }
    let source = ((control & SOURCE_MASK) >> SOURCE_SHIFT) as u8;
    let reaper = ((control & REAPER_MASK) >> REAPER_SHIFT) as u8;
    let claimed = (source == dead + 1 && reaper == live + 1)
        || reaper == dead + 1
        || (source == dead + 1 && reaper == 0);
    if !claimed {
        return if source == dead + 1 {
            QueueRecovery::Busy(reaper - 1)
        } else {
            QueueRecovery::None
        };
    }
    let complete = control & COMPLETE;
    match control & BASE_MASK {
        PRODUCER => {
            let distance = cursor.wrapping_sub(turn);
            if distance == 0 && complete == 0 {
                QueueRecovery::Recovered {
                    control: EMPTY,
                    turn,
                }
            } else if distance != 0 && distance < 1usize << (usize::BITS - 1) {
                QueueRecovery::Recovered {
                    control: AVAILABLE | complete,
                    turn,
                }
            } else {
                QueueRecovery::Corrupted
            }
        }
        CONSUMER if cursor == turn => QueueRecovery::Recovered {
            control: AVAILABLE | complete,
            turn,
        },
        CONSUMER => {
            let distance = cursor.wrapping_sub(turn);
            QueueRecovery::Recovered {
                control: EMPTY,
                turn: if distance < 1usize << (usize::BITS - 1) {
                    turn.wrapping_add(one_lap)
                } else {
                    turn
                },
            }
        }
        _ => unreachable!(),
    }
}

#[test]
fn every_canonical_queue_word_has_one_packed_meaning() {
    let count = (0..=u16::MAX).filter(|word| canonical(*word)).count();
    assert_eq!(
        count,
        3 + 4 * PARTICIPANTS as usize * (PARTICIPANTS as usize + 1)
    );
}

#[test]
fn producer_recovery_is_decided_only_by_the_tail_commit() {
    let reserved = owned(PRODUCER, false, 3);
    let staged = owned(PRODUCER, true, 3);
    assert_eq!(
        recover_queue(reserved, 3, 5, 8, 8, 4),
        QueueRecovery::Recovered {
            control: EMPTY,
            turn: 8
        }
    );
    assert_eq!(
        recover_queue(staged, 3, 5, 8, 12, 4),
        QueueRecovery::Recovered {
            control: AVAILABLE | COMPLETE,
            turn: 8
        }
    );
    assert_eq!(
        recover_queue(staged, 3, 5, 8, 8, 4),
        QueueRecovery::Corrupted
    );
}

#[test]
fn consumer_recovery_never_advances_a_turn_twice() {
    let claimed = owned(CONSUMER, true, 3);
    assert_eq!(
        recover_queue(claimed, 3, 5, 8, 12, 4),
        QueueRecovery::Recovered {
            control: EMPTY,
            turn: 12
        }
    );
    assert_eq!(
        recover_queue(claimed, 3, 5, 12, 12, 4),
        QueueRecovery::Recovered {
            control: AVAILABLE | COMPLETE,
            turn: 12
        }
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OrderingContract {
    publish_release: bool,
    claim_acquire: bool,
    recycle_release: bool,
    reserve_acquire: bool,
}

impl OrderingContract {
    const fn payload_visible(self) -> bool {
        self.publish_release && self.claim_acquire
    }

    const fn overwrite_after_recycle(self) -> bool {
        self.recycle_release && self.reserve_acquire
    }
}

#[test]
fn queue_visibility_rejects_each_weakened_happens_before_edge() {
    let required = OrderingContract {
        publish_release: true,
        claim_acquire: true,
        recycle_release: true,
        reserve_acquire: true,
    };
    assert!(required.payload_visible());
    assert!(required.overwrite_after_recycle());

    for weakened in [
        OrderingContract {
            publish_release: false,
            ..required
        },
        OrderingContract {
            claim_acquire: false,
            ..required
        },
    ] {
        assert!(
            !weakened.payload_visible(),
            "unpublished payload became readable"
        );
    }
    for weakened in [
        OrderingContract {
            recycle_release: false,
            ..required
        },
        OrderingContract {
            reserve_acquire: false,
            ..required
        },
    ] {
        assert!(
            !weakened.overwrite_after_recycle(),
            "producer overwrote storage before consumer completion"
        );
    }
}

#[test]
fn a_dead_reaper_can_be_replaced_without_changing_source_evidence() {
    let control = owned(PRODUCER, true, 3) | ((REAPER_OWNER.slot as u16 + 1) << REAPER_SHIFT);
    assert_eq!(
        recover_queue(control, REAPER_OWNER.slot, 6, 8, 12, 4),
        QueueRecovery::Recovered {
            control: AVAILABLE | COMPLETE,
            turn: 8
        }
    );
    assert_eq!(
        recover_queue(control, SOURCE_OWNER.slot, 6, 8, 12, 4),
        QueueRecovery::Busy(REAPER_OWNER.slot)
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PoolAuthority {
    Free(u16),
    Local(u16, u8),
    Detached(u16),
}

fn reap_pool(authority: &mut PoolAuthority, dead: u8) {
    if let PoolAuthority::Local(generation, owner) = *authority
        && owner == dead
    {
        *authority = PoolAuthority::Free(generation);
    }
}

fn release_transfer(authority: &mut PoolAuthority, generation: u16, source: u8) -> bool {
    let current = match *authority {
        PoolAuthority::Free(current)
        | PoolAuthority::Local(current, _)
        | PoolAuthority::Detached(current) => current,
    };
    if generation == 0 || current < generation {
        return false;
    }
    if current > generation || matches!(*authority, PoolAuthority::Free(_)) {
        return true;
    }
    if matches!(*authority, PoolAuthority::Local(_, owner) if owner != source) {
        return false;
    }
    *authority = PoolAuthority::Free(generation);
    true
}

#[test]
fn pool_and_queue_scan_order_have_the_same_release_result() {
    for detached in [false, true] {
        let initial = if detached {
            PoolAuthority::Detached(7)
        } else {
            PoolAuthority::Local(7, SOURCE_OWNER.slot)
        };
        let mut queue_first = initial;
        assert!(release_transfer(&mut queue_first, 7, SOURCE_OWNER.slot));
        reap_pool(&mut queue_first, SOURCE_OWNER.slot);

        let mut pool_first = initial;
        reap_pool(&mut pool_first, SOURCE_OWNER.slot);
        assert!(release_transfer(&mut pool_first, 7, SOURCE_OWNER.slot));
        assert_eq!(queue_first, PoolAuthority::Free(7));
        assert_eq!(pool_first, queue_first);
    }
}

#[test]
fn replacement_reaper_uses_original_source_and_stale_generation_is_harmless() {
    let mut interrupted = PoolAuthority::Local(7, SOURCE_OWNER.slot);
    assert!(release_transfer(&mut interrupted, 7, SOURCE_OWNER.slot));
    assert_eq!(interrupted, PoolAuthority::Free(7));

    let mut reused = PoolAuthority::Local(8, SOURCE_OWNER.slot);
    assert!(release_transfer(&mut reused, 7, SOURCE_OWNER.slot));
    assert_eq!(reused, PoolAuthority::Local(8, SOURCE_OWNER.slot));
    assert!(!release_transfer(&mut reused, 0, SOURCE_OWNER.slot));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeapMutation {
    Clean,
    Mutating(Owner),
    Poisoned,
}

fn recover_heap(state: HeapMutation, dead: Owner) -> HeapMutation {
    match state {
        HeapMutation::Mutating(owner) if owner == dead => HeapMutation::Poisoned,
        other => other,
    }
}

#[test]
fn exact_allocator_owner_death_poison_is_fail_stop() {
    assert_eq!(
        recover_heap(HeapMutation::Mutating(SOURCE_OWNER), SOURCE_OWNER),
        HeapMutation::Poisoned
    );
    assert_eq!(
        recover_heap(HeapMutation::Mutating(SOURCE_OWNER), REAPER_OWNER),
        HeapMutation::Mutating(SOURCE_OWNER)
    );
    assert_eq!(
        recover_heap(HeapMutation::Clean, SOURCE_OWNER),
        HeapMutation::Clean
    );
    assert_eq!(
        recover_heap(HeapMutation::Poisoned, SOURCE_OWNER),
        HeapMutation::Poisoned
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManagedCreate {
    Allocated,
    Pending,
}

fn may_commit_managed_heap(phase: ManagedCreate) -> bool {
    matches!(phase, ManagedCreate::Pending)
}

#[test]
fn managed_heap_cannot_become_clean_before_pending_evidence() {
    assert!(!may_commit_managed_heap(ManagedCreate::Allocated));
    assert!(may_commit_managed_heap(ManagedCreate::Pending));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManagedRemove {
    Deallocated,
    Released,
}

fn may_commit_removed_heap(phase: ManagedRemove) -> bool {
    matches!(phase, ManagedRemove::Released)
}

#[test]
fn removed_heap_cannot_become_clean_before_released_is_durable() {
    assert!(!may_commit_removed_heap(ManagedRemove::Deallocated));
    assert!(may_commit_removed_heap(ManagedRemove::Released));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeadRemove {
    Removing,
    Released,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReapRemove {
    Deallocate,
    Vacate,
}

fn reap_clean_remove(phase: DeadRemove) -> ReapRemove {
    match phase {
        DeadRemove::Removing => ReapRemove::Deallocate,
        DeadRemove::Released => ReapRemove::Vacate,
    }
}

#[test]
fn released_recovery_never_repeats_deallocation() {
    assert_eq!(
        reap_clean_remove(DeadRemove::Removing),
        ReapRemove::Deallocate
    );
    assert_eq!(reap_clean_remove(DeadRemove::Released), ReapRemove::Vacate);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeadGrowth {
    Unlinked,
    Linked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReapGrowth {
    Deallocate,
    Retain,
}

fn reap_clean_growth(state: DeadGrowth) -> ReapGrowth {
    match state {
        DeadGrowth::Unlinked => ReapGrowth::Deallocate,
        DeadGrowth::Linked => ReapGrowth::Retain,
    }
}

#[test]
fn slab_link_publication_decides_rollback_without_guessing_from_bytes() {
    assert_eq!(
        reap_clean_growth(DeadGrowth::Unlinked),
        ReapGrowth::Deallocate
    );
    assert_eq!(reap_clean_growth(DeadGrowth::Linked), ReapGrowth::Retain);
}

#[derive(Clone, Copy)]
struct Replay(u64);

impl Replay {
    fn next(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        value
    }
}

fn replay(seed: u64, steps: usize) -> Result<(), (usize, &'static str)> {
    let mut random = Replay(seed);
    for step in 0..steps {
        let source = (random.next() % PARTICIPANTS as u64) as u8;
        let live = (random.next() % PARTICIPANTS as u64) as u8;
        let base = if random.next() & 1 == 0 {
            PRODUCER
        } else {
            CONSUMER
        };
        let complete = random.next() & 1 != 0;
        let mut control = owned(base, complete, source);
        if random.next().is_multiple_of(3) {
            control |= ((live as u16 + 1) << REAPER_SHIFT) & REAPER_MASK;
        }
        let turn = (random.next() as usize) & !3;
        let cursor = match random.next() % 3 {
            0 => turn,
            1 => turn.wrapping_add(4),
            _ => turn.wrapping_sub(4),
        };
        if let QueueRecovery::Recovered {
            control: recovered,
            turn: recovered_turn,
        } = recover_queue(control, source, live, turn, cursor, 4)
        {
            if !canonical(recovered) {
                return Err((step, "recovery fabricated a noncanonical word"));
            }
            if recovered & COMPLETE != 0 && control & COMPLETE == 0 {
                return Err((step, "recovery fabricated initialized data"));
            }
            if recovered_turn != turn && recovered_turn != turn.wrapping_add(4) {
                return Err((step, "recovery advanced a turn by more than one lap"));
            }
        }
    }
    Ok(())
}

#[test]
fn seeded_replay_reports_a_minimal_reproducible_prefix() {
    const STEPS: usize = 16_384;
    for seed in [
        0x243f_6a88_85a3_08d3,
        0x1319_8a2e_0370_7344,
        0xa409_3822_299f_31d0,
        0x082e_fa98_ec4e_6c89,
    ] {
        if let Err((failed, reason)) = replay(seed, STEPS) {
            let mut low = 1;
            let mut high = failed + 1;
            while low < high {
                let middle = low + (high - low) / 2;
                if replay(seed, middle).is_err() {
                    high = middle;
                } else {
                    low = middle + 1;
                }
            }
            panic!("seed {seed:#018x}, minimal prefix {low}: {reason}");
        }
    }
}
