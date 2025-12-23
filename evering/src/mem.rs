use core::alloc::Layout;
use core::marker::PhantomData;
use core::ptr::NonNull;

mod area;

pub use self::area::{MapHandle, MapLayout, RawMap};
pub use alloc::alloc::{AllocError, handle_alloc_error};

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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        core::fmt::Debug::fmt(&self, f)
    }
}

pub const trait Accessible: Copy + From<Access> {
    fn permits(self, access: Access) -> bool;
}

pub trait AddrSpec {
    type Addr: memory_addr::MemoryAddr;
    type Flags: Accessible;
}

pub trait Mmap<S: AddrSpec>: Sized {
    type Handle;
    type MapFlags: Copy;
    type Error: core::fmt::Debug;

    fn map(
        self,
        start: Option<S::Addr>,
        size: usize,
        mflags: Self::MapFlags,
        pflags: S::Flags,
        handle: Self::Handle,
    ) -> Result<RawMap<S, Self>, Self::Error>;
    fn unmap(area: &mut RawMap<S, Self>) -> Result<(), Self::Error>;
}

pub trait Mprotect<S: AddrSpec>: Mmap<S> {
    unsafe fn protect(area: &mut RawMap<S, Self>, new_flags: S::Flags) -> Result<(), Self::Error>;
}

pub trait SharedMmap<S: AddrSpec>: Mmap<S> {
    fn shared(
        self,
        size: usize,
        access: Access,
        handle: Self::Handle,
    ) -> Result<RawMap<S, Self>, Self::Error>;
}

pub enum Error<S: AddrSpec, M: Mmap<S>> {
    PermissionDenied { requested: Access },
    OutofSize { requested: usize, bound: usize },
    UnenoughSpace { requested: usize, allocated: usize },
    Contention,
    InvalidHeader,
    MapError(M::Error),
}

impl<S: AddrSpec, M: Mmap<S>> core::error::Error for Error<S, M> {}

impl<S: AddrSpec, M: Mmap<S>> core::fmt::Debug for Error<S, M> {
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
            Self::InvalidHeader => write!(f, "Header initialization failed"),
            Self::MapError(err) => write!(f, "Mapping error: {:?}", err),
        }
    }
}

impl<S: AddrSpec, M: Mmap<S>> core::fmt::Display for Error<S, M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self, f)
    }
}

pub struct MapBuilder<S: AddrSpec, M: Mmap<S>> {
    bk: M,
    _marker: PhantomData<S>,
}

impl<S: AddrSpec, M: Mmap<S>> MapBuilder<S, M> {
    #[inline]
    pub const fn from_backend(bk: M) -> Self {
        MapBuilder {
            bk,
            _marker: PhantomData,
        }
    }

    #[inline]
    pub fn map_layout(
        self,
        start: Option<S::Addr>,
        size: usize,
        mflags: M::MapFlags,
        pflags: S::Flags,
        handle: M::Handle,
    ) -> Result<MapLayout<S, M>, Error<S, M>> {
        let Self { bk, _marker } = self;
        let raw = bk
            .map(start, size, mflags, pflags, handle)
            .map_err(|e| Error::MapError(e))?;
        MapLayout::new(raw)
    }

    #[inline]
    pub fn map<T: TryFrom<MapLayout<S, M>, Error = Error<S, M>>>(
        self,
        start: Option<S::Addr>,
        size: usize,
        mflags: M::MapFlags,
        pflags: S::Flags,
        handle: M::Handle,
    ) -> Result<T, Error<S, M>> {
        T::try_from(self.map_layout(start, size, mflags, pflags, handle)?)
    }
}

impl<S: AddrSpec, M: SharedMmap<S>> MapBuilder<S, M> {
    #[inline]
    pub fn shared_layout(
        self,
        size: usize,
        access: Access,
        handle: M::Handle,
    ) -> Result<MapLayout<S, M>, Error<S, M>> {
        let Self { bk, _marker } = self;
        let raw = bk
            .shared(size, access, handle)
            .map_err(|e| Error::MapError(e))?;
        MapLayout::new(raw)
    }

    #[inline]
    pub fn shared<T: TryFrom<MapLayout<S, M>, Error = Error<S, M>>>(
        self,
        size: usize,
        access: Access,
        handle: M::Handle,
    ) -> Result<T, Error<S, M>> {
        T::try_from(self.shared_layout(size, access, handle)?)
    }
}

pub type MetaOf<A> = <A as MemAlloc>::Meta;

