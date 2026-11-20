//! Owns no state at all: it authors the commit-then-ring ordering a transfer has to follow, and the
//! withheld-doorbell debt a batch carries when a ring was written before its item was committed. The
//! debt moves only inside `Deferred`'s publish, adopt, admit, deliver and discard and inside
//! `Permit`'s send, publish and cancel, and it is paid — or rung — when the guard drops; retries happen
//! only through `Patience::step` under `reserve_with` and `claim_with`.
//!
//! The default ladder is a `const` here that only `Adaptive::new` reads, while this file's own tests
//! use a separate budget and drive the counter directly: so nothing in this module pins what the
//! default policy costs, or that dropping a guard still rings. Both are settled only by the suites
//! outside it.
use core::{convert::Infallible, future::Future};

use crate::{
    AdoptError, Block, PoolRef, ReceiveError, Received, Rx, Transfer, TransferStaged, TrySendError,
    Tx, channel::PublishError, channel::TransferReserved, layout::Repr, token::Shape,
};

const LOCAL_RETRY_LIMIT: usize = 32;

/// Sends an advisory notification after shared state has committed.
///
/// The `os` adapters report `io::Error` on every platform, so a value returned
/// as `Committed<T, N::Error>` carries one failure type wherever it travels.
///
/// A notification carries no state of its own: the shared region stays the sole
/// authority on what was published, and a wake only says re-examining it may pay.
/// A conformant provider is sticky (a kick that arrives before any waiter exists
/// is not lost), may coalesce without counting (a hundred kicks may act as one),
/// and lets a cancelled wait consume nothing. How many waiters one kick releases
/// is deliberately outside this contract — an `os` backend releases every
/// registered waiter while a seL4 notification dequeues its head thread alone —
/// so a woken agent re-examines its own condition rather than trusting the
/// wake-set. The clauses, and which of them a new adapter is checked against,
/// are in `tests/latch_conformance.rs`.
pub trait Notify {
    type Error;

    fn notify(&self) -> Result<(), Self::Error>;
}

impl Notify for () {
    type Error = Infallible;

    #[inline(always)]
    fn notify(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Observes and consumes sticky advisory readiness in one operation.
///
/// One wake releases at least one waiter and names no cause, so work that has to
/// distinguish causes belongs in separate latches, not in a narrower wake: the
/// adapters hand out one pair per condition, and callers that share a `Wait`
/// across agents must each be able to judge their own progress from the region.
/// That a park left the latch empty is not observable here — an empty latch and a
/// readiness the driver never re-reported look identical from inside a sleep — so
/// a provider proves consumption where it can read the primitive directly.
pub trait Wait {
    type Error;

    fn wait(&self) -> impl Future<Output = Result<(), Self::Error>> + '_;
}

/// Why an authoritative operation did not commit.
///
/// The distinction is not stylistic. [`Contention::Busy`] is resolved by another
/// agent in this process, while [`Contention::Full`] and [`Contention::Empty`]
/// are resolved by the peer. A policy that waits for the peer on a busy
/// operation can sleep through the local progress that would have resolved it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Contention {
    /// Local retry may progress without receiver action. Which reason a taken
    /// slot reports belongs to the Queue: a claim in flight is `Busy` to the
    /// receiver, while a slot held by another agent's reservation is `Full` to
    /// a second sender.
    Busy,
    /// Shared capacity is exhausted; receiver progress is required.
    Full,
    /// Nothing is published yet; sender progress is required.
    Empty,
}

/// How long a retrying operation keeps trying, and what it spends while it waits.
///
/// The retry loops in [`Signals`] do not own this answer: they re-attempt until
/// the policy says stop, so how many times to spin, whether to wait for the
/// peer, and when to abandon the call belong to the caller's policy rather than
/// to the queue. [`Adaptive`] is the ladder those loops use on their own.
///
/// Reporting `false` is not only a timeout. A policy that only ever parks runs one
/// operation to commit, and an agent holding both a `Tx` and an `Rx` stops reading while it
/// does that. If the peer is written the same way, each waits on the ring the other would
/// free by reading, and nothing inside either loop can notice. An agent owning both
/// directions therefore bounds its budget, or reads between attempts, rather than parking
/// without end; `tests/duplex_patience.rs` shows the pair stalling at a two-slot ring and
/// the same window draining once a read is attempted between publishes.
pub trait Patience {
    type Error;

