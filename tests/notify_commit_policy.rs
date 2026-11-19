#![cfg(all(feature = "map", feature = "tokio"))]

use std::{
    cell::{Cell, RefCell},
    future,
    rc::Rc,
};

use evering::{
    Encoded, ReceiveError, Session, TrySendError,
    layout::{RegionId, Repr, SchemaId, SchemaKey},
    mapping::{Access, Request},
    notify::{
        Adaptive, Committed, Contention, Deferred, Notify, Patience, ProgressError, Signals, Wait,
    },
};

const REGION: RegionId = RegionId::new(0x6539_7375_7266_6163, 1);
const RUNTIME: SchemaKey = SchemaKey::new(SchemaId(0x6539_7275_6e74_696d), 1);

#[repr(C)]
#[derive(Debug)]
struct Envelope(u64);

unsafe impl Repr for Envelope {
    const SCHEMA: SchemaKey = SchemaKey::new(SchemaId(0x6539_656e_7665_6c6f), 1);
}

struct Bell(Cell<usize>);

impl Notify for Bell {
    type Error = ();

    fn notify(&self) -> Result<(), Self::Error> {
        self.0.set(self.0.get() + 1);
        Ok(())
    }
}

struct LocalWait(Rc<()>, Cell<usize>);

impl Wait for LocalWait {
    type Error = ();

    fn wait(&self) -> impl Future<Output = Result<(), Self::Error>> + '_ {
        self.1.set(self.1.get() + 1);
        future::ready(Ok(()))
    }
}

struct ActingWait<F>(RefCell<F>, Cell<usize>);

impl<F: FnMut()> Wait for ActingWait<F> {
    type Error = ();

    fn wait(&self) -> impl Future<Output = Result<(), Self::Error>> + '_ {
        self.1.set(self.1.get() + 1);
        (self.0.borrow_mut())();
        future::ready(Ok(()))
    }
}

struct FaultBell(Cell<usize>);

impl Notify for FaultBell {
    type Error = u8;

    fn notify(&self) -> Result<(), Self::Error> {
        self.0.set(self.0.get() + 1);
        Err(7)
    }
}

struct FailedWait;

impl Wait for FailedWait {
    type Error = u8;

    fn wait(&self) -> impl Future<Output = Result<(), Self::Error>> + '_ {
        future::ready(Err(9))
    }
}

#[cfg(unix)]
fn sessions() -> (Session, Session) {
    let source = evering::os::unix::UnixFd::memfd("evering-e9", 1 << 20, false).unwrap();
    let request = Request::new(1 << 20, Access::READ | Access::WRITE);
    (
        Session::create(source.borrow(), request, REGION).unwrap(),
        Session::open(source.borrow(), request, REGION).unwrap(),
    )
}

#[test]
fn signal_capabilities_are_static_and_commits_are_consumed_explicitly() {
    let (left_session, right_session) = sessions();
    let (left, port) = left_session.create_channel::<Encoded>(1).unwrap();
    let right = right_session.adopt(port).unwrap();
    let pool = left_session.create_pool(64 * 1024, None).unwrap();
    let right_pool = right_session.open_pool(pool.id()).unwrap();
    let (tx, _) = left.split();
    let (_, rx) = right.split();
    let signals = Signals::none();

    signals
        .try_send(&tx, pool.as_ref().put(29_u64).unwrap().encode(RUNTIME))
        .unwrap()
        .into_value();
    let value = signals
        .admit::<u64>(rx.claim().unwrap(), right_pool.as_ref(), RUNTIME)
        .unwrap()
        .into_value();
    assert_eq!(*value, 29);

    let bell = FaultBell(Cell::new(0));
    let ((), notified) = Signals::notify(&bell).close_tx(&tx).into_parts();
    assert_eq!(notified, Err(7));
}

#[cfg(windows)]
fn sessions() -> (Session, Session) {
    let source =
        evering::os::windows::Section::anonymous(1 << 20, Access::READ | Access::WRITE).unwrap();
    let request = Request::new(1 << 20, Access::READ | Access::WRITE);
    (
        Session::create(source.borrow(), request, REGION).unwrap(),
        Session::open(source.borrow(), request, REGION).unwrap(),
    )
}

