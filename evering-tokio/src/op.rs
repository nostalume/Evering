use evering::driver::locked::{LockDriverSpec, SlabDriver};
use evering::driver::unlocked::PoolDriver;
use evering::driver::{Completer, SQEHandle};
use evering::uring::UringSpec;

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
pub struct MyHandle;

impl SQEHandle<MySlabDriver> for MyHandle {
    fn try_handle_ref(cq: &Completer<MySlabDriver>) {
        // use tokio::time::{self, Duration};
        while let Ok(ch) = cq.receiver().try_recv() {
            println!("[handle]: recv: {}", ch);
            // time::sleep(Duration::from_millis(50)).await;
            let res = fastrand::alphabetic();
            if let Err(e) = cq.sender().try_send(ch.replace(res)) {
                println!("[handle]: send err: {}", e);
            }
            println!("[handle]: send: {}", res);
        }
    }

    async fn handle(cq: Completer<MySlabDriver>) {
        while let Ok(ch) = cq.receiver().recv().await {
            println!("[handle]: recv: {}", ch);
            // time::sleep(Duration::from_millis(50)).await;
            let res = fastrand::alphabetic();
            if let Err(e) = cq.sender().send(ch.replace(res)).await {
                println!("[handle]: send err: {}", e);
            }
            println!("[handle]: send: {}", res);
        }
    }
}

impl SQEHandle<MyPoolDriver> for MyHandle {
    fn try_handle_ref(cq: &Completer<MyPoolDriver>) {
        // use tokio::time::{self, Duration};
        while let Ok(ch) = cq.receiver().try_recv() {
            println!("[handle]: recv: {}", ch);
            // time::sleep(Duration::from_millis(50)).await;
            let res = fastrand::alphabetic();
            if let Err(e) = cq.sender().try_send(ch.replace(res)) {
                println!("[handle]: send err: {}", e);
            }
            println!("[handle]: send: {}", res);
        }
    }

    async fn handle(cq: Completer<MyPoolDriver>) {
        while let Ok(ch) = cq.receiver().recv().await {
            println!("[handle]: recv: {}", ch);
            // time::sleep(Duration::from_millis(50)).await;
            let res = fastrand::alphabetic();
            if let Err(e) = cq.sender().send(ch.replace(res)).await {
                println!("[handle]: send err: {}", e);
            }
            println!("[handle]: send: {}", res);
        }
    }
}

#[cfg(test)]
mod tests {
    use evering::uring::asynch::default_channel;
    use tokio::task::yield_now;

    use crate::op::CharUring;

    #[tokio::test]
    async fn uring_sync() {
        let (pa, pb) = default_channel::<CharUring>();
        // sync test
        for _ in 0..10 {
            let sqe = fastrand::alphabetic();
            pa.sender().send(sqe).await.unwrap();
            let Ok(res) = pb.receiver().recv().await else {
                unreachable!()
            };
            dbg!(format!("pa -> pb: {}", res));
            assert_eq!(res, sqe);

            let cqe = fastrand::alphabetic();
            pb.sender().send(cqe).await.unwrap();
            let Ok(res) = pa.receiver().recv().await else {
                unreachable!()
            };
            dbg!(format!("pb -> pa: {}", res));
            assert_eq!(res, cqe);
        }

        // async test
        let send = tokio::spawn(async move {
            for _ in 0..10 {
                let ch = fastrand::alphabetic();
                dbg!(format!("send: {}", ch));
                pa.sender().send(ch).await.unwrap();
                yield_now().await;
            }
        });

        let recv = tokio::spawn(async move {
            while let Ok(ch) = pb.receiver().recv().await {
                dbg!(format!("recv: {}", ch));
                yield_now().await;
            }
        });

        let (res1, res2) = tokio::join!(send, recv);
        res1.unwrap();
        res2.unwrap();
    }
}
