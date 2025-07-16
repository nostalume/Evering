#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicUsize};

use super::*;

struct CharUring;
impl UringSpec for CharUring {
    type A = char;
    type B = char;
}

#[test]
fn queue_inspect() {
    let mut len_a = 0;
    let mut len_b = 0;
    let (mut pa, mut pb) = Builder::<CharUring>::default().build();
    for _ in 0..32 {
        let ch = fastrand::alphabetic();
        match fastrand::u8(0..4) {
            0 => len_a += pa.send(ch).map_or(0, |_| 1),
            1 => len_b += pb.send(ch).map_or(0, |_| 1),
            2 => {
                if let Some(ch) = pa.recv() {
                    dbg!(format!("A recv: {}", ch));
                    len_b -= 1;
                }
            }
            3 => {
                if let Some(ch) = pb.recv() {
                    dbg!(format!("B recv: {}", ch));
                    len_a -= 1;
                }
            }
            _ => unreachable!(),
        }
        assert_eq!(pa.sender().len(), pb.receiver().len());
        assert_eq!(pa.receiver().len(), pb.sender().len());
        assert_eq!(pa.sender().len(), len_a);
        assert_eq!(pb.sender().len(), len_b);
    }
}

#[test]
fn uring_drop() {
    static DROP_COUNT: AtomicUsize = AtomicUsize::new(0);

    #[derive(Debug)]
    struct DropCounter(char);
    impl Drop for DropCounter {
        fn drop(&mut self) {
            DROP_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }

    struct CounterRing;

    impl UringSpec for CounterRing {
        type A = DropCounter;
        type B = DropCounter;
    }

    let input = std::iter::repeat_with(fastrand::alphabetic)
        .take(30)
        .collect::<Vec<_>>();

    let (mut pa, mut pb) = Builder::<CounterRing>::default().build();
    std::thread::scope(|cx| {
        cx.spawn(|| {
            for i in input.iter().copied().map(DropCounter) {
                if i.0.is_uppercase() {
                    pa.send(i).unwrap();
                } else {
                    _ = pa.recv();
                }
            }
            drop(pa);
        });
        cx.spawn(|| {
            for i in input.iter().copied().map(DropCounter) {
                if i.0.is_lowercase() {
                    pb.send(i).unwrap();
                } else {
                    _ = pb.recv();
                }
            }
            drop(pb);
        });
    });

    assert_eq!(DROP_COUNT.load(Ordering::Relaxed), input.len() * 2);
}

#[test]
fn uring_threaded() {
    let input = std::iter::repeat_with(fastrand::alphabetic)
        .take(30)
        .collect::<Vec<_>>();

    let (mut pa, mut pb) = Builder::<CharUring>::default().build();
    let (pa_finished, pb_finished) = (AtomicBool::new(false), AtomicBool::new(false));
    std::thread::scope(|cx| {
        cx.spawn(|| {
            let mut r = vec![];
            for i in input.iter().copied() {
                pa.send(i).unwrap();
                while let Some(i) = pa.recv() {
                    r.push(i);
                }
            }
            pa_finished.store(true, Ordering::Release);
            while !pb_finished.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            while let Some(i) = pa.recv() {
                r.push(i);
            }
            assert_eq!(r, input);
        });
        cx.spawn(|| {
            let mut r = vec![];
            for i in input.iter().copied() {
                pb.send(i).unwrap();
                while let Some(i) = pb.recv() {
                    r.push(i);
                }
            }
            pb_finished.store(true, Ordering::Release);
            while !pa_finished.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            while let Some(i) = pb.recv() {
                r.push(i);
            }
            assert_eq!(r, input);
        });
    });
}

#[test]
fn uring_threaded_bulk() {
    let input = std::iter::repeat_with(fastrand::alphabetic)
        .take(30)
        .collect::<Vec<_>>();

    let (mut pa, mut pb) = Builder::<CharUring>::default().build();
    let (pa_finished, pb_finished) = (AtomicBool::new(false), AtomicBool::new(false));
    std::thread::scope(|cx| {
        cx.spawn(|| {
            let mut r = vec![];
            pa.send_bulk(input.iter().copied());
            pa_finished.store(true, Ordering::Release);
            while !pb_finished.load(Ordering::Acquire) {
                r.extend(pa.recv_bulk());
                std::thread::yield_now();
            }
            r.extend(pa.recv_bulk());
            assert_eq!(r, input);
        });
        cx.spawn(|| {
            let mut r = vec![];
            pb.send_bulk(input.iter().copied());
            pb_finished.store(true, Ordering::Release);
            while !pa_finished.load(Ordering::Acquire) {
                r.extend(pb.recv_bulk());
                std::thread::yield_now();
            }
            r.extend(pb.recv_bulk());
            assert_eq!(r, input);
        });
    });
}
