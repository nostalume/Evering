use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const MAX_QUEUES: usize = 2 * 6;

type AddrSpan = crate::area::AddrSpan<usize>;

#[repr(C)]
struct RegistryEntry {
    span: AddrSpan,
    rc: AtomicU32,
    active: AtomicBool,
}

impl RegistryEntry {
    pub const fn null() -> Self {
        Self {
            span: AddrSpan::null(),
            rc: AtomicU32::new(0),
            active: AtomicBool::new(false),
        }
    }
}

#[repr(C)]
struct Registry {
    count: AtomicU32,
    entires: [RegistryEntry; MAX_QUEUES],
}

impl core::fmt::Debug for Registry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Registry")
            .field("count", &self.count)
            .finish()
    }
}

impl crate::header::Layout for Registry {
    type Config = ();
    fn init(&mut self, _cfg: ()) -> crate::header::HeaderStatus {
        self.count.store(0, Ordering::Relaxed);

        for entry in &mut self.entires {
            *entry = RegistryEntry::null()
        }

        crate::header::HeaderStatus::Initialized
    }

    fn attach(&self) -> crate::header::HeaderStatus {
        todo!()
    }
}

impl Registry {
    pub const fn null() -> Self {
        Self {
            count: AtomicU32::new(0),
            entires: [const { RegistryEntry::null() }; MAX_QUEUES],
        }
    }
}
