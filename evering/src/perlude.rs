mod root {
    use core::marker::PhantomData;

    use super::channel::MsgDuplex;
    use crate::{
        arena::{self, Strategy, cap_bound},
        mem::{self, AddrSpec, MapLayout, MemOps, Mmap},
        msg::Envelope,
        numeric::Id,
        perlude::channel::{MsgDuplexPeek, MsgDuplexView},
        reg::{self},
    };

    pub use crate::arena::{Config, Optimistic, Pessimistic};

    pub type Meta = arena::Meta;
    pub type Span = arena::SpanMeta;

    pub type AllocMetaConfig = arena::MetaConfig;
    pub type AllocHeader<G> = arena::Header<G>;
    pub type RefAlloc<'a, G> = arena::RefArena<'a, G>;
    pub type MapAlloc<G, S, M> = arena::MapArena<G, S, M>;

    pub type RegistryHeader<H, const N: usize> = reg::Header<MsgDuplex<H>, N>;
    pub type MapRegistry<H, const N: usize, S, M> = reg::MapRegistry<MsgDuplex<H>, N, S, M>;

    pub struct SessionBy<G: Strategy, H: Envelope, const N: usize> {
        _marker: PhantomData<(G, H)>,
    }

    pub struct Session<G: Strategy, H: Envelope, const N: usize, S: AddrSpec, M: Mmap<S>> {
        pub alloc: MapAlloc<G, S, M>,
        pub reg: MapRegistry<H, N, S, M>,
    }

    impl<G: Strategy, H: Envelope, const N: usize> SessionBy<G, H, N> {
        pub fn from<S: AddrSpec, M: Mmap<S>>(
            area: MapLayout<S, M>,
        ) -> Result<Session<G, H, N, S, M>, mem::Error<S, M>> {
            let conf = Config::default();
            Self::from_config(area, conf)
        }

        pub fn from_config<S: AddrSpec, M: Mmap<S>>(
            area: MapLayout<S, M>,
            aconf: Config,
        ) -> Result<Session<G, H, N, S, M>, mem::Error<S, M>> {
            let mut area = area;
            let reg = area.push::<RegistryHeader<H, N>>(())?;

            let offset = area.cur_offset();
            let size = cap_bound(area.size() - offset);
            let conf = AllocMetaConfig::default::<G>();
            let alloc = area.push::<AllocHeader<G>>(conf)?;

            let _ = area.finish();

            let alloc = MapAlloc::from_conf(alloc, size, aconf);
            Ok(Session { alloc, reg })
        }
    }

    impl<G: Strategy, H: Envelope, const N: usize, S: AddrSpec, M: Mmap<S>> TryFrom<MapLayout<S, M>>
        for Session<G, H, N, S, M>
    {
        type Error = mem::Error<S, M>;

        fn try_from(value: MapLayout<S, M>) -> Result<Self, Self::Error> {
            SessionBy::from(value)
        }
    }

    impl<G: Strategy, H: Envelope, const N: usize, S: AddrSpec, M: Mmap<S>> Clone
        for Session<G, H, N, S, M>
    {
        fn clone(&self) -> Self {
            Self {
                alloc: self.alloc.clone(),
                reg: self.reg.clone(),
            }
        }
    }

    impl<G: Strategy, H: Envelope, const N: usize, S: AddrSpec, M: Mmap<S>> Session<G, H, N, S, M> {
        pub fn prepare(&self, cap: usize) -> Option<Id> {
            let Ok((id, _)) = self.reg.prepare(cap, self.alloc.as_ref()) else {
                return None;
            };

            Some(id)
        }

        pub fn peek(&self, id: Id) -> Option<MsgDuplexPeek<'_, H>> {
            let (duplex, _) = self.reg.peek(id, self.alloc.clone());
            duplex
        }

        pub fn acquire(&self, id: Id) -> Option<MsgDuplexView<H, S, M>> {
            let (duplex, _) = self.reg.view(id, self.alloc.clone());
            duplex
        }
    }
}

pub mod allocator {
    use super::root::Meta;
    use crate::mem;

    pub use super::root::{Config, MapAlloc, Optimistic, Pessimistic, RefAlloc};
    pub use crate::mem::{Access, Accessible, MapBuilder, MemAllocInfo};

    pub trait MemAllocator: mem::MemAllocator<Meta = Meta> {}
    impl<T: mem::MemAllocator<Meta = Meta>> MemAllocator for T {}
}

