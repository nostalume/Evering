use core::ops::Deref;

use spin::RwLock;

#[repr(C)]
pub struct HeaderIn {
    magic: u16,
    status: ShmStatus,
    spec: [Option<isize>; 5],
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
    pub const fn spec(&self, idx: usize) -> Option<isize> {
        assert!(
            idx < self.spec.len(),
            "idx must smaller than length of spec",
        );
        // Safety: assert!()
        *self.spec.get(idx).unwrap()
    }

    #[inline]
    pub const fn with_spec(&mut self, offset: isize, idx: usize) -> bool {
        assert!(
            idx < self.spec.len(),
            "idx must smaller than length of spec"
        );
        if self.spec.get(idx).unwrap().is_some() {
            false
        } else {
            self.spec[idx] = Some(offset);
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
