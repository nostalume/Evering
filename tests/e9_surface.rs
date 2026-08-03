#![cfg(all(feature = "map", feature = "tokio"))]

use std::{
    cell::{Cell, RefCell},
    future,
    rc::Rc,
};

use evering::{
    Encoded, ReceiveError, Session,
    layout::{RegionId, Repr, SchemaId, SchemaKey},
    mapping::{Access, Request},
    notify::{Committed, Notify, ProgressError, Signals, Wait},
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
