use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::ops::Deref;
use core::panic::{RefUnwindSafe, UnwindSafe};
use core::ptr::{self};
use core::sync::atomic::AtomicUsize;

use crate::boxed::PBox;
use crate::channel::{Endpoint, Header, Queue, QueueRx, QueueTx, Slot, Slots};
use crate::mem::{MemAllocator, MetaSpanOf};
use crate::msg::Envelope;
use crate::reg::{AsEntry, EntryGuard, Finalize, Project, Resource};
use crate::token::{PackToken, TokenOf};

type Token<H, M> = PackToken<H, M>;
type Tokens<H, M> = Slots<Token<H, M>>;

type TokenOfTokens<H, M> = TokenOf<Tokens<H, M>, M>;

#[derive(Debug)]
#[repr(transparent)]
pub struct ViewOfQueue<H: Envelope, M> {
    ptr: ptr::NonNull<Tokens<H, M>>,
}

unsafe impl<H: Envelope, M> Send for ViewOfQueue<H, M> {}

impl<H: Envelope, M> const Deref for ViewOfQueue<H, M> {
    type Target = Tokens<H, M>;
    fn deref(&self) -> &Self::Target {
        // Safety: only view in TokenQueue context.
        unsafe { self.ptr.as_ref() }
    }
}

impl<H: Envelope, M> Clone for ViewOfQueue<H, M> {
    fn clone(&self) -> Self {
        Self { ptr: self.ptr }
    }
}

#[derive(Debug)]
pub struct ViewOfDuplex<H: Envelope, M> {
    left: ViewOfQueue<H, M>,
    right: ViewOfQueue<H, M>,
}

impl<H: Envelope, M> Clone for ViewOfDuplex<H, M> {
    fn clone(&self) -> Self {
        Self {
            left: self.left.clone(),
            right: self.right.clone(),
        }
    }
}

pub trait AsTokenQueue<H: Envelope, M>: AsEntry<TokenQueue<H, M>> {}
impl<H: Envelope, M, T: AsEntry<TokenQueue<H, M>>> AsTokenQueue<H, M> for T {}

type QueueView<H, M, E> = EntryGuard<E, TokenQueue<H, M>, ViewOfQueue<H, M>>;
type TokenQueueOf<H, A> = TokenQueue<H, MetaSpanOf<A>>;
pub struct TokenQueue<H: Envelope, M> {
    header: Header,
    buf: TokenOfTokens<H, M>,
}

unsafe impl<H: Send + Envelope, M> Send for TokenQueue<H, M> {}
unsafe impl<H: Send + Envelope, M> Sync for TokenQueue<H, M> {}

impl<H: Envelope, M> UnwindSafe for TokenQueue<H, M> {}
impl<H: Envelope, M> RefUnwindSafe for TokenQueue<H, M> {}

impl<H: Envelope, M> Finalize for TokenQueue<H, M> {
    fn finalize(&self) {
        // Recover the disconnection state for next preparation.
        self.header.open()
    }
}

impl<H: Envelope, A: MemAllocator> Resource<A> for TokenQueueOf<H, A> {
    type Config = usize;
    fn new(conf: Self::Config, ctx: A) -> (Self, A) {
        let cap = conf;
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
        let Self { header: _, buf } = s;
        let b = buf.detoken(alloc);
        PBox::drop_in(b)
    }
}

impl<H: Envelope, A: MemAllocator> Project<A> for TokenQueueOf<H, A> {
    type View = ViewOfQueue<H, MetaSpanOf<A>>;

    #[inline]
    fn project(&self, ctx: A) -> (Self::View, A) {
        let alloc = ctx;
        let (buf, alloc) = self.buf.as_ptr(alloc);
        (ViewOfQueue { ptr: buf }, alloc)
    }
}

impl<E: AsTokenQueue<H, M>, H: Envelope, M> Queue for QueueView<H, M, E> {
    type Item = Token<H, M>;

    #[inline]
    fn header(&self) -> &Header {
        &self.as_ref().header
    }

    #[inline]
    fn buf(&self) -> &Slots<Self::Item> {
        &self.view
    }
}

impl<E: AsTokenQueue<H, M>, H: Envelope, M> Endpoint for QueueView<H, M, E> {}

pub trait AsTokenDuplex<H: Envelope, M>: AsEntry<TokenDuplex<H, M>> {}
impl<H: Envelope, M, T: AsEntry<TokenDuplex<H, M>>> AsTokenDuplex<H, M> for T {}

