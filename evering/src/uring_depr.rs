use core::alloc::{Layout, LayoutError};
use core::fmt;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::ptr::NonNull;
use core::sync::atomic::{self, AtomicU32, Ordering};

mod queue;
mod tests;

use queue::{Pow2, Queue, Range, Slot};

pub trait UringSpec {
    type A;
    type B;
    type Ext = ();

    fn layout(size_a: usize, size_b: usize) -> Result<(Layout, usize, usize), LayoutError> {
        let layout_header = Layout::new::<Header<Self::Ext>>();
        let layout_a = Layout::array::<Slot<Self::A>>(size_a)?;
        let layout_b = Layout::array::<Slot<Self::B>>(size_b)?;

        let (comb_ha, off_a) = layout_header.extend(layout_a)?;
        let (comb_hab, off_b) = comb_ha.extend(layout_b)?;

        let comb_hab = comb_hab.pad_to_align();

        Ok((comb_hab, off_a, off_b))
    }
}

pub trait UringSender {
    type T;
    fn sender(&mut self) -> Queue<'_, Self::T>;
    fn send(&mut self, val: Self::T) -> Result<(), Self::T> {
        self.sender().enqueue(val)
    }
    fn send_bulk(&mut self, val: impl Iterator<Item = Self::T>) -> usize {
        self.sender().enqueue_bulk(val)
    }
}

pub trait UringReceiver {
    type T;
    fn receiver(&mut self) -> Queue<'_, Self::T>;
    fn recv(&mut self) -> Option<Self::T> {
        self.receiver().dequeue()
    }
}

pub struct UringA<S: UringSpec>(RawUring<S>);
pub struct UringB<S: UringSpec>(RawUring<S>);

unsafe impl<S: UringSpec> Send for UringA<S>
where
    S::A: Send,
    S::B: Send,
    S::Ext: Send,
{
}
unsafe impl<S: UringSpec> Send for UringB<S>
where
    S::A: Send,
    S::B: Send,
    S::Ext: Send,
{
}

impl<S: UringSpec> UringSender for UringA<S> {
    type T = S::A;

    fn sender(&mut self) -> Queue<'_, Self::T> {
        self.queue_a()
    }
}

impl<S: UringSpec> UringSender for UringB<S> {
    type T = S::B;

    fn sender(&mut self) -> Queue<'_, Self::T> {
        self.queue_b()
    }
}

impl<S: UringSpec> UringReceiver for UringA<S> {
    type T = S::B;

    fn receiver(&mut self) -> Queue<'_, Self::T> {
        self.queue_b()
    }
}

impl<S: UringSpec> UringReceiver for UringB<S> {
    type T = S::A;

    fn receiver(&mut self) -> Queue<'_, Self::T> {
        self.queue_a()
    }
}

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
    range_a: Range,
    range_b: Range,
    rc: AtomicU32,
    ext: Ext,
}

impl<Ext> Header<Ext> {
    #[inline(always)]
    pub fn cap_a(&self) -> usize {
        self.range_a.capacity()
    }

    #[inline(always)]
    pub fn cap_b(&self) -> usize {
        self.range_b.capacity()
    }

    #[inline(always)]
    pub fn len_a(&self) -> usize {
        self.range_a.len()
    }

    #[inline(always)]
    pub fn len_b(&self) -> usize {
        self.range_b.len()
    }

    /// Returns `true` if the remote [`Uring`] is not dropped.
    #[inline(always)]
    pub fn is_connected(&self) -> bool {
        self.rc.load(Ordering::Relaxed) > 1
    }
}

#[derive(Debug)]
pub struct RawUring<S: UringSpec> {
    header: NonNull<Header<S::Ext>>,
    buf_a: NonNull<Slot<S::A>>,
    buf_b: NonNull<Slot<S::B>>,
}

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

impl<S: UringSpec> RawUring<S> {
    #[inline(always)]
    pub fn header(&self) -> &Header<S::Ext> {
        // Safety: The header is always initiated.
        unsafe { self.header.as_ref() }
    }

    fn header_mut(&mut self) -> &mut Header<S::Ext> {
        // Safety: The header is always initiated.
        unsafe { self.header.as_mut() }
    }

    #[inline(always)]
    pub fn is_connected(&self) -> bool {
        self.header().is_connected()
    }

