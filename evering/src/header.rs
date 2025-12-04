use core::{
    ops::Deref,
    sync::atomic::{AtomicU8, AtomicU16, AtomicUsize, Ordering},
};

pub type Magic = u16;
type AtomicMagic = AtomicU16;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Uninitialized = 0,
    Initializing = 1,
    Initialized = 2,
    Corrupted = 3, // optional
}

impl Status {
    #[inline]
    pub const fn from_u8(v: u8) -> Status {
        match v {
            0 => Status::Uninitialized,
            1 => Status::Initializing,
            2 => Status::Initialized,
            _ => Status::Corrupted,
        }
    }
}

pub trait Layout: Sized {
    type Config;

    const MAGIC: Magic;

    #[inline]
    unsafe fn from_raw<'a>(ptr: *mut Self) -> &'a mut Self {
        unsafe { &mut *(ptr.cast()) }
    }
    fn init(&mut self, conf: Self::Config) -> Status;
    fn attach(&self) -> Status;
    #[inline]
    fn attach_or_init(&mut self, conf: Self::Config) -> Status {
        match self.attach() {
            Status::Initialized => Status::Initialized,
            Status::Corrupted | Status::Uninitialized => {
                if self.init(conf) == Status::Initialized {
                    Status::Initialized
                } else {
                    Status::Corrupted
                }
            }
            Status::Initializing => Status::Initializing,
        }
    }
    /// Modify state sucessfully or not in unmap
    fn finalize(&self) -> bool;
}

#[repr(C)]
pub struct Header<T: Layout> {
    magic: AtomicMagic,
    status: AtomicU8,
    pub inner: T,
}

impl<T: Layout + core::fmt::Debug> core::fmt::Debug for Header<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let status = Status::from_u8(self.status.load(Ordering::Relaxed));
        f.debug_struct("Header")
            .field("magic", &self.magic)
            .field("status", &status)
            .field("inner", &self.inner)
            .finish()
    }
}

impl<T: Layout> Deref for Header<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T: Layout> Header<T> {
    #[inline]
    pub fn status(&self) -> Status {
        Status::from_u8(self.status.load(Ordering::Acquire))
    }

    #[inline]
    fn with_status(&self, st: Status) {
        self.status.store(st as u8, Ordering::Release);
    }

    #[inline]
    fn try_with_status(&self, expected: Status, new: Status) -> bool {
        self.status
            .compare_exchange_weak(
                expected as u8,
                new as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    #[inline]
    fn valid_magic(&self) -> bool {
        self.magic.load(Ordering::Acquire) == T::MAGIC
    }

    #[inline]
    fn with_magic(&self) {
        self.magic.store(T::MAGIC, Ordering::Release);
    }
}

impl<T: Layout> Layout for Header<T> {
    type Config = T::Config;

    const MAGIC: Magic = T::MAGIC;

    fn init(&mut self, conf: Self::Config) -> Status {
        if !self.try_with_status(Status::Uninitialized, Status::Initializing) {
            // race
            return self.status();
        }

        if self.inner.init(conf) == Status::Initialized {
            self.with_magic();
            self.with_status(Status::Initialized);
            Status::Initialized
        } else {
            Status::Corrupted
        }
    }

    fn attach(&self) -> Status {
        if !self.valid_magic() {
            return Status::Uninitialized;
        }

        match self.status() {
            Status::Initialized => {
                self.inner.attach();
                Status::Initialized
            }
            Status::Initializing => Status::Initializing,
            _ => Status::Corrupted,
        }
    }

    fn finalize(&self) -> bool {
        self.inner.finalize()
    }
}

impl Layout for () {
    type Config = ();

    const MAGIC: Magic = 0x0;

    fn init(&mut self, _conf: ()) -> Status {
        Status::Initialized
    }

    fn attach(&self) -> Status {
        Status::Initialized
    }

    fn finalize(&self) -> bool {
        true
    }
}

#[derive(Debug)]
#[repr(C)]
pub struct RcMeta {
    rc: AtomicUsize,
}

impl Layout for RcMeta {
    type Config = ();

    const MAGIC: Magic = 0xABCD;

    #[inline]
    fn init(&mut self, _conf: Self::Config) -> Status {
        self.rc.store(1, Ordering::Release);
        Status::Initialized
    }

    #[inline]
    fn attach(&self) -> Status {
        self.rc.fetch_add(1, Ordering::Relaxed);
        Status::Initialized
    }

    #[inline]
    fn finalize(&self) -> bool {
        // Safety: It shouldn't be smaller than 0.
        self.rc.fetch_sub(1, Ordering::Acquire);
        true
    }
}
