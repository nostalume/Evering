use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::ops::Deref;
use core::panic::{RefUnwindSafe, UnwindSafe};
use core::ptr;
use core::sync::atomic::AtomicUsize;

use crate::boxed::PBox;
use crate::channel::{Endpoint, Header, Queue, Rx, Slot, Slots, Tx};
use crate::malloc::{MemAllocator, MetaSpanOf};
use crate::msg::Envelope;
use crate::reg::{EntryView, Project, Resource};
use crate::token::{PackToken, TokenOf};

type Token<H, M> = PackToken<H, M>;
type Tokens<H, M> = Slots<Token<H, M>>;

type TokenOfTokens<H, M> = TokenOf<Tokens<H, M>, M>;
type ViewOfSlots<H, M> = ptr::NonNull<Tokens<H, M>>;

type QueueView<'a, H, Alloc, M> = EntryView<'a, Alloc, TokenQueue<H, M>>;

pub struct TokenQueue<H: Envelope, M> {
    header: Header,
    buf: TokenOfTokens<H, M>,
}

unsafe impl<H: Send + Envelope, M> Send for TokenQueue<H, M> {}
unsafe impl<H: Send + Envelope, M> Sync for TokenQueue<H, M> {}

impl<H: Envelope, M> UnwindSafe for TokenQueue<H, M> {}
impl<H: Envelope, M> RefUnwindSafe for TokenQueue<H, M> {}

impl<H: Envelope, A: MemAllocator> Resource<A> for TokenQueue<H, MetaSpanOf<A>> {
    type Config = usize;
    fn new(cfg: Self::Config, ctx: A) -> (Self, A) {
        let cap = cfg;
        let alloc = ctx;
        let header = Header::new(cap);
        let buffer: PBox<_, A> = PBox::new_slice_in(
            cap,
            |i| Slot {
                stamp: AtomicUsize::new(i),
                value: UnsafeCell::new(MaybeUninit::uninit()),
            },
            alloc,
        );
        let (buf, alloc) = buffer.token_of_with();
        (TokenQueue { header, buf }, alloc)
    }

    fn free(s: Self, ctx: A) -> A {
        let alloc = ctx;
        // let (view, alloc) = self.project(alloc);
        // struct DropView<'a, H: Envelope, A: MemAllocator> {
        //     h: &'a Header,
        //     view: ptr::NonNull<Tokens<H, A>>,
        // }

        // impl<H: Envelope, A: MemAllocator> Queue for DropView<'_, H, A> {
        //     type Item = SpanPackToken<H, A>;

        //     fn header(&self) -> &Header {
        //         self.h
        //     }

        //     fn buf(&self) -> &Slots<Self::Item> {
        //         unsafe { self.view.as_ref() }
        //     }
        // }

        // let drop_view = DropView::<'_, _, A> { h: &self.h, view };
        // unsafe { drop_view.drop_in() };
        let Self { header: _, buf } = s;
        let b = buf.detoken(alloc);
        PBox::drop_in(b)
    }
}

impl<H: Envelope, A: MemAllocator> Project<A> for TokenQueue<H, MetaSpanOf<A>> {
    type View = ViewOfSlots<H, MetaSpanOf<A>>;

    #[inline]
    fn project(&self, ctx: A) -> (Self::View, A) {
        let alloc = ctx;
        let (buf, alloc) = self.buf.as_ptr(alloc);
        (buf, alloc)
    }
}

impl<H: Envelope, A: MemAllocator> Queue for QueueView<'_, H, A, MetaSpanOf<A>> {
    type Item = Token<H, MetaSpanOf<A>>;

    #[inline]
    fn header(&self) -> &Header {
        &self.guard.as_ref().header
    }

    #[inline]
    fn buf(&self) -> &Slots<Self::Item> {
        unsafe { self.view.as_ref() }
    }
}

impl<'a, H: Envelope, A: MemAllocator> Endpoint for QueueView<'a, H, A, MetaSpanOf<A>> {}

pub type DuplexView<'a, H, A, M> = EntryView<'a, A, TokenDuplex<H, M>>;
pub struct TokenDuplex<H: Envelope, M> {
    l: TokenQueue<H, M>,
    r: TokenQueue<H, M>,
}

#[repr(transparent)]
#[derive(Clone, PartialEq)]
pub struct LEndpoint<T>(T);

#[repr(transparent)]
#[derive(Clone, PartialEq)]
pub struct REndpoint<T>(T);

impl<H: Envelope, A: MemAllocator> Resource<A> for TokenDuplex<H, MetaSpanOf<A>> {
    type Config = usize;

    fn new(cfg: Self::Config, ctx: A) -> (Self, A) {
        let alloc = ctx;
        let (l, alloc) = TokenQueue::new(cfg, alloc);
        let (r, alloc) = TokenQueue::new(cfg, alloc);
        (Self { l, r }, alloc)
    }

    fn free(s: Self, ctx: A) -> A {
        let alloc = ctx;
        let Self { l, r } = s;
        let alloc = TokenQueue::free(l, alloc);
        TokenQueue::free(r, alloc)
    }
}

impl<H: Envelope, A: MemAllocator> Project<A> for TokenDuplex<H, MetaSpanOf<A>> {
    type View = (ViewOfSlots<H, MetaSpanOf<A>>, ViewOfSlots<H, MetaSpanOf<A>>);

    fn project(&self, ctx: A) -> (Self::View, A) {
        let alloc = ctx;
        let (l, alloc) = self.l.project(alloc);
        let (r, alloc) = self.r.project(alloc);
        ((l, r), alloc)
    }
}

impl<T> Deref for LEndpoint<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a, H: Envelope, A: MemAllocator> Queue for LEndpoint<DuplexView<'a, H, A, MetaSpanOf<A>>> {
    type Item = Token<H, MetaSpanOf<A>>;

    #[inline]
    fn header(&self) -> &Header {
        &self.guard.as_ref().l.header
    }

    #[inline]
    fn buf(&self) -> &Slots<Self::Item> {
        unsafe { self.view.0.as_ref() }
    }
}

impl<'a, H: Envelope, A: MemAllocator> Endpoint for LEndpoint<DuplexView<'a, H, A, MetaSpanOf<A>>> {}

impl<T> Deref for REndpoint<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a, H: Envelope, A: MemAllocator> Queue for REndpoint<DuplexView<'a, H, A, MetaSpanOf<A>>> {
    type Item = Token<H, MetaSpanOf<A>>;

    #[inline]
    fn header(&self) -> &Header {
        &self.guard.as_ref().r.header
    }

    #[inline]
    fn buf(&self) -> &Slots<Self::Item> {
        unsafe { self.view.1.as_ref() }
    }
}

impl<'a, H: Envelope, A: MemAllocator> Endpoint for REndpoint<DuplexView<'a, H, A, MetaSpanOf<A>>> {}

impl<'a, H: Envelope, A: MemAllocator> DuplexView<'a, H, A, MetaSpanOf<A>> {
    pub fn sr_duplex(self) -> (Tx<LEndpoint<Self>>, Rx<REndpoint<Self>>) {
        let lq = LEndpoint(self.clone());
        let rq = REndpoint(self);
        (lq.sender(), rq.receiver())
    }

    pub fn rs_duplex(self) -> (Tx<REndpoint<Self>>, Rx<LEndpoint<Self>>) {
        let lq = LEndpoint(self.clone());
        let rq = REndpoint(self);
        (rq.sender(), lq.receiver())
    }
}
