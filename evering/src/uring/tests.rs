#![cfg(test)]
use crate::uring::{asynch, bare, IReceiver, ISender};
use crate::uring::{UringSpec, sync};

use std::sync::atomic::{AtomicBool, Ordering};

struct CharUring;

impl UringSpec for CharUring {
    type SQE = char;
    type CQE = char;
}

fn local<C: ISender<Item = char> + IReceiver<Item = char>>(r: (C, C)) {
    let mut len_a = 0;
    let mut len_b = 0;
    let (pa, pb) = r;

    for _ in 0..32 {
        let ch = fastrand::alphabetic();
        match fastrand::u8(0..4) {
            0 => len_a += pa.try_send(ch).map_or(0, |_| 1),
            1 => len_b += pb.try_send(ch).map_or(0, |_| 1),
            2 => {
                if let Ok(ch) = pa.try_recv() {
                    dbg!(format!("A recv: {}", ch));
                    len_b -= 1;
                }
            }
            3 => {
                if let Ok(ch) = pb.try_recv() {
                    dbg!(format!("B recv: {}", ch));
                    len_a -= 1;
                }
            }
            _ => unreachable!(),
        }
    }
    dbg!(format!("{}, {}", len_a, len_b));
}

use core::fmt::Debug;

fn multi<C: ISender<Item = char> + IReceiver<Item = char> + Send + Sync>(r: (C, C))
where
    <C as ISender>::TryError: Debug,
{
    let input = std::iter::repeat_with(fastrand::alphabetic)
        .take(30)
        .collect::<Vec<_>>();

    let (pa, pb) = r;
    let (pa_finished, pb_finished) = (AtomicBool::new(false), AtomicBool::new(false));
    std::thread::scope(|cx| {
        cx.spawn(|| {
            let mut r = vec![];
            for i in input.iter().copied() {
                pa.try_send(i).unwrap();
                while let Ok(i) = pa.try_recv() {
                    r.push(i);
                }
            }
            pa_finished.store(true, Ordering::Release);
            while !pb_finished.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            while let Ok(i) = pa.try_recv() {
                r.push(i);
            }
            assert_eq!(r, input);
        });
        cx.spawn(|| {
            let mut r = vec![];
            for i in input.iter().copied() {
                pb.try_send(i).unwrap();
                while let Ok(i) = pb.try_recv() {
                    r.push(i);
                }
            }
            pb_finished.store(true, Ordering::Release);
            while !pa_finished.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            while let Ok(i) = pb.try_recv() {
                r.push(i);
            }
            assert_eq!(r, input);
        });
    });
}

macro_rules! collect_test {
    ($fn:expr) => {{
        let r1 = $fn;
        let r2 = $fn;
        local(r1);
        multi(r2);
    }};
}

#[test]
fn collect() {
    collect_test!(sync::default_channel::<CharUring>());
    collect_test!(asynch::default_channel::<CharUring>());
}