    /// Opens one operation's retry run. A policy that counts attempts resets
    /// them here, so every call starts from its own budget.
    fn begin(&mut self) {}

    /// Spends one step after an attempt failed for `reason`, then reports
    /// whether the operation may be re-attempted. `false` ends the call without
    /// committing; `Err` reports the policy's own progress source as failed.
    fn step(&mut self, reason: Contention) -> impl Future<Output = Result<bool, Self::Error>> + '_;
}

/// Spins a bounded number of times per operation, then waits on the peer.
///
/// This is the ladder [`Signals::reserve`] and [`Signals::claim`] run, so a
/// caller that only needs a different spin count can say so without replacing
/// the retry loop. The budget is per operation: [`Patience::begin`] restores it,
/// and waiting on the peer means the operation is no longer making local
/// progress, so the next spin sequence starts from the full budget. It waits as
/// long as the peer takes and never abandons an operation on its own, which is
/// what [`Patience`] warns about for an agent holding both channel directions.
pub struct Adaptive<'a, W: ?Sized> {
    wait: &'a W,
    budget: usize,
    left: usize,
}

impl<'a, W: ?Sized> Adaptive<'a, W> {
    /// Waits on `wait` after the default spin budget.
    pub const fn new(wait: &'a W) -> Self {
        Self::budgeted(wait, LOCAL_RETRY_LIMIT)
    }

    /// Spends at most `budget` local spins on one operation before waiting on
    /// `wait`, and abandons the operation when a further busy result arrives
    /// with the budget spent.
    pub const fn budgeted(wait: &'a W, budget: usize) -> Self {
        Self {
            wait,
            budget,
            left: budget,
        }
    }
}

impl<W: Wait + ?Sized> Patience for Adaptive<'_, W> {
    type Error = W::Error;

    fn begin(&mut self) {
        self.left = self.budget;
    }

    async fn step(&mut self, reason: Contention) -> Result<bool, W::Error> {
        match spend(self.budget, &mut self.left, reason) {
            Effect::Stop => Ok(false),
            Effect::Spin => {
                core::hint::spin_loop();
                Ok(true)
            }
            Effect::Park => self.wait.wait().await.map(|()| true),
        }
    }
}

/// One step of the default ladder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Effect {
    /// Retry after a local pause.
    Spin,
    /// Retry after the peer's progress is observed.
    Park,
    /// Stop without committing.
    Stop,
}

/// The default ladder's transition, kept apart from its `await` so the shape of
/// the retry is checkable on its own.
fn spend(budget: usize, left: &mut usize, reason: Contention) -> Effect {
    match reason {
        Contention::Busy => {
            if *left == 0 {
                Effect::Stop
            } else {
                *left -= 1;
                Effect::Spin
            }
        }
        Contention::Full | Contention::Empty => {
            *left = budget;
            Effect::Park
        }
    }
}

/// A committed shared operation and the health of its advisory notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Committed<T, E> {
    value: T,
    notified: Result<(), E>,
}

impl<T, E> Committed<T, E> {
    pub fn into_parts(self) -> (T, Result<(), E>) {
        (self.value, self.notified)
    }
}

impl<T> Committed<T, Infallible> {
    pub fn into_value(self) -> T {
        match self.notified {
            Ok(()) => self.value,
            Err(never) => match never {},
        }
    }
}

/// An asynchronous operation that did not commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressError<E> {
    /// The waiting policy stopped before the operation could commit.
    Busy,
    Closed,
    /// The waiting policy reported its own progress source as failed.
    Wait(E),
}