#[tokio::test]
async fn borrowed_signals_commit_then_notify_concrete_endpoints() {
    let (left_session, right_session) = sessions();
    let (left, port) = left_session.create_channel::<Envelope>(2).unwrap();
    let right = right_session.adopt(port).unwrap();
    let pool = left_session.create_pool(64 * 1024, None).unwrap();
    let right_pool = right_session.open_pool(pool.id()).unwrap();
    let (tx, _) = left.split();
    let (_, rx) = right.split();
    let bell = Bell(Cell::new(0));
    let wait = LocalWait(Rc::new(()), Cell::new(0));
    let signals = Signals::new(&bell, &wait);

    let sent: Committed<(), ()> = signals
        .try_send(
            &tx,
            pool.as_ref().put(7_u64).unwrap().transfer(Envelope(11)),
        )
        .unwrap();
    assert_eq!(sent.into_parts().1, Ok(()));
    assert_eq!(bell.0.get(), 1);

    let received = signals.claim(&rx).await.unwrap();
    assert_eq!(bell.0.get(), 1, "claim alone does not recycle capacity");
    let discarded = signals.discard(received, right_pool.as_ref()).unwrap();
    let (discarded, _) = discarded.into_parts();
    assert_eq!(discarded.0, 11);
    assert_eq!(bell.0.get(), 2);
    assert_eq!(wait.1.get(), 0);

    let _: Option<ProgressError<()>> = None;
    let _ = &wait.0;
}

#[tokio::test]
async fn permit_rollback_precedes_its_notification() {
    let (left_session, right_session) = sessions();
    let (left, port) = left_session.create_channel::<Envelope>(1).unwrap();
    let right = right_session.adopt(port).unwrap();
    let (tx, _) = left.split();
    let (_, rx) = right.split();
    let bell = Bell(Cell::new(0));
    let wait = LocalWait(Rc::new(()), Cell::new(0));
    let signals = Signals::new(&bell, &wait);

    drop(signals.reserve(&tx).await.unwrap());
    assert_eq!(bell.0.get(), 1);
    assert!(matches!(rx.claim(), Err(ReceiveError::Busy)));

    let cancelled = signals.reserve(&tx).await.unwrap().cancel();
    assert_eq!(cancelled.into_parts().1, Ok(()));
    assert_eq!(bell.0.get(), 2);
}

#[tokio::test]
async fn full_reserve_waits_before_payload_exists() {
    let (left_session, right_session) = sessions();
    let (left, port) = left_session.create_channel::<Envelope>(1).unwrap();
    let right = right_session.adopt(port).unwrap();
    let pool = left_session.create_pool(64 * 1024, None).unwrap();
    let right_pool = right_session.open_pool(pool.id()).unwrap();
    let (tx, _) = left.split();
    let (_, rx) = right.split();
    tx.try_send(pool.as_ref().put(3_u64).unwrap().transfer(Envelope(5)))
        .unwrap();

    let wait = ActingWait(
        RefCell::new(|| {
            rx.claim().unwrap().discard(right_pool.as_ref()).unwrap();
        }),
        Cell::new(0),
    );
    let bell = Bell(Cell::new(0));
    let signals = Signals::new(&bell, &wait);
    let permit = signals.reserve(&tx).await.unwrap();
    assert_eq!(wait.1.get(), 1);
    assert_eq!(bell.0.get(), 0);
    permit.cancel();
    assert_eq!(bell.0.get(), 1);
}

#[tokio::test]
async fn notification_failure_never_turns_commit_into_retry() {
    let (left_session, right_session) = sessions();
    let (left, port) = left_session.create_channel::<Envelope>(1).unwrap();
    let right = right_session.adopt(port).unwrap();
    let pool = left_session.create_pool(64 * 1024, None).unwrap();
    let right_pool = right_session.open_pool(pool.id()).unwrap();
    let (tx, _) = left.split();
    let (_, rx) = right.split();
    let bell = FaultBell(Cell::new(0));
    let wait = LocalWait(Rc::new(()), Cell::new(0));
    let signals = Signals::new(&bell, &wait);

    let committed = signals
        .try_send(
            &tx,
            pool.as_ref().put(13_u64).unwrap().transfer(Envelope(17)),
        )
        .unwrap();
    assert_eq!(committed.into_parts().1, Err(7));
    let received = rx.claim().unwrap();
    let recycled = signals.discard(received, right_pool.as_ref()).unwrap();
    let (recycled, notified) = recycled.into_parts();
    assert_eq!(recycled.0, 17);
    assert_eq!(notified, Err(7));
    assert_eq!(bell.0.get(), 2);
}

