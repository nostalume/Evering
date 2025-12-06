mod root {
    use core::marker::PhantomData;

    use super::channel::MsgDuplex;
    use crate::{
        arena::{self, Strategy, cap_bound},
        mem::{self, AddrSpec, MemBlkHandle, MemBlkLayout, MemBlkOps, Mmap},
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
    pub type MemAllocHeader<G, S, M> = arena::MemArenaMeta<G, S, M>;
    pub type RefAlloc<'a, G> = arena::RefArena<'a, G>;
    pub type MemAlloc<G, S, M> = arena::MemArena<G, S, M>;

    pub type RegistryHeader<H, const N: usize> = reg::Header<MsgDuplex<H>, N>;
    pub type MemRegistry<H, const N: usize, S, M> = reg::MemRegistry<MsgDuplex<H>, N, S, M>;

    pub struct SessionBy<G: Strategy, H: Envelope, const N: usize> {
        _marker: PhantomData<(G, H)>,
    }

    pub struct Session<G: Strategy, H: Envelope, const N: usize, S: AddrSpec, M: Mmap<S>> {
        pub alloc: MemAlloc<G, S, M>,
        pub reg: MemRegistry<H, N, S, M>,
    }

    impl<G: Strategy, H: Envelope, const N: usize> SessionBy<G, H, N> {
        pub fn from<S: AddrSpec, M: Mmap<S>>(
            area: MemBlkLayout<S, M>,
        ) -> Result<Session<G, H, N, S, M>, mem::Error<S, M>> {
            let conf = Config::default();
            Self::from_config(area, conf)
        }

        pub fn from_config<S: AddrSpec, M: Mmap<S>>(
            area: MemBlkLayout<S, M>,
            aconf: Config,
        ) -> Result<Session<G, H, N, S, M>, mem::Error<S, M>> {
            let mut area = area;
            let reg = area.push::<RegistryHeader<H, N>>(())?;

            let offset = area.offset();
            let conf = AllocMetaConfig::default::<G>();
            let alloc = area.push::<AllocHeader<G>>(conf)?;

            let (area, _) = area.finish();
            let area: MemBlkHandle<_, _> = area.into();

            let size = cap_bound(area.size() - offset);
            let alloc = MemAlloc::from_conf(
                unsafe { MemAllocHeader::from_raw(area.clone(), alloc) },
                size,
                aconf,
            );
            let reg = unsafe { MemRegistry::from_raw(area, reg) };
            Ok(Session { alloc, reg })
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

    pub use super::root::{Config, MemAlloc, Optimistic, Pessimistic, RefAlloc};
    pub use crate::mem::{MemAllocInfo, MemBlkBuilder};

    pub trait MemAllocator: mem::MemAllocator<Meta = Meta> {}
    impl<T: mem::MemAllocator<Meta = Meta>> MemAllocator for T {}
}

pub mod channel {
    use super::root::Span;
    use crate::channel::cross;
    use crate::channel::{QueueReceiver, QueueSender};
    use crate::msg::Envelope;
    use crate::reg::{Entry, MemEntry};
    use crate::token;

    pub use crate::channel::{TryRecvError,TrySendError};
    pub type Token = token::Token<Span>;
    pub type MsgToken<H> = token::PackToken<H, Span>;

    pub type MsgQueue<H> = cross::TokenQueue<H, Span>;
    pub type MsgDuplex<H> = cross::TokenDuplex<H, Span>;
    pub type MsgDuplexPeek<'a, H> = cross::DuplexView<H, Span, &'a Entry<MsgDuplex<H>>>;
    pub type MsgDuplexView<H, S, M> = cross::DuplexView<H, Span, MemEntry<MsgDuplex<H>, S, M>>;

    pub type SenderPeek<'a, H, R> = cross::Sender<H, Span, &'a Entry<MsgDuplex<H>>, R>;
    pub type ReceiverPeek<'a, H, R> = cross::Sender<H, Span, &'a Entry<MsgDuplex<H>>, R>;
    pub type SenderView<'a, H, R, S, M> = cross::Sender<H, Span, MemEntry<MsgDuplex<H>, S, M>, R>;
    pub type ReceiverView<'a, H, R, S, M> = cross::Sender<H, Span, MemEntry<MsgDuplex<H>, S, M>, R>;

    pub trait MsgSender<H: Envelope>:
        QueueSender<Item = MsgToken<H>, TryError = TrySendError<MsgToken<H>>>
    {
    }

    impl<H: Envelope, T: QueueSender<Item = MsgToken<H>, TryError = TrySendError<MsgToken<H>>>>
        MsgSender<H> for T
    {
    }
    pub trait MsgReceiver<H: Envelope>:
        QueueReceiver<Item = MsgToken<H>, TryError = TryRecvError>
    {
    }

    impl<H: Envelope, T: QueueReceiver<Item = MsgToken<H>, TryError = TryRecvError>> MsgReceiver<H>
        for T
    {
    }

    // pub type CachePool<H, const N: usize> = driver::CachePoolHandle<MsgToken<H>, N>;

    // pub trait MsgSubmitter<H: Envelope>: driver::Submitter<H, Span> {}

    // impl<H: Envelope, T: driver::Submitter<H, Span>> MsgSubmitter<H> for T {}

    // pub trait MsgCompleter<H: Envelope>: driver::Completer<H, Span> {}

    // impl<H: Envelope, T: driver::Completer<H, Span>> MsgCompleter<H> for T {}
}

pub use root::{Session, SessionBy};