impl<E: core::fmt::Debug> core::fmt::Display for ProgressError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Busy => f.write_str("the waiting policy gave up before the operation committed"),
            Self::Closed => f.write_str("the queue is closed"),
            Self::Wait(error) => write!(f, "waiting on the progress source failed: {error:?}"),
        }
    }
}

#[cfg(feature = "std")]
impl<E: core::fmt::Debug> std::error::Error for ProgressError<E> {}

/// Borrowed process-local progress capabilities for concrete Channel endpoints.
pub struct Signals<'a, N: ?Sized, W: ?Sized> {
    notify: &'a N,
    wait: &'a W,
}

fn committed<N: Notify + ?Sized, T>(notify: &N, value: T) -> Committed<T, N::Error> {
    Committed {
        value,
        notified: notify.notify(),
    }
}

impl<'a, N: ?Sized, W: ?Sized> Signals<'a, N, W> {
    pub const fn new(notify: &'a N, wait: &'a W) -> Self {
        Self { notify, wait }
    }
}

impl Signals<'static, (), ()> {
    /// Uses no notification or wait adapter for nonblocking operations.
    ///
    /// ```compile_fail
    /// # use evering::{Rx, notify::Signals};
    /// # fn cannot_wait(rx: &Rx<()>) {
    /// Signals::none().claim(rx);
    /// # }
    /// ```
    pub const fn none() -> Self {
        Self::new(&(), &())
    }
}

impl<'a, N: Notify + ?Sized> Signals<'a, N, ()> {
    pub const fn notify(notify: &'a N) -> Self {
        Self { notify, wait: &() }
    }
}

#[must_use = "dropping an unused permit cancels its Queue reservation"]
pub struct Permit<'n, 'q, H: Repr, N: Notify + ?Sized> {
    reserved: Option<TransferReserved<'q, H>>,
    notify: &'n N,
}

/// Shared state committed by a batch whose notification is still withheld.
///
/// [`Signals::defer`] opens a batch, and committing through the guard makes the
/// bytes visible to the peer without ringing its waiter, so `N` operations cost
/// one doorbell instead of `N`. The withheld notification is delivered by
/// [`Deferred::deliver`] or, if the batch never calls it, when the guard is
/// dropped: a batch that committed something cannot leave its peer asleep. A
/// batch that committed nothing rings nothing, which is why the doorbell count
/// is a choice rather than a consequence of the commit path.
///
/// Closing an endpoint is deliberately absent from this surface. A peer parked
/// on a terminal state must be told about it before the batch ends.
#[must_use = "a batch that committed through this guard still rings once when it is dropped"]
pub struct Deferred<'n, N: Notify + ?Sized> {
    notify: &'n N,
    owed: bool,
}

impl<N: Notify + ?Sized> Deferred<'_, N> {
    /// Publishes a staged transfer and withholds the notification it would ring.
    ///
    /// The batch is only made owed by an operation that published, so a failed publication costs
    /// this transfer and no doorbell.
    pub fn publish<H: Repr>(
        &mut self,
        staged: TransferStaged<'_, '_, H>,
    ) -> Result<(), PublishError> {
        staged.publish()?;
        self.owed = true;
        Ok(())
    }

    /// Adopts a claim, recycling its Queue slot, without ringing the peer.
    pub fn adopt<'q, 'p, H, T>(
        &mut self,
        received: Received<'q, H>,
        pool: PoolRef<'p>,
    ) -> Result<(H, Block<'p, T>), AdoptError<'q, H>>
    where
        H: Repr,
        T: Repr + Shape + ?Sized,
    {
        let adopted = received.adopt(pool)?;
        self.owed = true;
        Ok(adopted)
    }

    /// Admits a claim against an expected schema without ringing the peer.
    pub fn admit<'q, 'p, T>(
        &mut self,
        received: Received<'q, crate::Encoded>,
        pool: PoolRef<'p>,
        expected: crate::layout::SchemaKey,
    ) -> Result<Block<'p, T>, AdoptError<'q, crate::Encoded>>
    where
        T: Repr + Shape + ?Sized,
    {
        let admitted = received.admit(pool, expected)?;
        self.owed = true;
        Ok(admitted)
    }

    /// Discards a claim, recycling its slot, without ringing the peer.
    pub fn discard<'q, H: Repr>(
        &mut self,
        received: Received<'q, H>,
        pool: PoolRef<'_>,
    ) -> Result<H, AdoptError<'q, H>> {
        let discarded = received.discard(pool)?;
        self.owed = true;
        Ok(discarded)
    }

    /// Rings the withheld notification once, if the batch owes one.
    ///
    /// Returns the provider's health for the notification it attempted, and
    /// `Ok(())` when nothing was owed. A failure is reported here and not
    /// retried at drop, matching every committed operation in [`Signals`].
    pub fn deliver(&mut self) -> Result<(), N::Error> {
        if !self.owed {
            return Ok(());
        }
        self.owed = false;
        self.notify.notify()
    }
}