pub type DuplexView<H, M, E> = EntryGuard<E, TokenDuplex<H, M>, ViewOfDuplex<H, M>>;
pub type LDuplexView<H, M, E> = Split<DuplexView<H, M, E>, Left>;
pub type RDuplexView<H, M, E> = Split<DuplexView<H, M, E>, Right>;
pub type Sender<H, M, E, R> = QueueTx<Split<DuplexView<H, M, E>, R>>;
pub type Receiver<H, M, E, R> = QueueRx<Split<DuplexView<H, M, E>, R>>;

pub type TokenDuplexOf<H, A> = TokenDuplex<H, MetaSpanOf<A>>;
pub struct TokenDuplex<H: Envelope, M> {
    left: TokenQueue<H, M>,
    right: TokenQueue<H, M>,
}

pub struct Left;
pub struct Right;

#[derive(PartialEq)]
pub struct Split<T, Role> {
    inner: T,
    _role: PhantomData<Role>,
}

impl<T: Clone, Role> Clone for Split<T, Role> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _role: PhantomData,
        }
    }
}

impl<H: Envelope, M> Finalize for TokenDuplex<H, M> {
    fn finalize(&self) {
        self.left.header.close()
    }
}

impl<H: Envelope, A: MemAllocator> Resource<A> for TokenDuplexOf<H, A> {
    type Config = usize;

    fn new(cfg: Self::Config, ctx: A) -> (Self, A) {
        let alloc = ctx;
        let (l, alloc) = TokenQueue::new(cfg, alloc);
        let (r, alloc) = TokenQueue::new(cfg, alloc);
        (Self { left: l, right: r }, alloc)
    }

    fn free(s: Self, ctx: A) -> A {
        let alloc = ctx;
        let Self { left: l, right: r } = s;
        let alloc = TokenQueue::free(l, alloc);
        TokenQueue::free(r, alloc)
    }
}

impl<H: Envelope, A: MemAllocator> Project<A> for TokenDuplexOf<H, A> {
    type View = ViewOfDuplex<H, MetaSpanOf<A>>;

    fn project(&self, ctx: A) -> (Self::View, A) {
        let alloc = ctx;
        let (left, alloc) = self.left.project(alloc);
        let (right, alloc) = self.right.project(alloc);
        (ViewOfDuplex { left, right }, alloc)
    }
}

impl<T, Role> const Deref for Split<T, Role> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<E: AsTokenDuplex<H, M>, H: Envelope, M> Queue for Split<DuplexView<H, M, E>, Left> {
    type Item = Token<H, M>;

    #[inline]
    fn header(&self) -> &Header {
        &self.as_ref().left.header
    }

    #[inline]
    fn buf(&self) -> &Slots<Self::Item> {
        &self.view.left
    }
}

impl<E: AsTokenDuplex<H, M>, H: Envelope, M> Endpoint for Split<DuplexView<H, M, E>, Left> {}

impl<E: AsTokenDuplex<H, M>, H: Envelope, M> Queue for Split<DuplexView<H, M, E>, Right> {
    type Item = Token<H, M>;

    #[inline]
    fn header(&self) -> &Header {
        &self.as_ref().right.header
    }

    #[inline]
    fn buf(&self) -> &Slots<Self::Item> {
        &self.view.right
    }
}

impl<E: AsTokenDuplex<H, M>, H: Envelope, M> Endpoint for Split<DuplexView<H, M, E>, Right> {}

impl<E: AsTokenDuplex<H, M> + Clone, H: Envelope, M> DuplexView<H, M, E> {
    fn split(duplex: Self) -> (LDuplexView<H, M, E>, RDuplexView<H, M, E>) {
        (
            LDuplexView {
                inner: duplex.clone(),
                _role: PhantomData,
            },
            RDuplexView {
                inner: duplex,
                _role: PhantomData,
            },
        )
    }
    pub fn lsplit(self) -> (Sender<H, M, E, Left>, Receiver<H, M, E, Right>) {
        let (l, r) = Self::split(self);
        (l.sender(), r.receiver())
    }

    pub fn rsplit(self) -> (Sender<H, M, E, Right>, Receiver<H, M, E, Left>) {
        let (l, r) = Self::split(self);
        (r.sender(), l.receiver())
    }
}
