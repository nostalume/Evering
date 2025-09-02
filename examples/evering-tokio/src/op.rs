use evering::driver::asynch::CompleterBridge;
use evering::driver::Pool;
use evering::uring::{IReceiver, ISender, UringSpec};

pub struct CharUring;
impl UringSpec for CharUring {
    type SQE = char;
    type CQE = char;
}

pub fn try_handle_ref(cq: &CompleterBridge<CharUring, Pool>) {
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

pub async fn handle(cq: &CompleterBridge<CharUring, Pool>) {
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