#[tokio::test]
async fn wait_and_admission_failures_preserve_precommit_authority() {
    let (left_session, right_session) = sessions();
    let (left, port) = left_session.create_channel::<Envelope>(1).unwrap();
    let right = right_session.adopt(port).unwrap();
    let pool = left_session.create_pool(64 * 1024, None).unwrap();
    let right_pool = right_session.open_pool(pool.id()).unwrap();
    let wrong_pool = right_session.create_pool(64 * 1024, None).unwrap();
    let (tx, _) = left.split();
    let (_, rx) = right.split();
    let bell = Bell(Cell::new(0));

    assert!(matches!(
        Signals::new(&bell, &FailedWait).claim(&rx).await,
        Err(ProgressError::Wait(9))
    ));
    let signals = Signals::new(&bell, &FailedWait);
    signals
        .try_send(
            &tx,
            pool.as_ref().put(19_u64).unwrap().transfer(Envelope(23)),
        )
        .unwrap();
    let received = rx.claim().unwrap();
    let received = match signals.adopt::<_, u64>(received, wrong_pool.as_ref()) {
        Ok(_) => panic!("wrong Pool cannot admit the transfer"),
        Err(error) => error.into_received(),
    };
    assert_eq!(bell.0.get(), 1, "failed admission does not recycle");
    signals.discard(received, right_pool.as_ref()).unwrap();
    assert_eq!(bell.0.get(), 2);

    assert_eq!(signals.close_tx(&tx).into_parts().1, Ok(()));
    assert_eq!(bell.0.get(), 3);
    assert!(matches!(rx.claim(), Err(ReceiveError::Closed)));
}

#[tokio::test]
async fn a_deferred_batch_commits_every_slot_and_rings_once() {
    let (left_session, right_session) = sessions();
    let (left, port) = left_session.create_channel::<Envelope>(4).unwrap();
    let right = right_session.adopt(port).unwrap();
    let pool = left_session.create_pool(64 * 1024, None).unwrap();
    let right_pool = right_session.open_pool(pool.id()).unwrap();
    let (tx, _) = left.split();
    let (_, rx) = right.split();
    let bell = Bell(Cell::new(0));
    let wait = LocalWait(Rc::new(()), Cell::new(0));
    let signals = Signals::new(&bell, &wait);

    let mut batch = signals.defer();
    for payload in [3_u64, 5, 7, 11] {
        signals
            .reserve(&tx)
            .await
            .unwrap()
            .publish(
                pool.as_ref()
                    .put(payload)
                    .unwrap()
                    .transfer(Envelope(payload)),
                &mut batch,
            )
            .unwrap();
    }
    assert_eq!(bell.0.get(), 0, "four committed publishes ring nothing");

    for payload in [3_u64, 5, 7, 11] {
        let (header, block) = batch
            .adopt::<_, u64>(rx.claim().unwrap(), right_pool.as_ref())
            .unwrap();
        assert_eq!(header.0, payload);
        assert_eq!(*block, payload);
    }
    assert_eq!(
        bell.0.get(),
        0,
        "recycling four slots through the guard still rings nothing"
    );

    assert_eq!(batch.deliver(), Ok(()));
    assert_eq!(bell.0.get(), 1, "one doorbell per batch, both directions");
}

#[tokio::test]
async fn a_forgotten_batch_rings_when_the_guard_dies() {
    let (left_session, right_session) = sessions();
    let (left, port) = left_session.create_channel::<Envelope>(2).unwrap();
    let right = right_session.adopt(port).unwrap();
    let pool = left_session.create_pool(64 * 1024, None).unwrap();
    let (tx, _) = left.split();
    let (_, _rx) = right.split();
    let bell = Bell(Cell::new(0));
    let wait = LocalWait(Rc::new(()), Cell::new(0));
    let signals = Signals::new(&bell, &wait);

    {
        let mut batch = signals.defer();
        signals
            .try_publish(
                &tx,
                pool.as_ref().put(13_u64).unwrap().transfer(Envelope(17)),
                &mut batch,
            )
            .unwrap();
        assert_eq!(bell.0.get(), 0);
    }
    assert_eq!(
        bell.0.get(),
        1,
        "a batch that committed cannot forget its doorbell"
    );
}

#[tokio::test]
async fn an_empty_batch_owes_no_doorbell() {
    let (left_session, right_session) = sessions();
    let (left, port) = left_session.create_channel::<Envelope>(1).unwrap();
    let _right = right_session.adopt(port).unwrap();
    let (tx, _) = left.split();
    let bell = Bell(Cell::new(0));
    let wait = LocalWait(Rc::new(()), Cell::new(0));
    let signals = Signals::new(&bell, &wait);
    let mut batch: Deferred<'_, Bell> = signals.defer();
    let _ = &tx;

    assert_eq!(batch.deliver(), Ok(()), "nothing owed is not a failure");
    drop(batch);
    assert_eq!(
        bell.0.get(),
        0,
        "a batch that committed nothing wakes nothing"
    );
}

