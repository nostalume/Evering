use core::ops::Deref;

use spin::RwLock;

#[repr(C)]
pub struct ShmHeaderIn {
    magic: u16,
    status: ShmStatus,
}

#[repr(transparent)]
pub struct ShmHeader(RwLock<ShmHeaderIn>);

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShmStatus {
    Uninitialized = 0,
    Initializing = 1,
    Initialized = 2,
    Corrupted = 3, // optional
}

impl ShmHeaderIn {
    // TODO
    pub const MAGIC_VALUE: u16 = 0x1000;

    #[inline]
    pub const fn intializing(&mut self) {
        self.with_magic();
        self.with_status(ShmStatus::Initializing);
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
}

impl ShmHeader {
    // including padding!
    pub const HEADER_SIZE: usize = core::mem::size_of::<Self>();
    pub const HEADER_ALIGN:usize = core::mem::align_of::<Self>();
}

impl Deref for ShmHeader {
    type Target = RwLock<ShmHeaderIn>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
