use core::alloc::Layout;
use core::ptr::NonNull;

pub use crate::header::LayoutField;
use crate::schema::LayoutId;

mod area;

#[cfg(test)]
pub use self::area::MapView;
pub(crate) use self::area::ProjectionError;
pub use self::area::{
    CloseError as RegionCloseError, Map, MapLayout, Mapped, Peer, Recovery, Ref, Region,
};
pub use alloc::alloc::AllocError;

bitflags::bitflags! {
    #[repr(transparent)]
    #[derive(Debug,Clone,Copy,PartialEq,Eq)]
    pub struct Access: u8 {
        const READ  = 0x1;
        const WRITE = 0x1 << 1;
        const EXEC  = 0x1 << 2;
    }
}

impl core::fmt::Display for Access {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&self, f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub len: usize,
    pub access: Access,
}

impl Request {
    pub const fn new(len: usize, access: Access) -> Self {
        Self { len, access }
    }
}

/// Produces one exclusively owned process-local mapping.
///
/// # Safety
///
/// An implementation must return a valid, suitably aligned mapping with the
/// requested access and extent. Its release function must remain callable and
/// release that mapping exactly once from any thread that may own an admitted
/// `Mapped<T>`.
pub unsafe trait Source: Sized {
    type Error: core::fmt::Debug;

    fn map(self, request: Request) -> Result<Map, Self::Error>;
}

pub enum OpenError<E> {
    Source(E),
    Admission(Error),
}

impl<E: core::fmt::Debug> core::fmt::Debug for OpenError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Source(error) => write!(f, "Mapping failed: {error:?}"),
            Self::Admission(error) => core::fmt::Debug::fmt(error, f),
        }
    }
}

impl<E: core::fmt::Debug> core::fmt::Display for OpenError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self, f)
    }
}

impl<E: core::fmt::Debug> core::error::Error for OpenError<E> {}

pub enum Error {
    PermissionDenied { requested: Access },
    OutofSize { requested: usize, bound: usize },
    UnenoughSpace { requested: usize, allocated: usize },
    Contention,
    LayoutClosed,
    DuplicateAttachment,
    InvalidHeader,
    LayoutMismatch(LayoutField),
    PoisonedComposition,
    ArithmeticOverflow,
    ParticipantExhausted,
}

impl core::error::Error for Error {}

impl core::fmt::Debug for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PermissionDenied { requested } => {
                write!(f, "Permission denied, requested {:?}", requested)
            }
            Self::UnenoughSpace {
                requested,
                allocated,
            } => write!(
                f,
                "Not enough space available, requested {}, allocated {}",
                requested, allocated
            ),
            Self::OutofSize { requested, bound } => write!(
                f,
                "Out of upper bounded size, requested {}, upper bound {}",
                requested, bound
            ),
            Self::Contention => write!(f, "Contention"),
            Self::LayoutClosed => write!(f, "Shared layout is closed"),
            Self::DuplicateAttachment => {
                write!(f, "This participant already attached the shared layout")
            }
            Self::InvalidHeader => write!(f, "Header initialization failed"),
            Self::LayoutMismatch(field) => write!(f, "Layout mismatch: {:?}", field),
            Self::PoisonedComposition => write!(f, "Layout composition is poisoned"),
            Self::ArithmeticOverflow => write!(f, "Layout cursor arithmetic overflow"),
            Self::ParticipantExhausted => write!(f, "No shared-memory participant slot is free"),
        }
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self, f)
    }
}

pub trait Meta: crate::msg::Repr {
    // type SpanMeta: Span;
    fn null() -> Self;
    fn is_null(&self) -> bool;
    unsafe fn recall(&self, base_ptr: *const u8) -> NonNull<u8>;
    fn recall_by<A: MemAlloc>(&self, alloc: &A) -> NonNull<u8> {
        unsafe { self.recall(alloc.base_ptr()) }
    }
    fn layout_bytes(&self) -> Layout;
}

// Internal allocator kernel. Safe callers allocate only through `Heap`.
pub trait MemAllocator: MemAlloc + MemDealloc {}
impl<A: MemAllocator> MemAllocator for &A {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferError {
    Busy,
    WrongAllocator,
    WrongExtent,
    Null,
    OutOfBounds,
    Misaligned,
}

/// An allocator instance that validates shared token metadata before recall.
///
/// # Safety
///
/// A successful admission must return storage owned by `layout_id()` that
/// satisfies `expected`.
pub unsafe trait TransferAllocator: MemAllocator {
    fn layout_id(&self) -> LayoutId;
    fn admit(
        &self,
        owner: LayoutId,
        meta: &Self::Meta,
        expected: Layout,
    ) -> Result<NonNull<u8>, TransferError>;
}

unsafe impl<A: TransferAllocator> TransferAllocator for &A {
    fn layout_id(&self) -> LayoutId {
        (*self).layout_id()
    }

