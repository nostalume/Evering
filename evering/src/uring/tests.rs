use super::UringSpec;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
struct CharUring;
impl UringSpec for CharUring {
    type SQE = char;
    type CQE = char;
}

#[test]
fn bare_inspect() {
    use super::bare::channel;
    let mut len_a = 0;
    let mut len_b = 0;
    let (pa,pb) = channel::<CharUring,16>();    

    for _ in 0..32 {
        let ch = fastrand::alphabetic();
        match fastrand::u8(0..4) {
            0 => len_a += pa.try_send(ch).map_or(0, |_| 1),
            1 => len_b += pb.try_send(ch).map_or(0, |_| 1),
            2 => {
                if let Some(ch) = pa.try_recv() {
                    dbg!(format!("A recv: {}", ch));
                    len_b -= 1;
                }
            }
            3 => {
                if let Some(ch) = pb.try_recv() {
                    dbg!(format!("B recv: {}", ch));
                    len_a -= 1;
                }
            }
            _ => unreachable!(),
        }
    }
    dbg!(format!("{}, {}",len_a,len_b));
}

#[test]
fn bare_drop() {
    use super::bare::channel;
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
        type SQE = DropCounter;
        type CQE = DropCounter;
    }

    let input = std::iter::repeat_with(fastrand::alphabetic)
        .take(30)
        .collect::<Vec<_>>();
    let (pa, pb) = channel::<CounterRing,16>();

    std::thread::scope(|cx| {
        cx.spawn(|| {
            for i in input.iter().copied().map(DropCounter) {
                if i.0.is_uppercase() {
                    _ = pa.try_send(i);
                } else {
                    _ = pa.try_recv();
                }
                std::thread::yield_now();
            }
            drop(pa);
        });
        cx.spawn(|| {
            for i in input.iter().copied().map(DropCounter) {
                if i.0.is_lowercase() {
                    _ = pb.try_send(i);
                } else {
                    _ = pb.try_recv();
                }
                std::thread::yield_now();
            }
            drop(pb);
        });
    });

    assert_eq!(DROP_COUNT.load(Ordering::Relaxed), input.len() * 2);
}

#[test]
fn uring_inspect() {
    use super::sync::default_channel;

    let mut len_a = 0;
    let mut len_b = 0;
    let (pa, pb) = default_channel::<CharUring>();

    for _ in 0..32 {
        let ch = fastrand::alphabetic();
        match fastrand::u8(0..4) {
            0 => len_a += pa.sender().send(ch).map_or(0, |_| 1),
            1 => len_b += pb.sender().send(ch).map_or(0, |_| 1),
            2 => {
                if let Ok(ch) = pa.receiver().try_recv() {
                    dbg!(format!("A recv: {}", ch));
                    len_b -= 1;
                }
            }
            3 => {
                if let Ok(ch) = pb.receiver().try_recv() {
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
    use super::sync::default_channel;
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
        type SQE = DropCounter;
        type CQE = DropCounter;
    }

    let input = std::iter::repeat_with(fastrand::alphabetic)
        .take(30)
        .collect::<Vec<_>>();
    let (pa, pb) = default_channel::<CounterRing>();

    std::thread::scope(|cx| {
        cx.spawn(|| {
            for i in input.iter().copied().map(DropCounter) {
                if i.0.is_uppercase() {
                    _ = pa.sender().try_send(i);
                } else {
                    _ = pa.receiver().try_recv();
                }
                std::thread::yield_now();
            }
            drop(pa);
        });
        cx.spawn(|| {
            for i in input.iter().copied().map(DropCounter) {
                if i.0.is_lowercase() {
                    _ = pb.sender().try_send(i);
                } else {
                    _ = pb.receiver().try_recv();
                }
                std::thread::yield_now();
            }
            drop(pb);
        });
    });

    assert_eq!(DROP_COUNT.load(Ordering::Relaxed), input.len() * 2);
}

#[test]
fn uring_threaded() {
    use crate::uring::sync::default_channel;
    let input = std::iter::repeat_with(fastrand::alphabetic)
        .take(30)
        .collect::<Vec<_>>();

    let (pa, pb) = default_channel::<CharUring>();
    let (pa_finished, pb_finished) = (AtomicBool::new(false), AtomicBool::new(false));
    std::thread::scope(|cx| {
        cx.spawn(|| {
            let mut r = vec![];
            for i in input.iter().copied() {
                pa.sender().send(i).unwrap();
                while let Ok(i) = pa.receiver().try_recv() {
                    r.push(i);
                }
            }
            pa_finished.store(true, Ordering::Release);
            while !pb_finished.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            while let Ok(i) = pa.receiver().try_recv() {
                r.push(i);
            }
            assert_eq!(r, input);
        });
        cx.spawn(|| {
            let mut r = vec![];
            for i in input.iter().copied() {
                pb.sender().send(i).unwrap();
                while let Ok(i) = pb.receiver().try_recv() {
                    r.push(i);
                }
            }
            pb_finished.store(true, Ordering::Release);
            while !pa_finished.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            while let Ok(i) = pb.receiver().try_recv() {
                r.push(i);
            }
            assert_eq!(r, input);
        });
    });
}
