use core::{
    alloc::Layout,
    mem::{self, ManuallyDrop, MaybeUninit},
    ops::{Deref, DerefMut},
    ptr::{self, NonNull},
};

#[cfg(test)]
use crate::mem::AllocError;
use crate::talc::{Meta, RefTalc};

pub struct PBox<'a, T: ?Sized> {
    ptr: NonNull<T>,
    meta: Meta,
    alloc: RefTalc<'a>,
}

unsafe impl<T: ?Sized + Send> Send for PBox<'_, T> {}
unsafe impl<T: ?Sized + Sync> Sync for PBox<'_, T> {}

impl<T: ?Sized + core::fmt::Debug> core::fmt::Debug for PBox<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized> Drop for PBox<'_, T> {
    fn drop(&mut self) {
        unsafe {
            let layout = Layout::for_value_raw(self.ptr.as_ptr());
            if layout.size() == 0 {
                ptr::drop_in_place(self.ptr.as_ptr());
                return;
            }
            let meta = mem::replace(&mut self.meta, Meta::null());
            let _ = self.alloc.release_value(self.ptr.as_ptr(), meta, layout);
        }
    }
}

impl<T: ?Sized> Deref for PBox<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T: ?Sized> DerefMut for PBox<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        unsafe { self.ptr.as_mut() }
    }
}

impl<'a, T: ?Sized> PBox<'a, T> {
    #[inline]
    pub fn as_ptr(&self) -> *const T {
        &raw const **self
    }

    #[inline]
    pub fn as_mut_ptr(&mut self) -> *mut T {
        &raw mut **self
    }

    fn into_raw(self) -> (*mut T, Meta, RefTalc<'a>) {
        let this = ManuallyDrop::new(self);
        unsafe {
            (
                this.ptr.as_ptr(),
                ptr::read(&this.meta),
                ptr::read(&this.alloc),
            )
        }
    }

    const unsafe fn from_raw(ptr: *mut T, meta: Meta, alloc: RefTalc<'a>) -> Self {
        Self {
            ptr: unsafe { NonNull::new_unchecked(ptr) },
            meta,
            alloc,
        }
    }

    pub fn release(self) -> Result<(), Self> {
        let (ptr, meta, alloc) = self.into_raw();
        let layout = unsafe { Layout::for_value_raw(ptr) };
        if layout.size() == 0 {
            unsafe { ptr::drop_in_place(ptr) };
            return Ok(());
        }
        match unsafe { alloc.release_value(ptr, meta, layout) } {
            Ok(()) => Ok(()),
            Err(meta) => Err(unsafe { Self::from_raw(ptr, meta, alloc) }),
        }
    }
}

impl<'a, T> PBox<'a, T> {
    pub(crate) fn null(alloc: RefTalc<'a>) -> Self {
        unsafe { Self::from_raw(NonNull::dangling().as_ptr(), Meta::null(), alloc) }
    }

    #[cfg(test)]
    pub(crate) fn try_new_in(value: T, alloc: RefTalc<'a>) -> Result<Self, AllocError> {
        Ok(PBox::<MaybeUninit<T>>::try_uninit(alloc)?.write(value))
    }
}

impl<'a, T> PBox<'a, [T]> {
    #[cfg(test)]
    pub(crate) fn try_new_slice_in(
        len: usize,
        mut make: impl FnMut(usize) -> T,
        alloc: RefTalc<'a>,
    ) -> Result<Self, AllocError> {
        let mut values = PBox::try_uninit_slice(len, alloc)?;
        for (index, value) in values.iter_mut().enumerate() {
            value.write(make(index));
        }
        Ok(unsafe { values.assume_init() })
    }
}

impl<'a, T> PBox<'a, MaybeUninit<T>> {
    #[cfg(test)]
    pub(crate) fn try_uninit(alloc: RefTalc<'a>) -> Result<Self, AllocError> {
        if size_of::<T>() == 0 {
            return Ok(PBox::null(alloc));
        }
        alloc
            .allocate(Layout::new::<MaybeUninit<T>>())
            .map(|meta| Self::from_meta(meta, alloc))
            .map_err(|_| AllocError)
    }

    pub(crate) fn from_meta(meta: Meta, alloc: RefTalc<'a>) -> Self {
        Self {
            ptr: alloc.pointer(&meta).cast(),
            meta,
            alloc,
        }
    }

    pub fn write(mut self, value: T) -> PBox<'a, T> {
        self.deref_mut().write(value);
        unsafe { self.assume_init() }
    }

    /// # Safety
    /// The stored value must be fully initialized as `T`.
    pub(crate) unsafe fn assume_init(self) -> PBox<'a, T> {
        let (ptr, meta, alloc) = self.into_raw();
        unsafe { PBox::from_raw(ptr.cast(), meta, alloc) }
    }
}

impl<'a, T> PBox<'a, [MaybeUninit<T>]> {
    pub(crate) fn from_meta_slice(meta: Meta, alloc: RefTalc<'a>, len: usize) -> Self {
        let ptr = ptr::slice_from_raw_parts_mut(alloc.pointer(&meta).cast().as_ptr(), len);
        unsafe { Self::from_raw(ptr, meta, alloc) }
    }

    #[cfg(test)]
    fn try_uninit_slice(len: usize, alloc: RefTalc<'a>) -> Result<Self, AllocError> {
        let layout = Layout::array::<MaybeUninit<T>>(len).map_err(|_| AllocError)?;
        if layout.size() == 0 {
            let ptr = ptr::slice_from_raw_parts_mut(NonNull::dangling().as_ptr(), len);
            return Ok(unsafe { Self::from_raw(ptr, Meta::null(), alloc) });
        }
        alloc
            .allocate(layout)
            .map(|meta| Self::from_meta_slice(meta, alloc, len))
            .map_err(|_| AllocError)
    }

    /// # Safety
    /// Every slice element must be fully initialized as `T`.
    pub(crate) unsafe fn assume_init(self) -> PBox<'a, [T]> {
        let (ptr, meta, alloc) = self.into_raw();
        unsafe { PBox::from_raw(ptr as *mut [T], meta, alloc) }
    }
}