pub mod channel {
    use super::root::Span;
    use crate::channel::driver::CachePoolHandle;
    use crate::channel::{Receiver, Sender};
    use crate::channel::{cross, driver};
    use crate::msg::Envelope;
    use crate::reg::{Entry, MapEntry};
    use crate::token;

    pub use crate::channel::driver::{Completer, Submitter, TryCompState};
    pub use crate::channel::{QueueChannel, TryRecvError, TrySendError};
    pub use crate::token::{ReqId, ReqNull};
    pub type Token = token::Token<Span>;
    pub type MsgToken<H> = token::PackToken<H, Span>;
    pub type OpMsgToken<H> = token::ReqToken<H, Span>;

    pub type MsgQueue<H> = cross::TokenQueue<H, Span>;
    pub type MsgDuplex<H> = cross::TokenDuplex<H, Span>;
    pub type MsgDuplexPeek<'a, H> = cross::DuplexView<H, Span, &'a Entry<MsgDuplex<H>>>;
    pub type MsgDuplexView<H, S, M> = cross::DuplexView<H, Span, MapEntry<MsgDuplex<H>, S, M>>;

    pub type SenderPeek<'a, H, R> = cross::Sender<H, Span, &'a Entry<MsgDuplex<H>>, R>;
    pub type ReceiverPeek<'a, H, R> = cross::Sender<H, Span, &'a Entry<MsgDuplex<H>>, R>;
    pub type SenderView<H, R, S, M> = cross::Sender<H, Span, MapEntry<MsgDuplex<H>, S, M>, R>;
    pub type ReceiverView<H, R, S, M> = cross::Sender<H, Span, MapEntry<MsgDuplex<H>, S, M>, R>;

    pub type SubmitterPeek<'a, H, const N: usize, R> =
        driver::Sx<SenderPeek<'a, H, R>, MsgToken<H>, N>;
    pub type CompleterPeek<'a, H, const N: usize, R> =
        driver::Cx<ReceiverPeek<'a, H, R>, MsgToken<H>, N>;
    pub type SubmitterView<H, const N: usize, R, S, M> =
        driver::Sx<SenderView<H, R, S, M>, MsgToken<H>, N>;
    pub type CompleterView<H, const N: usize, R, S, M> =
        driver::Cx<ReceiverView<H, R, S, M>, MsgToken<H>, N>;
    pub type TrySubmitError<H> = driver::TrySubmitError<TrySendError<OpMsgToken<H>>>;
    pub type RefOp<'a, H, const N: usize> = driver::RefOp<'a, MsgToken<H>, N>;
    pub type OwnOp<H, const N: usize> = driver::OwnOp<MsgToken<H>, N>;

    pub trait MsgSender<H: Envelope>:
        Sender<Item = MsgToken<H>, TryError = TrySendError<MsgToken<H>>> + QueueChannel
    {
    }

    impl<
        H: Envelope,
        T: Sender<Item = MsgToken<H>, TryError = TrySendError<MsgToken<H>>> + QueueChannel,
    > MsgSender<H> for T
    {
    }

    pub trait MsgReceiver<H: Envelope>:
        Receiver<Item = MsgToken<H>, TryError = TryRecvError> + QueueChannel
    {
    }

    impl<H: Envelope, T: Receiver<Item = MsgToken<H>, TryError = TryRecvError> + QueueChannel>
        MsgReceiver<H> for T
    {
    }

    pub type CachePool<H, const N: usize> = CachePoolHandle<MsgToken<H>, N>;

    pub trait MsgSubmitter<H: Envelope, const N: usize>:
        Submitter<OwnOp<H, N>, MsgToken<H>, Error = TrySubmitError<H>> + QueueChannel
    {
    }

    impl<
        H: Envelope,
        const N: usize,
        T: Submitter<OwnOp<H, N>, MsgToken<H>, Error = TrySubmitError<H>> + QueueChannel,
    > MsgSubmitter<H, N> for T
    {
    }

    pub trait MsgCompleter<H: Envelope, const N: usize>:
        Completer<MsgToken<H>, Error = TryRecvError> + QueueChannel
    {
    }

    impl<
        H: Envelope,
        const N: usize,
        T: Completer<MsgToken<H>, Error = TryRecvError> + QueueChannel,
    > MsgCompleter<H, N> for T
    {
    }
}

pub use root::{Session, SessionBy};
