use crate::layout::{alloc, alloc_buffer, dealloc, dealloc_buffer};
use crate::queue::{Drain, Offsets, Queue};
use core::fmt;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering};

#[non_exhaustive]
pub struct DisposeError {}

impl fmt::Debug for DisposeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DisposeError").finish_non_exhaustive()
    }
}

impl fmt::Display for DisposeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Uring is still connected")
    }
}

impl core::error::Error for DisposeError {}

pub(crate) trait UringSpec {
    type A;
    type B;
    type Ext = ();
}

pub(crate) trait Uring<S: UringSpec> {
    fn header(&self) -> &Header<S::Ext>;

    fn sender(&self) -> Queue<S::A>;

    fn receiver(&self) -> Queue<S::B>;

    fn ext(&self) -> &S::Ext
    where
        S::Ext: Sync,
    {
        &self.header().ext
    }

    /// Returns `true` if the remote [`Uring`] is not dropped.
    fn is_connected(&self) -> bool {
        self.header().rc.load(Ordering::Relaxed) > 1
    }

    fn send(&mut self, val: S::A) -> Result<(), S::A> {
        unsafe { self.sender().enqueue(val) }
    }

    fn send_bulk<I>(&mut self, vals: I) -> usize
    where
        I: Iterator<Item = S::A>,
    {
        unsafe { self.sender().enqueue_bulk(vals) }
    }

    fn recv(&mut self) -> Option<S::B> {
        unsafe { self.receiver().dequeue() }
    }

    fn recv_bulk(&mut self) -> Drain<S::B> {
        unsafe { self.receiver().dequeue_bulk() }
    }
}

// pub(crate) enum UringEither<S: UringSpec> {
//     A(UringA<S>),
//     B(UringB<S>),
// }

// impl<S: UringSpec> Uring for UringEither<S> {
//     fn header(&self) -> &Header<S::Ext> {
//         match self {
//             UringEither::A(a) => a.header(),
//             UringEither::B(b) => b.header(),
//         }
//     }

//     fn sender(&self) -> Queue<S::T> {
//         match self {
//             UringEither::A(a) => a.sender(),
//             UringEither::B(b) => b.sender(),
//         }
//     }

//     fn receiver(&self) -> Queue<T> {
//         match self {
//             UringEither::A(a) => a.receiver(),
//             UringEither::B(b) => b.receiver(),
//         }
//     }
// }

pub type Sender<S: UringSpec> = UringA<S>;
pub type Receiver<S: UringSpec> = UringB<S>;

unsafe impl<S:UringSpec> Send for UringA<S> where S::A: Send, S::B:Send, S::Ext: Send{}
unsafe impl<S:UringSpec> Send for UringB<S> where S::A: Send, S::B:Send, S::Ext: Send{}

pub(crate) struct UringA<S: UringSpec>(RawUring<S>);
pub(crate) struct UringB<S: UringSpec>(RawUring<S>);

// macro_rules! common_methods {
//     ($A:ident, $B:ident, $Ext:ident) => {
//         pub fn into_raw(self) -> RawUring<A, B, Ext> {
//             let inner = RawUring {
//                 header: self.0.header,
//                 buf_a: self.0.buf_a,
//                 buf_b: self.0.buf_b,
//                 marker: PhantomData,
//             };
//             core::mem::forget(self);
//             inner
//         }

//         /// Drops this [`Uring`] and all enqueued entries.
//         ///
//         /// It does nothing and returns an error if `self` is still connected.
//         /// Otherwise, the returned [`RawUring`] is safe to deallocate without
//         /// synchronization.
//         pub fn dispose_raw(self) -> Result<RawUring<A, B, Ext>, DisposeError> {
//             let mut raw = self.into_raw();
//             unsafe {
//                 match raw.dispose() {
//                     Ok(_) => Ok(raw),
//                     Err(e) => Err(e),
//                 }
//             }
//         }

//         /// # Safety
//         ///
//         /// The specified [`RawUring`] must be a valid value returned from
//         /// [`into_raw`](Self::into_raw).
//         pub unsafe fn from_raw(uring: RawUring<A, B, Ext>) -> Self {
//             Self(uring)
//         }
//     };
// }

// impl<A, B, Ext> UringA<A, B, Ext> {
//     common_methods!(A, B, Ext);
// }

// impl<A, B, Ext> UringB<A, B, Ext> {
//     common_methods!(A, B, Ext);
// }

