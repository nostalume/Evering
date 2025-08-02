use evering::driver::SQEHandle;
use tokio::task::yield_now;

mod op;

use op::{MyDriver, MyHandle};

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let (sb, cb, cq) = evering::driver::new::<MyDriver>();

    tokio::spawn(async move {
        loop {
            cb.try_complete();
            MyHandle::try_handle_ref(&cq);
            yield_now().await;
        }
    });

    tokio::spawn(async move {
        for i in 0..100 {
            let ch = fastrand::alphabetic();
            println!("[submit]: send {}", ch);
            let res = sb.try_submit(ch).unwrap().await;
            println!("[submit]: recv {}: {}", i, res);
            yield_now().await;
        }
    });

    use tokio::time::{self, Duration};
    time::sleep(Duration::from_secs(3)).await;
}
