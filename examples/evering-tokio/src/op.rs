use std::marker::PhantomData;

use evering::driver::locked::{LockDriverSpec, SlabDriver};
use evering::driver::unlocked::PoolDriver;
use evering::driver::{asynch::Completer, Driver};
use evering::uring::{IReceiver, ISender, UringSpec};

pub struct CharUring;
impl UringSpec for CharUring {
    type SQE = char;
    type CQE = char;
}

pub struct Spin;
impl LockDriverSpec for Spin {
    type Lock = evering::driver::locked::lock::StdMutex;
}

pub type MySlabDriver = SlabDriver<CharUring, Spin>;
pub type MyPoolDriver = PoolDriver<CharUring>;
pub struct MyHandle<D: Driver>(PhantomData<D>);

impl MyHandle<MySlabDriver> {
    pub fn try_handle_ref(cq: &Completer<MySlabDriver>) {
        // use tokio::time::{self, Duration};
        while let Ok(ch) = cq.try_recv() {
            println!("[handle]: recv: {}", ch);
            // time::sleep(Duration::from_millis(50)).await;
            let res = fastrand::alphabetic();
            if let Err(e) = cq.try_send(ch.replace(res)) {
                println!("[handle]: send err: {}", e);
            }
            println!("[handle]: send: {}", res);
        }
    }

    pub async fn handle(cq: Completer<MySlabDriver>) {
        while let Ok(ch) = cq.recv().await {
            println!("[handle]: recv: {}", ch);
            // time::sleep(Duration::from_millis(50)).await;
            let res = fastrand::alphabetic();
            if let Err(e) = cq.send(ch.replace(res)).await {
                println!("[handle]: send err: {}", e);
            }
            println!("[handle]: send: {}", res);
        }
    }
}

impl MyHandle<MyPoolDriver> {
    pub fn try_handle_ref(cq: &Completer<MyPoolDriver>) {
        // use tokio::time::{self, Duration};
        while let Ok(ch) = cq.try_recv() {
            println!("[handle]: recv: {}", ch);
            // time::sleep(Duration::from_millis(50)).await;
            let res = fastrand::alphabetic();
            if let Err(e) = cq.try_send(ch.replace(res)) {
                println!("[handle]: send err: {}", e);
            }
            println!("[handle]: send: {}", res);
        }
    }

    pub async fn handle(cq: Completer<MyPoolDriver>) {
        while let Ok(ch) = cq.recv().await {
            println!("[handle]: recv: {}", ch);
            // time::sleep(Duration::from_millis(50)).await;
            let res = fastrand::alphabetic();
            if let Err(e) = cq.send(ch.replace(res)).await {
                println!("[handle]: send err: {}", e);
            }
            println!("[handle]: send: {}", res);
        }
    }
}