#[tokio::test]
async fn one_guard_rings_once_per_group_of_commits() {
    let (left_session, right_session) = sessions();
    let (left, port) = left_session.create_channel::<Envelope>(2).unwrap();
    let right = right_session.adopt(port).unwrap();
    let pool = left_session.create_pool(64 * 1024, None).unwrap();
    let right_pool = right_session.open_pool(pool.id()).unwrap();
    let (tx, _) = left.split();
    let (_, rx) = right.split();
    let bell = Bell(Cell::new(0));
    let wait = LocalWait(Rc::new(()), Cell::new(0));
    let signals = Signals::new(&bell, &wait);
    let mut batch = signals.defer();

    signals
        .try_publish(
            &tx,
            pool.as_ref().put(19_u64).unwrap().transfer(Envelope(23)),
            &mut batch,
        )
        .unwrap();
    assert_eq!(batch.deliver(), Ok(()));
    assert_eq!(batch.deliver(), Ok(()));
    assert_eq!(
        bell.0.get(),
        1,
        "an undelivered second call is not a second trap"
    );

    batch
        .discard::<Envelope>(rx.claim().unwrap(), right_pool.as_ref())
        .unwrap();
    assert_eq!(bell.0.get(), 1);
    drop(batch);
    assert_eq!(bell.0.get(), 2, "the second group is still owed one ring");
}

#[tokio::test]
async fn a_failed_delivery_is_reported_once_and_not_retried_at_drop() {
    let (left_session, right_session) = sessions();
    let (left, port) = left_session.create_channel::<Envelope>(1).unwrap();
    let _right = right_session.adopt(port).unwrap();
    let pool = left_session.create_pool(64 * 1024, None).unwrap();
    let (tx, _) = left.split();
    let bell = FaultBell(Cell::new(0));
    let wait = LocalWait(Rc::new(()), Cell::new(0));
    let signals = Signals::new(&bell, &wait);
    let mut batch = signals.defer();

    signals
        .try_publish(
            &tx,
            pool.as_ref().put(29_u64).unwrap().transfer(Envelope(31)),
            &mut batch,
        )
        .unwrap();
    assert_eq!(batch.deliver(), Err(7));
    drop(batch);
    assert_eq!(
        bell.0.get(),
        1,
        "a withheld doorbell reports its failure once, like every committed operation"
    );
}

#[tokio::test]
async fn a_refused_publish_keeps_the_transfer_and_owes_nothing() {
    let (left_session, right_session) = sessions();
    let (left, port) = left_session.create_channel::<Envelope>(1).unwrap();
    let right = right_session.adopt(port).unwrap();
    let pool = left_session.create_pool(64 * 1024, None).unwrap();
    let right_pool = right_session.open_pool(pool.id()).unwrap();
    let (tx, _) = left.split();
    let (_, rx) = right.split();
    let bell = Bell(Cell::new(0));
    let wait = LocalWait(Rc::new(()), Cell::new(0));
    let signals = Signals::new(&bell, &wait);
    let mut batch = signals.defer();

    signals
        .try_publish(
            &tx,
            pool.as_ref().put(37_u64).unwrap().transfer(Envelope(41)),
            &mut batch,
        )
        .unwrap();
    let rejected = signals
        .try_publish(
            &tx,
            pool.as_ref().put(43_u64).unwrap().transfer(Envelope(47)),
            &mut batch,
        )
        .expect_err("a full Queue must not consume the batch");
    assert!(matches!(rejected, TrySendError::Full(_)));

    batch.deliver().unwrap();
    assert_eq!(bell.0.get(), 1, "only the committed publish is owed a ring");
    let recycled = rx.claim().unwrap();
    batch
        .discard::<Envelope>(recycled, right_pool.as_ref())
        .unwrap();
    assert_eq!(batch.deliver(), Ok(()));
    assert_eq!(bell.0.get(), 2);
}

struct Ledger {
    reasons: RefCell<Vec<Contention>>,
    begins: Cell<usize>,
    stop_after: usize,
}

