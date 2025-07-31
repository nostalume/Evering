use tokio::task::yield_now;
use evering::driver::SQEHandle;

mod op;

use op::{ADriver, CharUring};

#[tokio::main]
async fn main() {
    let (sb, cb, cq) = evering::driver::new::<CharUring, ADriver>();

    tokio::spawn(async move {
        loop {
            cb.try_complete();
            CharUring::try_handle_ref(&cq);
            yield_now().await;
        }
    });

    let sb_1 = sb.clone();
    tokio::spawn(async move {
        for i in 0..100 {
            let ch = fastrand::alphabetic();
            println!("[submit]: send {}", ch);
            let res = sb_1.try_submit(ch).unwrap().await;
            println!("[submit]: recv {}: {}", i, res);
        }
    });

    use tokio::time::{self, Duration};
    time::sleep(Duration::from_secs(3)).await;
}
