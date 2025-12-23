#![allow(unused_imports)]

pub mod talc {
    use core::marker::PhantomData;

    use crate::{
        mem::{self, AddrSpec, MapLayout, MemOps, Mmap},
        mod_channel,
        msg::Envelope,
        numeric::Id,
        reg, talc,
    };
    use channel::{MsgDuplex, MsgDuplexPeek, MsgDuplexView};

    pub use crate::mem::{Access, Accessible, MapBuilder, MemAllocInfo};

    mod_channel! {
        channel,
        meta: crate::talc::Meta,
    }

    pub trait MemAllocator = mem::MemAllocator<Meta = talc::Meta>;

    pub type Meta = talc::Meta;

    pub type AllocConfig = talc::Config;
    pub type AllocHeader = talc::Header<talc::Normal>;
    pub type RefAlloc<'a> = talc::RefTalc<'a, talc::Normal>;
    pub type MapAlloc<S, M> = talc::MapTalc<talc::Normal, S, M>;

    pub type RegistryHeader<H, const N: usize> = reg::Header<MsgDuplex<H>, N>;
    pub type MapRegistry<H, const N: usize, S, M> = reg::MapRegistry<MsgDuplex<H>, N, S, M>;

    pub struct SessionBy<H: Envelope, const N: usize> {
        _marker: PhantomData<H>,
    }

    pub struct Session<H: Envelope, const N: usize, S: AddrSpec, M: Mmap<S>> {
        pub alloc: MapAlloc<S, M>,
        pub reg: MapRegistry<H, N, S, M>,
    }

    impl<H: Envelope, const N: usize> SessionBy<H, N> {
        pub fn from<S: AddrSpec, M: Mmap<S>>(
            area: MapLayout<S, M>,
        ) -> Result<Session<H, N, S, M>, mem::Error<S, M>> {
            let conf = AllocConfig::new(area.size());
            Self::from_config(area, conf)
        }

        pub fn from_config<S: AddrSpec, M: Mmap<S>>(
            area: MapLayout<S, M>,
            conf: AllocConfig,
        ) -> Result<Session<H, N, S, M>, mem::Error<S, M>> {
            let mut area = area;
            let reg = area.push::<RegistryHeader<H, N>>(())?;
            let areserve = area.reserve::<AllocHeader>()?;
            let conf = conf.with_bound(area.rest_size());
            let alloc = area.commit(areserve, conf)?;
            let alloc = MapAlloc::from_handle(alloc);
            Ok(Session { alloc, reg })
        }
    }

    impl<H: Envelope, const N: usize, S: AddrSpec, M: Mmap<S>> TryFrom<MapLayout<S, M>>
        for Session<H, N, S, M>
    {
        type Error = mem::Error<S, M>;

        fn try_from(value: MapLayout<S, M>) -> Result<Self, Self::Error> {
            SessionBy::from(value)
        }
    }

    impl<H: Envelope, const N: usize, S: AddrSpec, M: Mmap<S>> Session<H, N, S, M> {
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

pub mod arena {
    use core::marker::PhantomData;

    use crate::{
        arena::{self, Strategy, cap_bound},
        mem::{self, AddrSpec, MapLayout, MemOps, Mmap},
        mod_channel,
        msg::Envelope,
        numeric::Id,
        reg::{self},
    };
    use channel::{MsgDuplex, MsgDuplexPeek, MsgDuplexView};

    mod_channel! {
        channel,
        meta:crate::arena::Meta,
    }

    pub use crate::arena::{Config, Optimistic, Pessimistic};
    pub use crate::mem::{Access, Accessible, MapBuilder, MemAllocInfo};

    trait MemAllocator = mem::MemAllocator<Meta = arena::Meta>;

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

            let size = cap_bound(area.rest_size());
            let conf = AllocMetaConfig::default::<G>();
            let alloc = area.push::<AllocHeader<G>>(conf)?;

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

#[macro_export]
macro_rules! mod_channel {
    (
        $modname:ident,
        meta:$meta:ty,
    ) => {
        pub mod $modname {
            type Meta = $meta;

            use $crate::channel::driver::CachePoolHandle;
            use $crate::channel::{Receiver, Sender};
            use $crate::channel::{cross, driver};
            use $crate::msg::Envelope;
            use $crate::reg::{Entry, MapEntry};
            use $crate::token;

            pub use $crate::channel::driver::{Completer, Submitter, TryCompState};
            pub use $crate::channel::{QueueChannel, TryRecvError, TrySendError};
            pub use $crate::token::{ReqId, ReqNull};

            pub type Token = token::Token<Meta>;
            pub type MsgToken<H> = token::PackToken<H, Meta>;
            pub type OpMsgToken<H> = token::ReqToken<H, Meta>;

            pub type MsgQueue<H> = cross::TokenQueue<H, Meta>;
            pub type MsgDuplex<H> = cross::TokenDuplex<H, Meta>;
            pub type MsgDuplexPeek<'a, H> = cross::DuplexView<H, Meta, &'a Entry<MsgDuplex<H>>>;
            pub type MsgDuplexView<H, S, M> =
                cross::DuplexView<H, Meta, MapEntry<MsgDuplex<H>, S, M>>;

            pub type SenderPeek<'a, H, R> = cross::Sender<H, Meta, &'a Entry<MsgDuplex<H>>, R>;
            pub type ReceiverPeek<'a, H, R> = cross::Sender<H, Meta, &'a Entry<MsgDuplex<H>>, R>;
            pub type SenderView<H, R, S, M> =
                cross::Sender<H, Meta, MapEntry<MsgDuplex<H>, S, M>, R>;
            pub type ReceiverView<H, R, S, M> =
                cross::Sender<H, Meta, MapEntry<MsgDuplex<H>, S, M>, R>;

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

            pub trait MsgSender<H: Envelope> =
                Sender<Item = MsgToken<H>, TryError = TrySendError<MsgToken<H>>> + QueueChannel;
            pub trait MsgReceiver<H: Envelope> =
                Receiver<Item = MsgToken<H>, TryError = TryRecvError> + QueueChannel;

            pub type CachePool<H, const N: usize> = CachePoolHandle<MsgToken<H>, N>;
            pub trait MsgSubmitter<H: Envelope, const N: usize> =
                Submitter<OwnOp<H, N>, MsgToken<H>, Error = TrySubmitError<H>> + QueueChannel;
            pub trait MsgCompleter<H: Envelope, const N: usize> =
                Completer<MsgToken<H>, Error = TryRecvError> + QueueChannel;
        }
    };
}