//! The waiting a multi-process claim does around one edge: how long a child may take to leave, how long a
//! full queue may stay full, how long an empty one may stay empty, and what a parent says to name a queue
//! to a process that cannot look inside its address space.
//!
//! A deadline copied per file is where two cases stop meaning the same thing by the same word, so these
//! four run from here with the budget as an argument. The argument is why the budget is not a constant
//! either: the claims disagree about it on purpose — a reap that walks a whole directory and a child that
//! only has to exit are not given the same number.
//!
//! What is deliberately not here: `admit`, which exists only where a handover moves a region and whose Unix
//! and Windows arms hand back different things (one keeps the ring that lets a parked child go, one keeps
//! the instance); the six-field `bootstrap` that names a queue with no pool, which exactly one claim reads;
//! and the `child_command`/`spawn` pair that picks which role a re-entered process runs, which is the
//! `role` module's business rather than this one's.

use std::{
    thread,
    time::{Duration, Instant},
};

use evering::{
    Block, Decline, DeclineCause, PoolRef, ReceiveError, Rx,
    layout::{Repr, Shape},
};
#[cfg(feature = "process")]
use evering::{
    PoolId, Port,
    process::{Bootstrap, Supervisor},
};

/// Runs `child` until it leaves and reports the code it left with.
///
/// The wait is bounded from the outside because the roles report through their exit code and a role that
/// cannot agree stays silent: an unbounded wait would report a hung test binary rather than the claim that
/// went unanswered. The child is killed before the panic so the parent's own run is not the next thing to
/// hang. `label` names the role, which is the only part of this a failure can point at.
#[cfg(feature = "process")]
pub fn exit_code(child: &mut Supervisor, label: &str, budget: Duration) -> i32 {
    let until = Instant::now() + budget;
    loop {
        if let Some(exit) = child.try_wait().unwrap() {
            return exit.status().code().unwrap_or(-1);
        }
        if Instant::now() >= until {
            let _ = child.kill_wait();
            panic!("{label} never finished");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Publishes `value` through `send` for as long as the queue may still be draining.
///
/// Only a full or contested queue is retried: a refusal that took the block's authority with it, or that
/// says the peer is gone, is the claim's failure to report, not something to loop on. The deadline is the
/// caller's, because the caller knows what it is waiting the peer to do.
pub fn send_bounded<T>(
    mut value: T,
    mut send: impl FnMut(T) -> Result<(), Decline<T>>,
    deadline: Instant,
) {
    loop {
        let Decline { cause, value: sent } = match send(value) {
            Ok(()) => return,
            Err(decline) => decline,
        };
        match (cause, sent) {
            (DeclineCause::NoRoom { .. } | DeclineCause::Contested { .. }, Some(returned)) => {
                value = returned
            }
            (DeclineCause::Sealed, _) => panic!("peer disconnected while sending"),
            (_, None) => panic!("block lost its Pool authority: {cause:?}"),
            (cause, Some(_)) => panic!("send refused for {cause:?}"),
        }
        assert!(Instant::now() < deadline, "queue remained full");
        thread::yield_now();
    }
}

/// Claims the next message for as long as the queue may still be filling, and adopts its block.
///
/// The pair of [`send_bounded`] at the other end of the same channel, with the same reason for each
/// branch: an empty or busy queue is the peer not having arrived yet, and anything else is the claim's
/// answer.
pub fn adopt_bounded<'p, H, T>(
    recv: &Rx<H>,
    pool: PoolRef<'p>,
    deadline: Instant,
) -> (H, Block<'p, T>)
where
    H: Repr,
    T: Repr + Shape + ?Sized,
{
    loop {
        match recv.claim() {
            Ok(received) => match received.adopt(pool) {
                Ok(value) => return value,
                Err(error) => panic!("matching Pool rejected transfer: {error:?}"),
            },
            Err(ReceiveError::Empty | ReceiveError::Busy) => {}
            Err(ReceiveError::Closed) => panic!("peer disconnected while receiving"),
        }
        assert!(Instant::now() < deadline, "queue remained empty");
        thread::yield_now();
    }
}

/// Names a queue and the pool its blocks live in, in the one string both process legs read.
///
/// Nine numbers, in the order the child's parser expects them, because a handover moves handles rather
/// than identities: the child reconstructs a [`Port`] and a [`PoolId`] from text and the region id it was
/// given separately. A claim whose channel carries no pool says six of the nine, and does so itself.
#[cfg(feature = "process")]
pub fn bootstrap(port: &Port<()>, pool: PoolId) -> Bootstrap {
    let id = port.id();
    let (_, pool_slab, pool_entry, pool_generation) = pool.parts();
    Bootstrap::new(format!(
        "{},{},{},{},{},{},{},{},{}",
        id.slab(),
        id.entry(),
        id.generation(),
        id.capacity(),
        port.role(),
        port.generation(),
        pool_slab,
        pool_entry,
        pool_generation,
    ))
    .unwrap()
}
