use core::future::Future;
use core::marker::PhantomData;

use crate::{
    driver::cell::IdCell,
    seal::Sealed,
    uring::{IReceiver, ISender, UringSpec},
};

mod cache;
pub mod cell;
pub mod locked;
pub mod unlocked;

pub trait Driver: Clone + Default {
    type Id: Clone;
    type Config;
    type Op: Future;

    fn register(&self) -> (Self::Id, Self::Op);
    fn complete(&self, id: Self::Id, payload: <Self::Op as Future>::Output);
    fn new(cfg: Self::Config) -> Self;
}

pub trait DriverFor<S: UringSpec> {
    type Driver: Driver;
}

pub struct Pool;
impl<S: UringSpec> DriverFor<S> for Pool
where
    S::CQE: 'static,
{
    type Driver = crate::driver::unlocked::PoolDriver<S::CQE>;
}

// pub struct Slab<L: LockFor>(PhantomData<L>);
// impl<S: UringSpec, L: LockFor> DriverFor<S> for Slab<L>
// where
//     S::CQE: 'static,
// {
//     type Driver = crate::driver::locked::SlabDriver<S::CQE, L>;
// }

pub trait Role: Sealed {}
pub struct Submit;
impl Sealed for Submit {}
impl Role for Submit {}
pub struct Receive;
impl Sealed for Receive {}
impl Role for Receive {}

pub struct BridgeTmpl<S: UringSpec, Sel: DriverFor<S>> {
    pub driver: Sel::Driver,
    _marker: PhantomData<S>,
}

impl<S: UringSpec, Sel: DriverFor<S>> Clone for BridgeTmpl<S, Sel> {
    fn clone(&self) -> Self {
        Self {
            driver: self.driver.clone(),
            _marker: PhantomData,
        }
    }
}

impl<S: UringSpec, Sel: DriverFor<S>> UringSpec for BridgeTmpl<S, Sel> {
    type SQE = IdCell<<Sel::Driver as Driver>::Id, S::SQE>;
    type CQE = IdCell<<Sel::Driver as Driver>::Id, S::CQE>;
}

pub struct Bridge<S: UringSpec, Chan, R: Role, Sel: DriverFor<S>> {
    bt: BridgeTmpl<S, Sel>,
    chan: Chan, // Submitter<BridgeTmpl<S, Sel>> or Completer<BridgeTmpl<S, Sel>>.
    _marker: PhantomData<R>,
}

impl<S: UringSpec, Chan: Clone, R: Role, Sel: DriverFor<S>> Clone for Bridge<S, Chan, R, Sel>
where
    BridgeTmpl<S, Sel>: Clone,
{
    fn clone(&self) -> Self {
        Self {
            bt: self.bt.clone(),
            chan: self.chan.clone(),
            _marker: PhantomData,
        }
    }
}

impl<S, Tx, Sel> Bridge<S, Tx, Submit, Sel>
where
    S: UringSpec,
    Sel: DriverFor<S>,
    Tx: ISender<Item = <BridgeTmpl<S, Sel> as UringSpec>::SQE> + Clone,
    <Sel::Driver as Driver>::Op: Future<Output = S::CQE>,
{
    pub async fn submit(
        &self,
        data: S::SQE,
    ) -> Result<<Sel::Driver as Driver>::Op, <Tx as ISender>::Error> {
        let (id, op) = self.bt.driver.register();
        let req = IdCell::new(id, data);
        self.chan.send(req).await?;
        Ok(op)
    }

    pub fn try_submit(
        &self,
        data: S::SQE,
    ) -> Result<<Sel::Driver as Driver>::Op, <Tx as ISender>::TryError> {
        let (id, op) = self.bt.driver.register();
        let req = IdCell::new(id, data);
        self.chan.try_send(req)?;
        Ok(op)
    }
}

impl<S, Rx, Sel> Bridge<S, Rx, Receive, Sel>
where
    S: UringSpec,
    Sel: DriverFor<S>,
    Rx: IReceiver<Item = <BridgeTmpl<S, Sel> as UringSpec>::CQE> + Clone,
    <Sel::Driver as Driver>::Op: Future<Output = S::CQE>,
{
    pub async fn complete(&self) {
        while let Ok(data) = self.chan.recv().await {
            let (id, payload) = data.into_inner();
            self.bt.driver.complete(id, payload);
        }
    }

    pub fn try_complete(&self) {
        if let Ok(data) = self.chan.try_recv() {
            let (id, payload) = data.into_inner();
            self.bt.driver.complete(id, payload);
        }
    }
}
fn build_with_driver<S, Chan, Sel>(
    chan: Chan,
    driver: Sel::Driver,
) -> (Bridge<S, Chan, Submit, Sel>, Bridge<S, Chan, Receive, Sel>)
where
    S: UringSpec,
    Chan: Clone,
    Sel: DriverFor<S>,
    Sel::Driver: Clone,
{
    let sq = Bridge {
        bt: BridgeTmpl {
            driver: driver.clone(),
            _marker: PhantomData,
        },
        chan: chan.clone(),
        _marker: PhantomData,
    };
    let rq = Bridge {
        bt: BridgeTmpl {
            driver,
            _marker: PhantomData,
        },
        chan,
        _marker: PhantomData,
    };
    (sq, rq)
}

fn build_default_bridge<S, Chan, Sel>(
    chan: Chan,
) -> (Bridge<S, Chan, Submit, Sel>, Bridge<S, Chan, Receive, Sel>)
where
    S: UringSpec,
    Chan: Clone,
    Sel: DriverFor<S>,
{
    build_with_driver(chan, Sel::Driver::default())
}

fn build_bridge<S, Chan, Sel>(
    chan: Chan,
    cfg: <Sel::Driver as Driver>::Config,
) -> (Bridge<S, Chan, Submit, Sel>, Bridge<S, Chan, Receive, Sel>)
where
    S: UringSpec,
    Chan: Clone,
    Sel: DriverFor<S>,
{
    build_with_driver(chan, Sel::Driver::new(cfg))
}

pub mod asynch {
    use crate::driver::{
        BridgeTmpl, Driver, DriverFor, Pool, Receive, Submit, build_bridge, build_default_bridge,
    };
    use crate::uring::UringSpec;
    use crate::{
        driver::Bridge,
        uring::asynch::{self, Completer, Submitter},
    };

    pub type CompleterBridge<S, D = Pool> = Completer<BridgeTmpl<S, D>>;
    pub type SubmitBridge<S, D = Pool> = Bridge<S, Submitter<BridgeTmpl<S, D>>, Submit, D>;
    pub type ReceiveBridge<S, D = Pool> = Bridge<S, Submitter<BridgeTmpl<S, D>>, Receive, D>;

    type Conn<S, D> = (
        SubmitBridge<S, D>,
        ReceiveBridge<S, D>,
        Completer<BridgeTmpl<S, D>>,
    );

    pub fn new<S: UringSpec, D: DriverFor<S>>(
        uring_cap: usize,
        driver_cfg: <D::Driver as Driver>::Config,
    ) -> Conn<S, D> {
        let (sq, cq) = asynch::new::<BridgeTmpl<S, D>>(uring_cap);
        let (sb, cb) = build_bridge(sq, driver_cfg);
        (sb, cb, cq)
    }

    pub fn default<S: UringSpec, D: DriverFor<S>>() -> Conn<S, D> {
        let (sq, cq) = asynch::default::<BridgeTmpl<S, D>>();
        let (sb, cb) = build_default_bridge(sq);
        (sb, cb, cq)
    }
}
