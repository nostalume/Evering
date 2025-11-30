mod root {
    use core::ptr::NonNull;

    use super::channel::{MsgDuplex, MsgDuplexView};
    use crate::{
        arena::{self, ARENA_MAX_CAPACITY, Strategy, UInt, max_bound},
        mem::{self, AddrSpec, MemBlkHandle, MemBlkLayout, MemBlkOps, Mmap},
        msg::Envelope,
        numeric::CastInto,
        reg,
    };

    pub use crate::arena::{Config, Optimistic, Pessimistic};

    pub type Span = arena::SpanMeta;
    pub type Allocator<'a, S> = arena::Arena<'a, S>;
    pub type AllocHeader<S> = arena::Header<S>;

    #[derive(Clone)]
    pub struct ArenaMem<S: AddrSpec, M: Mmap<S>, G: Strategy> {
        m: MemBlkHandle<S, M>,
        alloc: NonNull<AllocHeader<G>>,
        size: UInt,
    }

    impl<S: AddrSpec, M: Mmap<S>, G: Strategy> TryFrom<MemBlkLayout<S, M>> for ArenaMem<S, M, G> {
        type Error = mem::Error<S, M>;

        fn try_from(area: MemBlkLayout<S, M>) -> Result<Self, Self::Error> {
            let mut area = area;
            let _ = area.push::<mem::Header>(())?;
            let alloc = area.push::<AllocHeader<G>>(AllocHeader::<G>::MIN_SEGMENT_SIZE)?;
            let (area, offset) = area.finish();

            let size = area.size() - offset;
            let size = max_bound(size).ok_or(mem::Error::OutofSize {
                requested: size,
                bound: ARENA_MAX_CAPACITY.cast_into(),
            })?;

            Ok(Self {
                m: area.into(),
                alloc,
                // Safety: Previous arithmetic check
                size,
            })
        }
    }

    impl<S: AddrSpec, M: Mmap<S>, G: Strategy> ArenaMem<S, M, G> {
        #[inline(always)]
        pub fn header(&self) -> &mem::Header {
            self.m.header()
        }

        #[inline(always)]
        fn alloc_header(&self) -> &AllocHeader<G> {
            unsafe { self.alloc.as_ref() }
        }

        #[inline(always)]
        pub fn arena_with(&self, conf: Config) -> Allocator<'_, G> {
            Allocator::from_header(self.alloc_header(), self.size, conf)
        }

        #[inline(always)]
        pub fn arena(&self) -> Allocator<'_, G> {
            let conf = Config::default();
            Allocator::from_header(self.alloc_header(), self.size, conf)
        }
    }

    #[derive(Clone)]
    pub struct Conn<S: AddrSpec, M: Mmap<S>, G: Strategy, H: Envelope, const N: usize> {
        m: MemBlkHandle<S, M>,
        alloc: NonNull<AllocHeader<G>>,
        reg: NonNull<reg::Registry<MsgDuplex<H>, N>>,
        size: UInt,
    }

    impl<S: AddrSpec, M: Mmap<S>, G: Strategy, H: Envelope, const N: usize>
        TryFrom<MemBlkLayout<S, M>> for Conn<S, M, G, H, N>
    {
        type Error = mem::Error<S, M>;
        fn try_from(area: MemBlkLayout<S, M>) -> Result<Self, Self::Error> {
            let mut area = area;
            let _ = area.push::<mem::Header>(())?;
            let reg = area.push::<reg::Registry<_, N>>(())?;
            let alloc = area.push::<AllocHeader<G>>(AllocHeader::<G>::MIN_SEGMENT_SIZE)?;
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
        pub fn header(&self) -> &mem::Header {
            self.m.header()
        }

        #[inline(always)]
        pub fn reg(&self) -> &reg::Registry<MsgDuplex<H>, N> {
            unsafe { self.reg.as_ref() }
        }

        #[inline(always)]
        fn alloc_header(&self) -> &AllocHeader<G> {
            unsafe { self.alloc.as_ref() }
        }

        #[inline(always)]
        pub fn arena_with(&self, conf: Config) -> Allocator<'_, G> {
            Allocator::from_header(self.alloc_header(), self.size, conf)
        }

        #[inline(always)]
        pub fn arena(&self) -> Allocator<'_, G> {
            let conf = Config::default();
            Allocator::from_header(self.alloc_header(), self.size, conf)
        }

        #[inline(always)]
        pub fn lookup(&self, idx: usize) -> Option<reg::Id> {
            self.reg().lookup(idx)
        }

        // #[inline(always)]
        // pub fn clear(&self, id: reg::Id) {
        //     self.reg().clear(id, self.arena())
        // }

        pub fn prepare(&self, cap: usize) -> Option<reg::Id> {
            let Ok((id, _)) = self.reg().prepare(cap, self.arena()) else {
                return None;
            };

            Some(id)
        }

        pub fn acquire(&self, id: reg::Id) -> Option<MsgDuplexView<'_, H, G>> {
            let (duplex, _) = self.reg().view(id, self.arena());
            duplex
        }
    }
}

pub mod allocator {
    pub use super::root::{Allocator, Config, Optimistic, Pessimistic};
    pub use crate::mem::{MemAlloc, MemAllocInfo, MemAllocator, MemBlkBuilder, MemDealloc};
}

pub mod channel {
    use super::root::{Allocator, Span};
    use crate::channel::cross;
    use crate::channel::{Receiver, Sender};
    use crate::msg::Envelope;
    use crate::token;

    pub type Token = token::Token<Span>;
    pub type MsgToken<H> = token::PackToken<H, Span>;

    pub type MsgQueue<H> = cross::TokenQueue<H, Span>;
    pub type MsgDuplex<H> = cross::TokenDuplex<H, Span>;
    pub type MsgDuplexView<'a, H, G> = cross::DuplexView<'a, H, Allocator<'a, G>, Span>;

    pub trait MsgSender<H: Envelope>: Sender<Item = MsgToken<H>> {}

    impl<H: Envelope, T: Sender<Item = MsgToken<H>>> MsgSender<H> for T {}
    pub trait MsgReceiver<H: Envelope>: Receiver<Item = MsgToken<H>> {}

    impl<H: Envelope, T: Receiver<Item = MsgToken<H>>> MsgReceiver<H> for T {}
}

pub use root::{ArenaMem, Conn};