    fn admit(
        &self,
        owner: LayoutId,
        meta: &Self::Meta,
        expected: Layout,
    ) -> Result<NonNull<u8>, TransferError> {
        (*self).admit(owner, meta, expected)
    }
}
// pub trait MemAllocator2: MemAlloc + MemDeallocBy {}
// impl<A: MemAllocator2> MemAllocator2 for &A {}

/// Allocates region-relative blocks whose metadata may cross a process boundary.
///
/// # Safety
///
/// Implementors must return metadata that denotes storage within `base_ptr`'s
/// region, remains valid at another mapping base, and satisfies the requested
/// layout until it is successfully deallocated.
pub unsafe trait MemAlloc {
    type Meta: Meta;
    type Error;
    fn base_ptr(&self) -> *const u8;
    fn alloc(&self, layout: Layout) -> Result<Self::Meta, Self::Error>;
    fn alloc_of<H>(&self) -> Result<Self::Meta, Self::Error> {
        let layout = Layout::new::<H>();
        self.alloc(layout)
    }
    fn alloc_bytes(&self, size: usize) -> Result<Self::Meta, Self::Error> {
        let layout = Layout::array::<u8>(size).unwrap();
        self.alloc(layout)
    }
}

/// Releases blocks allocated by the same region-relative allocator.
///
/// # Safety
///
/// Implementors must reject foreign, stale, or layout-mismatched metadata
/// without releasing storage and must make each successful release observable
/// exactly once to all participating processes.
pub unsafe trait MemDealloc: MemAlloc {
    fn dealloc(&self, meta: Self::Meta, layout: Layout) -> Result<(), Self::Meta>;

    /// Drops `pointer` and releases its allocation only after deallocation
    /// authority has been acquired.
    ///
    /// # Safety
    ///
    /// `pointer`, `meta`, and `layout` must describe the same live allocation.
    /// On failure the implementation must not touch `pointer` and must return
    /// the exact `meta`.
    unsafe fn release<T: ?Sized>(
        &self,
        pointer: *mut T,
        meta: Self::Meta,
        layout: Layout,
    ) -> Result<(), Self::Meta>;

    #[inline]
    fn dealloc_bytes(&self, meta: Self::Meta) -> Result<(), Self::Meta> {
        let layout = meta.layout_bytes();
        self.dealloc(meta, layout)
    }
}

unsafe impl<A: MemAlloc> MemAlloc for &A {
    type Meta = A::Meta;
    type Error = A::Error;

    fn base_ptr(&self) -> *const u8 {
        (*self).base_ptr()
    }
    fn alloc(&self, layout: Layout) -> Result<Self::Meta, Self::Error> {
        (*self).alloc(layout)
    }
}

unsafe impl<A: MemDealloc> MemDealloc for &A {
    fn dealloc(&self, meta: Self::Meta, layout: Layout) -> Result<(), Self::Meta> {
        (*self).dealloc(meta, layout)
    }

    unsafe fn release<T: ?Sized>(
        &self,
        pointer: *mut T,
        meta: Self::Meta,
        layout: Layout,
    ) -> Result<(), Self::Meta> {
        unsafe { (*self).release(pointer, meta, layout) }
    }
}

/// Exposes the bounds of one contiguous memory region.
///
/// # Safety
///
/// `start_ptr` and `size` must describe one live contiguous allocation for the
/// duration of the implementation value. Mutable access must remain subject to
/// the owner's aliasing and synchronization rules.
pub unsafe trait MemOps {
    /// Returns the start pointer of the memory block.
    fn start_ptr(&self) -> *const u8;

    /// Returns the byte size of the memory block.
    fn size(&self) -> usize;

    /// Returns the start pointer of the memory block.
    ///
    /// ## Safety
    /// The `ptr` should be correctly modified.
    #[inline]
    unsafe fn start_mut_ptr(&self) -> *mut u8 {
        self.start_ptr().cast_mut()
    }

    /// Returns the offset to the start of the memory block.
    ///
    /// ## Safety
    /// - `ptr` must be allocated in the memory.
    #[inline]
    unsafe fn offset<T: ?Sized>(&self, ptr: *const T) -> usize {
        // Safety: `ptr` must has address greater than `self.start_ptr()`.
        unsafe { ptr.byte_offset_from_unsigned(self.start_ptr()) }
    }
}

unsafe impl<M: MemOps> MemOps for &M {
    fn start_ptr(&self) -> *const u8 {
        (*self).start_ptr()
    }

    fn size(&self) -> usize {
        (*self).size()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddrSpan<T> {
    pub start_offset: T,
    pub size: T,
}

impl<T> AddrSpan<T> {
    #[inline]
    pub const fn new(offset: T, size: T) -> Self {
        Self {
            start_offset: offset,
            size,
        }
    }
}

impl AddrSpan<usize> {
    pub const fn null() -> Self {
        Self::new(0, 0)
    }

    pub const fn is_null(&self) -> bool {
        self.start_offset == 0 || self.size == 0
    }

    pub const unsafe fn as_nonnull(&self, base: *const u8) -> NonNull<u8> {
        unsafe { NonNull::new_unchecked(base.add(self.start_offset).cast_mut()) }
    }
}
