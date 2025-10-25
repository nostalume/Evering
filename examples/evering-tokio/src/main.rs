use evering::driver::Pool;
use tokio::task::yield_now;

use crate::op::{CharUring, handle};

mod op;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let (sb, cb, cq) = evering::driver::asynch::default::<CharUring, Pool>();

    for _ in 0..5 {
        let cq = cq.clone();
        let cb = cb.clone();
        tokio::spawn(async move { cb.complete().await });
        tokio::spawn(async move { handle(&cq).await });
    }

    for th in 0..5 {
        let sb = sb.clone();
        tokio::spawn(async move {
            for i in 0..1000 {
                let ch = fastrand::alphabetic();
                println!("[submit {}]: send {}", th, ch);
                let res = sb.try_submit(ch).unwrap().await;
                println!("[submit {}]: recv {}: {}", th, i, res);
                yield_now().await;
            }
        });
    }

    use tokio::time::{self, Duration};
    time::sleep(Duration::from_secs(10)).await;
}