impl<N: Notify + ?Sized> Drop for Deferred<'_, N> {
    fn drop(&mut self) {
        if self.owed {
            let _ = self.notify.notify();
        }
    }
}

impl<N: Notify + ?Sized, W: ?Sized> Signals<'_, N, W> {
    /// Opens a batch that commits without ringing the peer on each operation.
    ///
    /// The returned guard is the only place a batched doorbell can be owed: it
    /// delivers the withheld notification at [`Deferred::deliver`] or, failing
    /// that, when it is dropped. See [`Deferred`].
    pub const fn defer(&self) -> Deferred<'_, N> {
        Deferred {
            notify: self.notify,
            owed: false,
        }
    }

    #[expect(
        clippy::result_large_err,
        reason = "pre-commit failure returns the exact linear Transfer"
    )]
    pub fn try_send<'p, H: Repr>(
        &self,
        tx: &Tx<H>,
        value: Transfer<'p, H>,
    ) -> Result<Committed<(), N::Error>, TrySendError<Transfer<'p, H>>> {
        tx.try_send(value).map(|()| committed(self.notify, ()))
    }

    /// Publishes without reserving a parked retry, leaving the doorbell to
    /// `deferred` instead of ringing it.
    ///
    /// Committing and owing are not separable here: the only way to publish
    /// through a guard is to hold it mutably, and holding it mutably is what
    /// records that the batch still owes exactly one notification.
    #[expect(
        clippy::result_large_err,
        reason = "pre-commit failure returns the exact linear Transfer"
    )]
    pub fn try_publish<'p, H: Repr>(
        &self,
        tx: &Tx<H>,
        value: Transfer<'p, H>,
        deferred: &mut Deferred<'_, N>,
    ) -> Result<(), TrySendError<Transfer<'p, H>>> {
        match tx.reserve() {
            Ok(reserved) => deferred
                .publish(reserved.stage(value))
                .map_err(|_| TrySendError::Authority)?,
            Err(crate::SendReserveError::Full) => return Err(TrySendError::Full(value)),
            Err(crate::SendReserveError::Busy) => return Err(TrySendError::Busy(value)),
            Err(crate::SendReserveError::Closed) => {
                return Err(TrySendError::Disconnected(value));
            }
        }
        Ok(())
    }

    /// Reserves a publish slot, waiting with the default [`Adaptive`] ladder.
    pub async fn reserve<'q, H: Repr>(
        &self,
        tx: &'q Tx<H>,
    ) -> Result<Permit<'_, 'q, H, N>, ProgressError<W::Error>>
    where
        W: Wait,
    {
        let mut patience = Adaptive::new(self.wait);
        self.reserve_with(tx, &mut patience).await
    }

    /// Reserves a publish slot with `patience` deciding every wait.
    ///
    /// A step that reports no further attempt is returned as
    /// [`ProgressError::Busy`]: the operation did not commit and the policy
    /// chose to stop, which is the only way this loop stops for local
    /// contention. A deadline belongs to the policy, not to the retry loop. An
    /// agent that holds both directions of a channel reads between its attempts
    /// instead of running this loop to commit, for the reason given on
    /// [`Patience`].
    pub async fn reserve_with<'q, H, P>(
        &self,
        tx: &'q Tx<H>,
        patience: &mut P,
    ) -> Result<Permit<'_, 'q, H, N>, ProgressError<P::Error>>
    where
        H: Repr,
        P: Patience + ?Sized,
    {
        patience.begin();
        loop {
            let reason = match tx.reserve() {
                Ok(reserved) => {
                    return Ok(Permit {
                        reserved: Some(reserved),
                        notify: self.notify,
                    });
                }
                Err(crate::SendReserveError::Closed) => return Err(ProgressError::Closed),
                Err(crate::SendReserveError::Busy) => Contention::Busy,
                Err(crate::SendReserveError::Full) => Contention::Full,
            };
            let further = patience.step(reason).await.map_err(ProgressError::Wait)?;
            if !further {
                return Err(ProgressError::Busy);
            }
        }
    }

    /// Claims the next published slot, waiting with the default [`Adaptive`]
    /// ladder.
    pub async fn claim<'q, H: Repr>(
        &self,
        rx: &'q Rx<H>,
    ) -> Result<Received<'q, H>, ProgressError<W::Error>>
    where
        W: Wait,
    {
        let mut patience = Adaptive::new(self.wait);
        self.claim_with(rx, &mut patience).await
    }

    /// Claims the next published slot with `patience` deciding every wait.
    pub async fn claim_with<'q, H, P>(
        &self,
        rx: &'q Rx<H>,
        patience: &mut P,
    ) -> Result<Received<'q, H>, ProgressError<P::Error>>
    where
        H: Repr,
        P: Patience + ?Sized,
    {
        patience.begin();
        loop {
            let reason = match rx.claim() {
                Ok(received) => return Ok(received),
                Err(ReceiveError::Closed) => return Err(ProgressError::Closed),
                Err(ReceiveError::Busy) => Contention::Busy,
                Err(ReceiveError::Empty) => Contention::Empty,
            };
            let further = patience.step(reason).await.map_err(ProgressError::Wait)?;
            if !further {
                return Err(ProgressError::Busy);
            }
        }
    }

    #[expect(
        clippy::type_complexity,
        reason = "the result exposes both committed notification health and retained claim authority"
    )]
    pub fn adopt<'q, 'p, H, T>(
        &self,
        received: Received<'q, H>,
        pool: PoolRef<'p>,
    ) -> Result<Committed<(H, Block<'p, T>), N::Error>, AdoptError<'q, H>>
    where
        H: Repr,
        T: Repr + Shape + ?Sized,
    {
        received
            .adopt(pool)
            .map(|value| committed(self.notify, value))
    }

    pub fn admit<'q, 'p, T>(
        &self,
        received: Received<'q, crate::Encoded>,
        pool: PoolRef<'p>,
        expected: crate::layout::SchemaKey,
    ) -> Result<Committed<Block<'p, T>, N::Error>, AdoptError<'q, crate::Encoded>>
    where
        T: Repr + crate::token::Shape + ?Sized,
    {
        received
            .admit(pool, expected)
            .map(|value| committed(self.notify, value))
    }

    pub fn discard<'q, H: Repr>(
        &self,
        received: Received<'q, H>,
        pool: PoolRef<'_>,
    ) -> Result<Committed<H, N::Error>, AdoptError<'q, H>> {
        received
            .discard(pool)
            .map(|value| committed(self.notify, value))
    }

    pub fn close_tx<H: Repr>(&self, tx: &Tx<H>) -> Committed<(), N::Error> {
        tx.close();
        committed(self.notify, ())
    }

    pub fn close_rx<H: Repr>(&self, rx: &Rx<H>) -> Committed<(), N::Error> {
        rx.close();
        committed(self.notify, ())
    }
}

