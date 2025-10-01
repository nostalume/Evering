use core::ops::Deref;

use spin::RwLock;

impl Layout for () {
    fn init(&mut self) -> HeaderStatus {
        HeaderStatus::Initialized
    }

    fn attach(&self) -> HeaderStatus {
        HeaderStatus::Initialized
    }
}

pub(crate) trait Layout: core::fmt::Debug + Sized {
	// #[inline]
    // unsafe fn from_ptr<'a>(ptr: *mut u8) -> &'a mut Self {
    //     unsafe { &mut *(ptr.cast()) }
    // }
	#[inline]
    unsafe fn from_raw<'a>(ptr: *mut Self) -> &'a mut Self {
        unsafe { &mut *(ptr.cast()) }
    }
    fn init(&mut self) -> HeaderStatus;
    fn attach(&self) -> HeaderStatus;
	#[inline]
    fn attach_or_init(&mut self) -> HeaderStatus {
        match self.attach() {
            HeaderStatus::Initialized => HeaderStatus::Initialized,
            HeaderStatus::Corrupted | HeaderStatus::Uninitialized => {
                if self.init().is_ok() {
                    HeaderStatus::Initialized
                } else {
                    HeaderStatus::Corrupted
                }
            }
            HeaderStatus::Initializing => HeaderStatus::Initializing,
        }
    }
}

pub(crate) trait Metadata: Layout {
    fn valid_magic(&self) -> bool;
    fn with_magic(&mut self);
}

pub(crate) trait Recoverable: Layout {
    fn recover(&mut self) -> bool;
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderStatus {
    Uninitialized = 0,
    Initializing = 1,
    Initialized = 2,
    Corrupted = 3, // optional
}

impl HeaderStatus {
    pub fn is_ok(&self) -> bool {
        matches!(self, HeaderStatus::Initialized)
    }
}

pub struct SanityMetadata<T: Metadata> {
    pub inner: T,
    status: HeaderStatus,
}

impl<T: Metadata> SanityMetadata<T> {
    #[inline]
    pub const fn status(&self) -> HeaderStatus {
        self.status
    }

    #[inline]
    pub const fn with_status(&mut self, status: HeaderStatus) {
        self.status = status;
    }
}

#[repr(transparent)]
pub struct Header<T: Metadata>(RwLock<SanityMetadata<T>>);

impl<T: Metadata> Deref for Header<T> {
    type Target = RwLock<SanityMetadata<T>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: Metadata> core::fmt::Debug for Header<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let h = self.0.read();
        match h.status {
            HeaderStatus::Initialized => core::fmt::Debug::fmt(&h.inner, f),
            HeaderStatus::Corrupted => write!(f, "Header is corrupted"),
            _ => write!(f, "Header is uninitialized"),
        }
    }
}

impl<T: Metadata + core::fmt::Display> core::fmt::Display for Header<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let h = self.0.read();
        match h.status {
            HeaderStatus::Initialized => core::fmt::Display::fmt(&h.inner, f),
            HeaderStatus::Corrupted => write!(f, "Header is corrupted"),
            _ => write!(f, "Header is uninitialized"),
        }
    }
}

impl<T: Metadata> Layout for Header<T> {
    fn init(&mut self) -> HeaderStatus {
        let mut write = self.write();
        if write.inner.init().is_ok() {
            write.with_status(HeaderStatus::Initialized);
            HeaderStatus::Initialized
        } else {
            write.with_status(HeaderStatus::Corrupted);
            HeaderStatus::Corrupted
        }
    }

    fn attach(&self) -> HeaderStatus {
        let read = self.0.read();
        if read.inner.valid_magic() {
            match read.status() {
                HeaderStatus::Initialized => {
                    read.inner.attach();
                    return HeaderStatus::Initialized;
                }
                HeaderStatus::Initializing => {
                    drop(read);
                    for _ in 0..Self::TRYTIMES {
                        let try_read = self.read();
                        match try_read.status() {
                            HeaderStatus::Initialized => {
                                try_read.inner.attach();
                                return HeaderStatus::Initialized;
                            }
                            _ => core::hint::spin_loop(),
                        }
                    }
                    return HeaderStatus::Initializing;
                }
                _ => return HeaderStatus::Corrupted,
            }
        } else {
            return HeaderStatus::Uninitialized;
        }
    }
}

impl<T: Metadata> Header<T> {
    const TRYTIMES: u8 = 50;
}
