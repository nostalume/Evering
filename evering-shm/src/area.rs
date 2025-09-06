use memory_addr::{AddrRange, MemoryAddr};

pub trait ShmSpec {
    type Addr: MemoryAddr;
    type Flags: Copy;
}

pub trait ShmBackend<S: ShmSpec>: Sized {
    type Config;
    type Error: core::fmt::Debug;

    fn map(
        self,
        start: Option<S::Addr>,
        size: usize,
        flags: S::Flags,
        cfg: Self::Config,
    ) -> Result<ShmArea<S, Self>, Self::Error>;
    fn unmap(area: &mut ShmArea<S, Self>) -> Result<(), Self::Error>;
}

pub trait ShmProtect<S: ShmSpec>: ShmBackend<S> {
    fn protect(area: &mut ShmArea<S, Self>, new_flags: S::Flags) -> Result<(), Self::Error>;
}

pub struct ShmArea<S: ShmSpec, M: ShmBackend<S>> {
    va_range: AddrRange<S::Addr>,
    flags: S::Flags,
    bk: M,
}

impl<S: ShmSpec, M: ShmBackend<S>> ShmSpec for ShmArea<S, M> {
    type Addr = S::Addr;
    type Flags = S::Flags;
}

impl<S: ShmSpec, M: ShmBackend<S>> Clone for ShmArea<S, M>
where
    M: Clone,
{
    fn clone(&self) -> Self {
        Self {
            va_range: self.va_range,
            flags: self.flags,
            bk: self.bk.clone(),
        }
    }
}

impl<S: ShmSpec, M: ShmBackend<S>> ShmArea<S, M> {
    /// Creates a new memory area without mapping it.
    pub(crate) fn new(start: S::Addr, size: usize, flags: S::Flags, bk: M) -> Self {
        let va_range = AddrRange::from_start_size(start, size);
        Self {
            va_range,
            flags,
            bk,
        }
    }

    #[inline]
    pub(crate) fn as_addr(&self, offset: usize) -> Option<(S::Addr, usize)> {
        let addr = self.start().add(offset);
        let size = self.end().checked_sub_addr(addr)?;
        Some((addr, size))
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
    /// (`*mut T`, `next_start`)
    /// - `*mut T`: the pointer to the instance.
    /// - `next_start`: `start + size_of<T>()`
    #[inline]
    pub(crate) unsafe fn acquire_by_offset<T: Sized>(
        &self,
        offset: usize,
    ) -> Option<(*mut T, usize)> {
        let t_size = core::mem::size_of::<T>();
        let t_align = core::mem::align_of::<T>();

        let t_start = self.start().add(offset);
        let new_offset = offset.add(t_size).align_up(t_align);
        let t_end = t_start.add(new_offset);
        if t_end > self.end() {
            return None;
        }
        let ptr = t_start.into() as *mut T;
        Some((ptr, new_offset))
    }

    /// Given a start address, acquire the `Sized` instance.
    /// from the area.
    ///
    /// ## Panics
    /// `start.add(size_of<T>())` overflows.
    ///
    /// ## Safety
    /// User should ensure the validity of memory area and instance.
    ///
    /// ## Returns
    /// (`*mut T`, `next_start`)
    /// - `*mut T`: the pointer to the instance.
    /// - `next_start`: `start + size_of<T>()`
    #[inline]
    pub(crate) unsafe fn acquire_by_addr<T: Sized>(
        &self,
        start: S::Addr,
    ) -> Option<(*mut T, usize)> {
        let offset = start.sub_addr(self.start());
        unsafe { self.acquire_by_offset(offset) }
    }
}

impl<S: ShmSpec, M: ShmBackend<S>> ShmArea<S, M> {
    /// Returns the virtual address range.
    #[inline]
    pub const fn va_range(&self) -> AddrRange<S::Addr> {
        self.va_range
    }

    /// Returns the memory flags, e.g., the permission bits.
    #[inline]
    pub const fn flags(&self) -> S::Flags {
        self.flags
    }

    /// Returns the start address of the memory area.
    #[inline]
    pub const fn start(&self) -> S::Addr {
        self.va_range.start
    }

    /// Returns the end address of the memory area.
    #[inline]
    pub const fn end(&self) -> S::Addr {
        self.va_range.end
    }

    /// Returns the size of the memory area.
    #[inline]
    pub fn size(&self) -> usize {
        self.va_range.size()
    }

    #[inline]
    pub fn backend(&self) -> &M {
        &self.bk
    }

    #[inline]
    pub fn backend_mut(&mut self) -> &mut M {
        &mut self.bk
    }
}