impl<'n, 'q, H: Repr, N: Notify + ?Sized> Permit<'n, 'q, H, N> {
    fn rollback(&mut self) {
        drop(self.reserved.take());
    }

    /// Publishes into the reserved slot, then rings the doorbell.
    ///
    /// `Err(PublishError)` means nothing was committed and no notification is owed: another
    /// participant reclaimed the block's authority before this publish.
    pub fn send<'p>(
        mut self,
        value: Transfer<'p, H>,
    ) -> Result<Committed<(), N::Error>, PublishError> {
        self.reserved.take().unwrap().stage(value).publish()?;
        Ok(committed(self.notify, ()))
    }

    /// Commits the reserved slot and hands the doorbell to `deferred`.
    ///
    /// [`Permit::send`] rings once per operation; this is the same commit with
    /// the notification withheld, so a batch of reserved publishes costs the one
    /// ring its guard delivers. The guard must borrow the notification authority
    /// the permit itself reserved against, which is what ties the batch to the
    /// peer that is owed the wake.
    pub fn publish<'p>(
        mut self,
        value: Transfer<'p, H>,
        deferred: &mut Deferred<'n, N>,
    ) -> Result<(), PublishError> {
        deferred.publish(self.reserved.take().unwrap().stage(value))
    }

    pub fn cancel(mut self) -> Committed<(), N::Error> {
        self.rollback();
        committed(self.notify, ())
    }
}