impl<S: UringSpec> Deref for UringB<S> {
    type Target = RawUring<S>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl<S: UringSpec> DerefMut for UringB<S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<S: UringSpec> Deref for UringA<S> {
    type Target = RawUring<S>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl<S: UringSpec> DerefMut for UringA<S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<S: UringSpec> Drop for UringA<S> {
    fn drop(&mut self) {
        unsafe { self.0.drop_in_place() }
    }
}

impl<S: UringSpec> Drop for UringB<S> {
    fn drop(&mut self) {
        unsafe { self.0.drop_in_place() }
    }
}

pub struct Header<Ext = ()> {
    off_a: Offsets,
    off_b: Offsets,
    rc: AtomicU32,
    ext: Ext,
}

impl<Ext> Header<Ext> {
    pub fn size_a(&self) -> usize {
        self.off_a.ring_mask as usize + 1
    }

    pub fn size_b(&self) -> usize {
        self.off_b.ring_mask as usize + 1
    }
}

pub(crate) struct RawUring<S: UringSpec> {
    pub header: NonNull<Header<S::Ext>>,
    pub buf_a: NonNull<S::A>,
    pub buf_b: NonNull<S::B>,
    _marker: PhantomData<fn(S)>,
}

impl<S: UringSpec> Uring<S> for RawUring<S> {
    fn header(&self) -> &Header<S::Ext> {
        unsafe { self.header() }
    }

    fn sender(&self) -> Queue<S::A> {
        unsafe { self.queue_a() }
    }

    fn receiver(&self) -> Queue<S::B> {
        unsafe { self.queue_b() }
    }
}

impl<S: UringSpec> RawUring<S> {
    pub const fn dangling() -> Self {
        Self {
            header: NonNull::dangling(),
            buf_a: NonNull::dangling(),
            buf_b: NonNull::dangling(),
            _marker: PhantomData,
        }
    }

    unsafe fn header(&self) -> &Header<S::Ext> {
        unsafe { self.header.as_ref() }
    }

    unsafe fn queue_a(&self) -> Queue<'_, S::A> {
        Queue {
            off: unsafe { &self.header().off_a },
            buf: self.buf_a,
        }
    }

    unsafe fn queue_b(&self) -> Queue<'_, S::B> {
        Queue {
            off: unsafe { &self.header().off_b },
            buf: self.buf_b,
        }
    }

    unsafe fn dispose(&mut self) -> Result<(), DisposeError> {
        let rc = unsafe { &self.header().rc };
        debug_assert!(rc.load(Ordering::Relaxed) >= 1);
        // `Release` enforeces any use of the data to happen before here.
        if rc.fetch_sub(1, Ordering::Release) != 1 {
            return Err(DisposeError {});
        }
        // `Acquire` enforces the deletion of the data to happen after here.
        core::sync::atomic::fence(Ordering::Acquire);

        unsafe {
            self.queue_a().drop_in_place();
            self.queue_b().drop_in_place();
        }
        Ok(())
    }

    unsafe fn drop_in_place(&mut self) {
        unsafe {
            if self.dispose().is_ok() {
                let h = self.header.as_ref();
                dealloc_buffer(self.buf_a, h.off_a.ring_mask as usize + 1);
                dealloc_buffer(self.buf_b, h.off_b.ring_mask as usize + 1);
                dealloc(self.header);
            }
        }
    }
}

struct Builder<S: UringSpec> {
    size_a: usize,
    size_b: usize,
    ext: S::Ext,
}

impl<S: UringSpec> Builder<S> {
    const SIZE_A: usize = 32;
    const SIZE_B: usize = 32;
    pub fn new() -> Self
    where
        S::Ext: Default,
    {
        Self::new_ext(S::Ext::default())
    }

    pub fn new_ext(ext: S::Ext) -> Self {
        Self {
            size_a: Self::SIZE_A,
            size_b: Self::SIZE_B,
            ext,
        }
    }

    pub fn size_a(&mut self, size: usize) -> &mut Self {
        assert!(size.is_power_of_two());
        self.size_a = size;
        self
    }

    pub fn size_b(&mut self, size: usize) -> &mut Self {
        assert!(size.is_power_of_two());
        self.size_b = size;
        self
    }

    pub fn build_header(self) -> Header<S::Ext> {
        Header {
            off_a: Offsets::new(self.size_a as u32),
            off_b: Offsets::new(self.size_b as u32),
            rc: AtomicU32::new(2),
            ext: self.ext,
        }
    }

    pub fn build(self) -> (UringA<S>, UringB<S>) {
        let header;
        let buf_a;
        let buf_b;

        unsafe {
            header = alloc::<Header<S::Ext>>();
            buf_a = alloc_buffer(self.size_a);
            buf_b = alloc_buffer(self.size_b);

            header.write(self.build_header());
        }

        let ring_a = UringA(RawUring {
            header,
            buf_a,
            buf_b,
            _marker: PhantomData,
        });
        let ring_b = UringB(RawUring {
            header,
            buf_a,
            buf_b,
            _marker: PhantomData,
        });

        (ring_a, ring_b)
    }
}

impl<S: UringSpec> Default for Builder<S>
where
    S::Ext: Default,
{
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    use super::*;
    struct VoidUring;
    impl UringSpec for VoidUring {
        type A = ();
        type B = ();
    }

    struct CharRing;
    impl UringSpec for CharRing {
        type A = char;
        type B = char;
    }

    #[test]
    fn queue_len() {
        let mut len_a = 0;
        let mut len_b = 0;
        let (mut pa, mut pb) = Builder::<VoidUring>::new().build();
        for _ in 0..32 {
            match fastrand::u8(0..4) {
                0 => len_a += pa.send(()).map_or(0, |_| 1),
                1 => len_b += pb.send(()).map_or(0, |_| 1),
                2 => len_a -= pb.recv().map_or(0, |_| 1),
                3 => len_b -= pa.recv().map_or(0, |_| 1),
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

        let (mut pa, mut pb) = Builder::<CounterRing>::new().build();
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

        let (mut pa, mut pb) = Builder::<CharRing>::new().build();
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

        let (mut pa, mut pb) = Builder::<CharRing>::new().build();
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
}
