use core::{ops::Deref, sync::atomic::AtomicU32};

use spin::RwLock;

#[repr(C)]
pub struct HeaderIn {
    magic: u16,
    status: ShmStatus,
    rc: AtomicU32,
    spec: [Option<isize>; 5],
}

#[repr(transparent)]
pub struct Header(RwLock<HeaderIn>);

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShmStatus {
    Initializing = 1,
    Initialized = 2,
    Corrupted = 3, // optional
}

impl HeaderIn {
    // TODO
    pub const MAGIC_VALUE: u16 = 0x7203;

    #[inline]
    pub fn initializing(&mut self) {
        self.with_magic();
        self.with_status(ShmStatus::Initializing);
        self.rc.store(1, core::sync::atomic::Ordering::Relaxed);
        self.spec = [None; 5];
    }

    #[inline]
    pub const fn with_magic(&mut self) {
        self.magic = Self::MAGIC_VALUE;
    }

    #[inline]
    pub const fn valid_magic(&self) -> bool {
        self.magic == Self::MAGIC_VALUE
    }

    #[inline]
    pub const fn status(&self) -> ShmStatus {
        self.status
    }

    #[inline]
    pub const fn with_status(&mut self, status: ShmStatus) {
        self.status = status;
    }

    #[inline]
    pub fn inc_rc(&self) -> u32 {
        self.rc.fetch_add(1, core::sync::atomic::Ordering::AcqRel)
    }

    #[inline]
    pub fn dec_rc(&self) -> Option<u32> {
        use core::sync::atomic::Ordering;
        match self
            .rc
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |cur| {
                if cur == 0 { None } else { Some(cur - 1) }
            }) {
            Ok(prev) => Some(prev),
            Err(_) => None,
        }
    }

    #[inline]
    pub const fn spec(&self, idx: usize) -> Option<isize> {
        assert!(
            idx < self.spec.len(),
            "idx must smaller than length of spec",
        );
        // Safety: assert!
        self.spec.get(idx).copied().flatten()
    }

    #[inline]
    pub fn with_spec(&mut self, offset: isize, idx: usize) {
        assert!(
            idx < self.spec.len(),
            "idx must smaller than length of spec"
        );
        self.spec.get_mut(idx).map(|slot| {
            *slot = Some(offset);
        });
    }
}

impl Header {
    // including padding!
    pub const HEADER_SIZE: usize = core::mem::size_of::<Self>();
    pub const HEADER_ALIGN: usize = core::mem::align_of::<Self>();

    pub fn init<E, F: FnOnce() -> Result<(), E>>(&self, f: F) -> Result<(), E> {
        let mut inner = self.0.write();
        if inner.valid_magic() {
            inner.inc_rc();
            Ok(())
        } else {
            inner.initializing();
            let res = f();
            if res.is_ok() {
                inner.with_status(ShmStatus::Initialized);
            } else {
                inner.with_status(ShmStatus::Corrupted);
            }
            res
        }
    }
}

impl Deref for Header {
    type Target = RwLock<HeaderIn>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