pub trait Meta: Clone {
    // type SpanMeta: Span;
    fn null() -> Self;
    fn is_null(&self) -> bool;
    unsafe fn recall(&self, base_ptr: *const u8) -> NonNull<u8>;
    fn recall_by<A: MemAlloc>(&self, alloc: &A) -> NonNull<u8> {
        unsafe { self.recall(alloc.base_ptr()) }
    }
    fn layout_bytes(&self) -> Layout;
}

pub trait MemAllocator: MemAlloc + MemDealloc {}
impl<A: MemAllocator> MemAllocator for &A {}
// pub trait MemAllocator2: MemAlloc + MemDeallocBy {}
// impl<A: MemAllocator2> MemAllocator2 for &A {}

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

pub unsafe trait MemDealloc: MemAlloc {
    fn dealloc(&self, meta: Self::Meta, layout: Layout) -> bool;
    #[inline]
    fn dealloc_bytes(&self, meta: Self::Meta) -> bool {
        let layout = meta.layout_bytes();
        self.dealloc(meta, layout)
    }
}

pub trait MemAllocInfo: MemAlloc {
    // type Header;
    // fn header(&self) -> &Self::Header;
    fn allocated(&self) -> usize;
    fn remained(&self) -> usize;
    fn discarded(&self) -> usize;
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
    fn dealloc(&self, meta: Self::Meta, layout: Layout) -> bool {
        (*self).dealloc(meta, layout)
    }
}

pub unsafe trait MemOps {
    #[inline]
    fn start<Addr: memory_addr::MemoryAddr>(&self) -> Addr {
        self.start_ptr().addr().into()
    }

    /// Returns the start pointer of the memory block.
    fn start_ptr(&self) -> *const u8;

    #[inline]
    fn end<Addr: memory_addr::MemoryAddr>(&self) -> Addr {
        self.end_ptr().addr().into()
    }

    fn end_ptr(&self) -> *const u8;

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

    /// Returns a pointer to the memory at the given offset.
    #[inline]
    fn get_ptr(&self, offset: usize) -> *const u8 {
        unsafe { self.start_ptr().add(offset) }
    }

    /// Returns a pointer to the memory at the given offset.
    #[inline]
    fn get_mut_ptr(&self, offset: usize) -> *mut u8 {
        unsafe { self.start_mut_ptr().add(offset) }
    }

    /// Given a offset related to start, acquire the `Sized` instance.
    /// from the area.
    ///
    /// ## Panics
    /// `start.add(size_of<T>())` overflows.
    ///
    /// ## Safety
    /// User should ensure the validity of memory area and instance.
    ///
    /// ## Returns
    /// (`*mut T`, `next_offset`)
    /// - `*mut T`: the pointer to the instance.
    /// - `next_offset`: `offset + size_of<T>()`
    #[inline]
    unsafe fn obtain_by_offset<T>(&self, offset: usize) -> Result<(*mut T, usize), usize> {
        use memory_addr::MemoryAddr;
        let t_size = core::mem::size_of::<T>();
        let t_align = core::mem::align_of::<T>();

        let start = self.start_ptr().addr();
        let t_start = start.add(offset).align_up(t_align);
        let new_offset = t_start.add(t_size) - start;
        if new_offset > self.size() {
            return Err(new_offset);
        }
        let ptr = (t_start as *const u8).cast::<T>().cast_mut();
        Ok((ptr, new_offset))
    }

    /// Given a absolute addr, acquire the `Sized` instance.
    /// from the area.
    ///
    /// ## Panics
    /// `start.add(size_of<T>())` overflows.
    ///
    /// ## Safety
    /// User should ensure the validity of memory area and instance.
    ///
    /// ## Returns
    /// (`*mut T`, `next_offset`)
    /// - `*mut T`: the pointer to the instance.
    /// - `next_offset`: `offset + size_of<T>()`
    #[inline]
    unsafe fn obtain_by_addr<T, Addr: memory_addr::MemoryAddr>(
        &self,
        start: Addr,
    ) -> Result<(*mut T, usize), Option<usize>> {
        if start < self.start() {
            return Err(None);
        }
        let offset = start.sub_addr(self.start());
        unsafe { self.obtain_by_offset(offset).map_err(Option::Some) }
    }

