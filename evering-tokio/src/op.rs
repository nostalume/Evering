use evering::driver::{Completer, DriverSpec, SQEHandle, WithSink, WithStream, lock::SpinMutex};
use evering::uring::UringSpec;

pub struct CharUring;
impl UringSpec for CharUring {
    type SQE = char;
    type CQE = char;
}

impl SQEHandle<CharUring> for CharUring {
    fn try_handle_ref(cq: &Completer<CharUring>) -> Self::HandleOutput {
        // use tokio::time::{self, Duration};
        while let Ok((id, ch)) = cq.receiver().try_recv() {
            println!("[handle]: recv: {}", ch);
            // time::sleep(Duration::from_millis(50)).await;
            let res = fastrand::alphabetic();
            if let Err(e) = cq.sender().try_send((id, res)) {
                println!("[handle]: send err: {}", e);
            }
            println!("[handle]: send: {}", res);
        }
    }

    async fn handle(cq: Completer<CharUring>) ->  Self::HandleOutput {
        use tokio::time::{self, Duration};
        while let Ok((id, ch)) = cq.receiver().recv().await {
            println!("[handle]: recv: {}", ch);
            time::sleep(Duration::from_millis(50)).await;
            let res = fastrand::alphabetic();
            if let Err(e) = cq.sender().send((id, res)).await {
                println!("[handle]: send err: {}", e);
            }
            println!("[handle]: send: {}", res);
        }
    }
}

pub struct ADriver;
impl DriverSpec for ADriver {
    type Lock = SpinMutex;
}

#[cfg(test)]
mod tests {
    use evering::uring::asynch::{WithSink, WithStream, default_channel};
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