impl<H: Repr, N: Notify + ?Sized> Drop for Permit<'_, '_, H, N> {
    fn drop(&mut self) {
        if self.reserved.is_some() {
            self.rollback();
            let _ = self.notify.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Contention, Effect, spend};

    const BUDGET: usize = 3;
    const REASONS: [Contention; 3] = [Contention::Busy, Contention::Full, Contention::Empty];

    /// Runs the default ladder over `path` and reports what it spent.
    ///
    /// Assertion failures inside mean the ladder answered a reason the way the
    /// retry loop must not.
    fn walk(budget: usize, path: &[Contention]) -> (usize, usize, bool) {
        let mut left = budget;
        let mut spins = 0;
        let mut since_park = 0;
        for reason in path {
            match spend(budget, &mut left, *reason) {
                Effect::Stop => return (spins, 0, true),
                Effect::Spin => {
                    assert_eq!(
                        *reason,
                        Contention::Busy,
                        "a peer-bound result is never answered by local spinning"
                    );
                    since_park += 1;
                    assert!(
                        since_park <= budget,
                        "one spin streak may not outlast the budget"
                    );
                    spins += 1;
                }
                Effect::Park => {
                    assert_ne!(
                        *reason,
                        Contention::Busy,
                        "local contention never waits on the peer's readiness"
                    );
                    assert_eq!(left, budget, "a park refills the local budget");
                    since_park = 0;
                }
            }
        }
        (spins, since_park, false)
    }

    #[test]
    fn a_busy_streak_ends_within_the_budget() {
        for budget in 0..=5 {
            let path = vec![Contention::Busy; budget + 1];
            let (spins, _, stopped) = walk(budget, &path);
            assert!(stopped, "a local contender cannot spin without bound");
            assert_eq!(spins, budget);
        }
    }

    #[test]
    fn a_park_refills_so_a_long_lived_pair_keeps_making_progress() {
        let mut path = vec![Contention::Busy; BUDGET];
        path.push(Contention::Empty);
        path.extend((0..BUDGET).map(|_| Contention::Busy));
        let (spins, since_park, stopped) = walk(BUDGET, &path);
        assert!(
            !stopped,
            "the budget is per streak, not per operation lifetime"
        );
        assert_eq!(spins, 2 * BUDGET);
        assert_eq!(since_park, BUDGET);
    }

    #[test]
    fn no_reason_sequence_parks_on_local_contention() {
        let mut paths = vec![Vec::new()];
        while let Some(path) = paths.pop() {
            let (_, _, stopped) = walk(BUDGET, &path);
            if stopped || path.len() == 5 {
                continue;
            }
            for reason in REASONS {
                let mut longer = path.clone();
                longer.push(reason);
                paths.push(longer);
            }
        }
    }
}
