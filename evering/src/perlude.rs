mod root {
    use core::ptr::NonNull;

    use super::channel::{MsgDuplex, MsgDuplexView};
    use crate::{
        arena::{self, ARENA_MAX_CAPACITY, Strategy, UInt, max_bound},
        mem::{self, AddrSpec, MemBlkHandle, MemBlkLayout, MemBlkOps, MemRef, Mmap},
        msg::{Envelope, Operation},
        numeric::{CastInto, Id},
        reg,
    };

    pub use crate::arena::{ArenaRef, Config, MemArena, Optimistic, Pessimistic};

    pub type Meta = arena::Meta;
    pub type Span = arena::SpanMeta;
    pub type Allocator<H, S> = arena::Arena<H, S>;
    pub type AllocHeader<S> = arena::Header<S>;

    pub type AConn<S, M, G, H, const N: usize> = Conn<S, M, G, Operation<H>, N>;

    #[derive(Clone)]
    pub struct Conn<S: AddrSpec, M: Mmap<S>, G: Strategy, H: Envelope, const N: usize> {
        m: MemBlkHandle<S, M>,
        alloc: NonNull<AllocHeader<G>>,
        reg: NonNull<reg::Header<MsgDuplex<H>, N>>,
        size: UInt,
    }

    impl<S: AddrSpec, M: Mmap<S>, G: Strategy, H: Envelope, const N: usize>
        TryFrom<MemBlkLayout<S, M>> for Conn<S, M, G, H, N>
    {
        type Error = mem::Error<S, M>;
        fn try_from(area: MemBlkLayout<S, M>) -> Result<Self, Self::Error> {
            let mut area = area;
            let reg = area.push::<reg::Header<_, N>>(())?;
            let conf = arena::MetaConfig::default::<G>();
            let alloc = area.push::<AllocHeader<G>>(conf)?;
            let (area, offset) = area.finish();

            let size = area.size() - offset;
            let size = max_bound(size).ok_or(mem::Error::OutofSize {
                requested: size,
                bound: ARENA_MAX_CAPACITY.cast_into(),
            })?;

            Ok(Self {
                m: area.into(),
                alloc,
                reg,
                // Safety: Previous arithmetic check
                size,
            })
        }
    }

    impl<S: AddrSpec, M: Mmap<S>, G: Strategy, H: Envelope, const N: usize> Conn<S, M, G, H, N> {
        #[inline(always)]
        pub fn header(&self) -> &mem::RcHeader {
            self.m.header()
        }

        #[inline(always)]
        pub fn reg(&self) -> &reg::Header<MsgDuplex<H>, N> {
            unsafe { self.reg.as_ref() }
        }

        #[inline]
        fn arena_header_ref(&self) -> &AllocHeader<G> {
            unsafe { self.alloc.as_ref() }
        }

        #[inline]
        fn arena_header(&self) -> MemRef<AllocHeader<G>, S, M> {
            unsafe { MemRef::from_raw(self.m.clone(), self.alloc) }
        }

        #[inline(always)]
        pub fn lookup(&self, idx: usize) -> Option<Id> {
            self.reg().lookup(idx)
        }

        // #[inline(always)]
        // pub fn clear(&self, id: reg::Id) {
        //     self.reg().clear(id, self.arena())
        // }

        pub fn prepare(&self, cap: usize) -> Option<Id> {
            let Ok((id, _)) = self.reg().prepare(cap, self.arena()) else {
                return None;
            };

            Some(id)
        }

        pub fn acquire(&self, id: Id) -> Option<MsgDuplexView<'_, H, &AllocHeader<G>, G>> {
            let (duplex, _) = self.reg().view(id, self.arena_ref());
            duplex
        }
    }
}

pub mod allocator {
    use super::root::Meta;
    use crate::mem;

    pub use super::root::{ArenaRef, Config, MemArena, Optimistic, Pessimistic};
    pub use crate::mem::{MemAllocInfo, MemBlkBuilder};

    pub trait MemAllocator: mem::MemAllocator<Meta = Meta> {}
    impl<T: mem::MemAllocator<Meta = Meta>> MemAllocator for T {}
}

pub mod channel {
    use super::root::{Allocator, Span};
    use crate::channel::cross;
    use crate::channel::driver;
    use crate::channel::{Receiver, Sender};
    use crate::msg::Envelope;
    use crate::token;

    pub type Token = token::Token<Span>;
    pub type MsgToken<H> = token::PackToken<H, Span>;

    pub type MsgQueue<H> = cross::TokenQueue<H, Span>;
    pub type MsgDuplex<H> = cross::TokenDuplex<H, Span>;
    pub type MsgDuplexView<'a, H, A, G> = cross::DuplexView<'a, H, Allocator<A, G>, Span>;

    pub trait MsgSender<H: Envelope>: Sender<Item = MsgToken<H>> {}

    impl<H: Envelope, T: Sender<Item = MsgToken<H>>> MsgSender<H> for T {}
    pub trait MsgReceiver<H: Envelope>: Receiver<Item = MsgToken<H>> {}

    impl<H: Envelope, T: Receiver<Item = MsgToken<H>>> MsgReceiver<H> for T {}

    pub type CachePool<H, const N: usize> = driver::CachePoolHandle<MsgToken<H>, N>;

    pub trait MsgSubmitter<H: Envelope>: driver::Submitter<H, Span> {}

    impl<H: Envelope, T: driver::Submitter<H, Span>> MsgSubmitter<H> for T {}

    pub trait MsgCompleter<H: Envelope>: driver::Completer<H, Span> {}

    impl<H: Envelope, T: driver::Completer<H, Span>> MsgCompleter<H> for T {}
}

pub use root::{AConn, Conn};