    pub fn queue_a(&mut self) -> Queue<'_, S::A> {
        let buf = self.buf_a;
        unsafe { Queue::init(&mut self.header_mut().range_a, buf) }
    }

    pub fn queue_b(&mut self) -> Queue<'_, S::B> {
        let buf = self.buf_b;
        unsafe { Queue::init(&mut self.header_mut().range_b, buf) }
    }

    fn drop_queues(&mut self) -> Result<(), DisposeError> {
        let rc = &self.header().rc;
        debug_assert!(rc.load(Ordering::Relaxed) >= 1);

        // `Release` enforeces any use of the data to happen before here.
        if rc.fetch_sub(1, Ordering::Release) != 1 {
            return Err(DisposeError {});
        }
        // `Acquire` enforces the deletion of the data to happen after here.
        atomic::fence(Ordering::Acquire);

        // Safety: the queue is owned by sole instance.
        unsafe {
            self.queue_a().drop_elems();
            self.queue_b().drop_elems();
        };
        Ok(())
    }

    unsafe fn drop_in_place(&mut self) {
        unsafe {
            if self.drop_queues().is_ok() {
                // Safety: the initiated data indicates the layout never overflows.
                let (ly, _, _) = S::layout(self.header().cap_a(), self.header().cap_b()).unwrap();
                alloc::alloc::dealloc(self.header.as_ptr().cast(), ly);
            }
        }
    }
}

impl<S: UringSpec> Clone for RawUring<S> {
    fn clone(&self) -> Self {
        let hd = self.header();
        hd.rc.fetch_add(1, Ordering::Release);

        Self {
            header: self.header.clone(),
            buf_a: self.buf_a.clone(),
            buf_b: self.buf_b.clone(),
        }
    }
}

impl<S: UringSpec> Default for RawUring<S>
where
    S::Ext: Default,
{
    fn default() -> Self {
        let builder = Builder::default();
        builder
            .init(S::Ext::default())
            .expect("[Uring]: Initiation of RawUring failed.")
    }
}

struct Builder<S: UringSpec> {
    cap_a: Pow2,
    cap_b: Pow2,
    _marker: PhantomData<S>,
}

impl<S: UringSpec> Builder<S> {
    const CAP_A: Pow2 = Pow2::new(5);
    const CAP_B: Pow2 = Pow2::new(5);

    pub fn new(cap_a: Pow2, cap_b: Pow2) -> Self {
        Self {
            cap_a,
            cap_b,
            _marker: PhantomData,
        }
    }

    pub fn init_header(self, ext: S::Ext) -> Header<S::Ext> {
        Header {
            range_a: Range::new(self.cap_a),
            range_b: Range::new(self.cap_b),
            rc: AtomicU32::new(1),
            ext,
        }
    }

    fn init(self, ext: S::Ext) -> Result<RawUring<S>, LayoutError> {
        // Safety: the layout is ensured by `Pow2` to avoid overflow.
        let (comb_hab, off_a, off_b) = S::layout(self.cap_a.as_usize(), self.cap_b.as_usize())?;
        // Safety: the alloc memory is obviously non-null
        // unless it is out of memory as shut down.
        let ptr = unsafe {
            NonNull::new(alloc::alloc::alloc(comb_hab))
                .unwrap_or_else(|| alloc::alloc::handle_alloc_error(comb_hab))
        };

        let header = ptr.cast::<Header<S::Ext>>();
        // Safety: the new alloc memory with perscribed offset.
        unsafe { header.write(self.init_header(ext)) };
        let buf_a = unsafe { ptr.add(off_a).cast::<Slot<S::A>>() };
        let buf_b = unsafe { ptr.add(off_b).cast::<Slot<S::B>>() };

        Ok(RawUring {
            header,
            buf_a,
            buf_b,
        })
    }

    pub fn try_build_ext(self, ext: S::Ext) -> Result<(UringA<S>, UringB<S>), LayoutError> {
        let ru = self.init(ext)?;
        let ra = UringA(ru.clone());
        let rb = UringB(ru);

        Ok((ra, rb))
    }

    pub fn build_ext(self, ext: S::Ext) -> (UringA<S>, UringB<S>) {
        let ru = self
            .init(ext)
            .expect("[Uring]: Initiation of Uring failed.");
        let ra = UringA(ru.clone());
        let rb = UringB(ru);

        (ra, rb)
    }

    pub fn build(self) -> (UringA<S>, UringB<S>)
    where
        S::Ext: Default,
    {
        let ru = self
            .init(S::Ext::default())
            .expect("[Uring]: Initiation of Uring failed.");
        let ra = UringA(ru.clone());
        let rb = UringB(ru);

        (ra, rb)
    }
}

impl<S: UringSpec> Default for Builder<S> {
    fn default() -> Self {
        Self::new(Self::CAP_A, Self::CAP_B)
    }
}