impl Ledger {
    fn new(stop_after: usize) -> Self {
        Self {
            reasons: RefCell::new(Vec::new()),
            begins: Cell::new(0),
            stop_after,
        }
    }

    fn taken(&self) -> Vec<Contention> {
        self.reasons.borrow().clone()
    }
}

impl Patience for Ledger {
    type Error = u8;

    fn begin(&mut self) {
        self.begins.set(self.begins.get() + 1);
    }

    fn step(&mut self, reason: Contention) -> impl Future<Output = Result<bool, u8>> + '_ {
        self.reasons.borrow_mut().push(reason);
        let seen = self.reasons.borrow().len();
        future::ready(Ok(seen < self.stop_after))
    }
}

struct Faulty;

impl Patience for Faulty {
    type Error = u8;

    fn step(&mut self, _reason: Contention) -> impl Future<Output = Result<bool, u8>> + '_ {
        future::ready(Err(3))
    }
}

#[tokio::test]
async fn a_busy_claim_stops_where_its_policy_says_and_never_waits_on_the_peer() {
    let (left_session, right_session) = sessions();
    let (left, port) = left_session.create_channel::<Envelope>(1).unwrap();
    let right = right_session.adopt(port).unwrap();
    let (tx, _) = left.split();
    let (_, rx) = right.split();
    let bell = Bell(Cell::new(0));
    let wait = LocalWait(Rc::new(()), Cell::new(0));
    let signals = Signals::new(&bell, &wait);
    let held = signals.reserve(&tx).await.unwrap();

    let mut patience = Ledger::new(3);
    let outcome = signals.claim_with(&rx, &mut patience).await;
    assert!(matches!(outcome, Err(ProgressError::Busy)));
    assert_eq!(
        patience.taken(),
        vec![Contention::Busy; 3],
        "the retry loop must report the condition it met, not a fixed count"
    );
    assert_eq!(patience.begins.get(), 1);
    assert_eq!(
        wait.1.get(),
        0,
        "a busy claim never waits on the peer's readiness"
    );

    drop(held);
}

#[tokio::test]
async fn a_silent_policy_ends_a_reserve_and_a_failing_one_reports_itself() {
    let (left_session, right_session) = sessions();
    let (left, port) = left_session.create_channel::<Envelope>(1).unwrap();
    let _right = right_session.adopt(port).unwrap();
    let (tx, _) = left.split();
    let bell = Bell(Cell::new(0));
    let wait = LocalWait(Rc::new(()), Cell::new(0));
    let signals = Signals::new(&bell, &wait);
    let held = signals.reserve(&tx).await.unwrap();

    let mut once = Ledger::new(1);
    let outcome = signals.reserve_with(&tx, &mut once).await;
    assert!(matches!(outcome, Err(ProgressError::Busy)));
    assert_eq!(
        once.taken(),
        vec![Contention::Full],
        "an occupied slot reads to a second sender as peer-bound capacity, \
         and only the receive side calls an in-flight claim local contention"
    );

    let outcome = signals.reserve_with(&tx, &mut Faulty).await;
    assert!(matches!(outcome, Err(ProgressError::Wait(3))));
    assert!(
        matches!(held.cancel().into_parts(), ((), Ok(()))),
        "the abandoned retries left the outstanding reservation intact"
    );
}

#[tokio::test]
async fn an_injected_default_ladder_parks_for_the_peer_then_commits() {
    let (left_session, right_session) = sessions();
    let (left, port) = left_session.create_channel::<Envelope>(1).unwrap();
    let right = right_session.adopt(port).unwrap();
    let pool = left_session.create_pool(64 * 1024, None).unwrap();
    let right_pool = right_session.open_pool(pool.id()).unwrap();
    let (tx, _) = left.split();
    let (_, rx) = right.split();
    tx.try_send(pool.as_ref().put(5_u64).unwrap().transfer(Envelope(9)))
        .unwrap();

    let wait = ActingWait(
        RefCell::new(|| {
            rx.claim().unwrap().discard(right_pool.as_ref()).unwrap();
        }),
        Cell::new(0),
    );
    let bell = Bell(Cell::new(0));
    let signals = Signals::new(&bell, &wait);
    let mut patience = Adaptive::budgeted(&wait, 2);
    let permit = signals.reserve_with(&tx, &mut patience).await.unwrap();

    assert_eq!(
        wait.1.get(),
        1,
        "the peer-bound reason, not a local spin, is what waits"
    );
    assert_eq!(bell.0.get(), 0);
    permit.cancel();
    assert_eq!(bell.0.get(), 1);
}