    /// Given a offset related to start, acquire the slice instance.
    /// from the area.
    ///
    /// ## Panics
    /// `start.add(size_of<T>())` overflows.
    ///
    /// ## Safety
    /// User should ensure the validity of memory area and instance.
    ///
    /// ## Returns
    /// (`*mut [T]`, `next_offset`)
    /// - `*mut [T]`: the pointer to the slice.
    /// - `next_offset`: `offset + size_of<T>() * len`
    #[inline]
    unsafe fn obtain_slice_by_offset<T>(
        &self,
        offset: usize,
        len: usize,
    ) -> Result<(*mut [T], usize), Option<usize>> {
        use alloc::alloc::Layout;
        use memory_addr::MemoryAddr;

        let layout = Layout::array::<T>(len).map_err(|_| Option::None)?;
        let arr_size = layout.size();
        let arr_align = layout.align();

        unsafe {
            let start = self.start_ptr().addr();
            let t_start = start.add(offset).align_up(arr_align);
            let new_offset = t_start.add(arr_size) - start;
            if new_offset > self.size() {
                return Err(Some(new_offset));
            }
            let ptr = (t_start as *const u8).cast::<T>().cast_mut();
            let ptr = core::slice::from_raw_parts_mut(ptr, len);
            Ok((ptr, new_offset))
        }
    }

    /// Given a offset related to start, acquire the slice instance.
    /// from the area.
    ///
    /// ## Panics
    /// `start.add(size_of<T>())` overflows.
    ///
    /// ## Safety
    /// User should ensure the validity of memory area and instance.
    ///
    /// ## Returns
    /// (`*mut [T]`, `next_offset`)
    /// - `*mut [T]`: the pointer to the slice.
    /// - `next_offset`: `offset + size_of<T>() * len`
    #[inline]
    unsafe fn obtain_slice_by_addr<T, Addr: memory_addr::MemoryAddr>(
        &self,
        start: Addr,
        len: usize,
    ) -> Result<(*mut [T], usize), Option<usize>> {
        if start < self.start() {
            return Err(None);
        }
        let offset = start.sub_addr(self.start());
        unsafe { self.obtain_slice_by_offset(offset, len) }
    }
}

unsafe impl<M: MemOps> MemOps for &M {
    fn start_ptr(&self) -> *const u8 {
        (*self).start_ptr()
    }

    fn end_ptr(&self) -> *const u8 {
        (*self).end_ptr()
    }

    fn size(&self) -> usize {
        (*self).size()
    }
}

macro_rules! addr_span {
    ($ty:ty) => {
        impl AddrSpan<$ty> {
            #[inline]
            pub const fn null() -> Self {
                Self {
                    start_offset: 0,
                    size: 0,
                }
            }

            #[inline]
            pub const fn is_null(&self) -> bool {
                self.start_offset == 0 || self.size == 0
            }

            #[inline]
            pub const fn end_offset(&self) -> $ty {
                self.start_offset + self.size
            }

            #[inline]
            pub const fn align_of<T>(&self) -> Self {
                use crate::numeric::{Alignable, Measurable};
                Self {
                    start_offset: self.start_offset.align_up_of::<T>(),
                    size: <$ty>::size_of::<T>(),
                }
            }

            #[inline]
            pub const fn align_to(&self, align: $ty) -> Self {
                use crate::numeric::Alignable;
                let aligned_offset = self.start_offset.align_up(align);
                let new_size = self.end_offset() - aligned_offset;

                Self {
                    start_offset: aligned_offset,
                    size: new_size,
                }
            }

            #[inline]
            pub const fn align_to_of<T>(&self) -> Self {
                use crate::numeric::Alignable;
                let aligned_offset = self.start_offset.align_up_of::<T>();
                let new_size = self.end_offset() - aligned_offset;

                Self {
                    start_offset: aligned_offset,
                    size: new_size,
                }
            }

            #[inline]
            pub const fn shift(&self, delta: $ty) -> Self {
                Self {
                    start_offset: self.start_offset.saturating_add(delta),
                    size: self.size,
                }
            }

            #[inline]
            pub const unsafe fn as_ptr(&self, base_ptr: *const u8) -> *const u8 {
                use crate::numeric::CastInto;
                unsafe { base_ptr.add(self.start_offset.cast_into()) }
            }

            #[inline]
            pub const unsafe fn as_mut_ptr(&self, base_ptr: *const u8) -> *mut u8 {
                use crate::numeric::CastInto;
                unsafe { base_ptr.add(self.start_offset.cast_into()).cast_mut() }
            }

            #[inline]
            pub const unsafe fn as_nonnull(&self, base_ptr: *const u8) -> NonNull<u8> {
                unsafe { NonNull::new_unchecked(self.as_mut_ptr(base_ptr)) }
            }
        }
    };
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

addr_span!(u32);
addr_span!(usize);
