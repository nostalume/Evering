use core::ops::Deref;

use spin::RwLock;

#[repr(C)]
pub struct HeaderIn {
    magic: u16,
    status: ShmStatus,
    spec: Option<isize>,
}

#[repr(transparent)]
pub struct Header(RwLock<HeaderIn>);

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShmStatus {
    Uninitialized = 0,
    Initializing = 1,
    Initialized = 2,
    Corrupted = 3, // optional
}

impl HeaderIn {
    // TODO
    pub const MAGIC_VALUE: u16 = 0x1000;

    #[inline]
    pub const fn intializing(&mut self) {
        self.with_magic();
        self.with_status(ShmStatus::Initializing);
        self.spec = None;
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
    pub const fn spec(&self) -> Option<isize> {
        self.spec
    }

    #[inline]
    pub fn with_spec(&mut self, offset: isize) -> bool {
        if self.spec.is_some() {
            false
        } else {
            self.spec = Some(offset);
            true
        }
    }
}

impl Header {
    // including padding!
    pub const HEADER_SIZE: usize = core::mem::size_of::<Self>();
    pub const HEADER_ALIGN: usize = core::mem::align_of::<Self>();
}

impl Deref for Header {
    type Target = RwLock<HeaderIn>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
