use core::ptr::NonNull;

pub use crate::arena::{Optimistic, Pessimistic};

use crate::{
    area::{self, AddrSpec, MemBlkHandle, Mmap, RawMemBlk},
    arena::{self, ARENA_MAX_CAPACITY, Arena, Strategy, UInt, max_bound},
    channel::cross::{DuplexView, TokenDuplex},
    msg::{self, Envelope},
    numeric::CastInto,
    reg,
};

pub mod allocator {
    pub use crate::arena::{Arena, Optimistic, Pessimistic};
    pub use crate::malloc::{MemAlloc, MemAllocInfo, MemAllocator, MemDealloc};
}

pub mod channel {
    use crate::channel::{Receiver, Sender};
    use crate::msg::Envelope;

    pub use crate::channel::cross::{TokenDuplex, TokenQueue};

    type Span = crate::arena::SpanMeta;

    pub type Token = crate::token::Token<Span>;
    pub type MsgToken<H> = crate::token::PackToken<H, Span>;
    pub trait MsgSender<H: Envelope>: Sender<Item = MsgToken<H>> {}

    impl<H: Envelope, T: Sender<Item = MsgToken<H>>> MsgSender<H> for T {}
    pub trait MsgReceiver<H: Envelope>: Receiver<Item = MsgToken<H>> {}

    impl<H: Envelope, T: Receiver<Item = MsgToken<H>>> MsgReceiver<H> for T {}
}

#[derive(Clone)]
pub struct ArenaMem<S: AddrSpec, M: Mmap<S>, G: Strategy> {
    m: MemBlkHandle<S, M>,
    alloc: NonNull<arena::Header<G>>,
    size: UInt,
}

impl<S: AddrSpec, M: Mmap<S>, G: Strategy> TryFrom<RawMemBlk<S, M>> for ArenaMem<S, M, G> {
    type Error = area::Error<S, M>;

    fn try_from(area: RawMemBlk<S, M>) -> Result<Self, Self::Error> {
        unsafe {
            let (_, hoffset) = area.init_header::<area::Header>(0, ())?;
            let (alloc, aoffset) = area.init_header::<crate::arena::Header<G>>(
                hoffset,
                crate::arena::Header::<G>::MIN_SEGMENT_SIZE,
            )?;

            let size = area.size() - aoffset;
            let size = max_bound(size).ok_or(area::Error::OutofSize {
                requested: size,
                bound: ARENA_MAX_CAPACITY.cast_into(),
            })?;

            Ok(Self {
                m: area::MemBlk::from_raw(area).into(),
                alloc,
                // Safety: Previous arithmetic check
                size,
            })
        }
    }
}

impl<S: AddrSpec, M: Mmap<S>, G: Strategy> ArenaMem<S, M, G> {
    #[inline(always)]
    pub fn header(&self) -> &area::Header {
        self.m.header()
    }

    #[inline(always)]
    fn alloc_header(&self) -> &arena::Header<G> {
        unsafe { self.alloc.as_ref() }
    }

    #[inline(always)]
    pub fn arena_with(&self, conf: arena::Config) -> arena::Arena<'_, G> {
        arena::Arena::from_header(self.alloc_header(), self.size, conf)
    }

    #[inline(always)]
    pub fn arena(&self) -> arena::Arena<'_, G> {
        let conf = arena::Config::default();
        arena::Arena::from_header(self.alloc_header(), self.size, conf)
    }
}

pub type ArenaDuplex<H> = TokenDuplex<H, arena::SpanMeta>;
pub type ArenaDuplexView<'a, G, H> = DuplexView<'a, H, Arena<'a, G>, arena::SpanMeta>;
// A temporory design!
pub trait SessionIn {
    type Strategy: arena::Strategy;
    type TokenHeader: msg::Envelope;
}

pub type Session<S, M, const N: usize, Conf> =
    Conn<S, M, <Conf as SessionIn>::Strategy, <Conf as SessionIn>::TokenHeader, N>;

#[derive(Clone)]
pub struct Conn<S: AddrSpec, M: Mmap<S>, G: Strategy, H: Envelope, const N: usize> {
    m: MemBlkHandle<S, M>,
    alloc: NonNull<arena::Header<G>>,
    reg: NonNull<reg::Registry<ArenaDuplex<H>, N>>,
    size: UInt,
}

impl<S: AddrSpec, M: Mmap<S>, G: Strategy, H: Envelope, const N: usize> TryFrom<RawMemBlk<S, M>>
    for Conn<S, M, G, H, N>
{
    type Error = area::Error<S, M>;
    fn try_from(area: RawMemBlk<S, M>) -> Result<Self, Self::Error> {
        unsafe {
            let (_, hoffset) = area.init_header::<area::Header>(0, ())?;
            let (reg, roffset) = area.init_header::<reg::Registry<_, N>>(hoffset, ())?;
            let (alloc, aoffset) = area
                .init_header::<arena::Header<G>>(roffset, arena::Header::<G>::MIN_SEGMENT_SIZE)?;

            let size = area.size() - aoffset;
            let size = max_bound(size).ok_or(area::Error::OutofSize {
                requested: size,
                bound: ARENA_MAX_CAPACITY.cast_into(),
            })?;

            Ok(Self {
                m: area::MemBlk::from_raw(area).into(),
                alloc,
                reg,
                size,
            })
        }
    }
}

impl<S: AddrSpec, M: Mmap<S>, G: Strategy, H: Envelope, const N: usize> Conn<S, M, G, H, N> {
    #[inline(always)]
    pub fn header(&self) -> &area::Header {
        self.m.header()
    }

    #[inline(always)]
    pub fn reg(&self) -> &reg::Registry<ArenaDuplex<H>, N> {
        unsafe { self.reg.as_ref() }
    }

    pub fn arena<'a>(&'a self) -> arena::Arena<'a, G> {
        let config = arena::Config::default();
        Arena::from_header(unsafe { self.alloc.as_ref() }, self.size, config)
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

    pub fn acquire(&self, id: reg::Id) -> Option<ArenaDuplexView<'_, G, H>> {
        let (duplex, _) = self.reg().view(id, self.arena());
        duplex
    }
}
