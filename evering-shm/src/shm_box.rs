use core::alloc::Layout;
use core::marker::PhantomData;
use core::mem;
use core::mem::MaybeUninit;
use core::mem::SizedTypeProperties;
use core::ops::Deref;
use core::ops::DerefMut;
use core::pin::Pin;
use core::ptr;
use core::ptr::NonNull;

#[cfg(feature = "nightly")]
use alloc::alloc::{AllocError, handle_alloc_error};
#[cfg(not(feature = "nightly"))]
use allocator_api2::alloc::{AllocError, handle_alloc_error};

use crate::shm_alloc::ShmAllocator;

#[repr(C)]
pub struct ShmBox<T: ?Sized, A: ShmAllocator>(NonNull<T>, A);

impl<T, A: ShmAllocator> ShmBox<T, A> {
    pub fn new_in(x: T, alloc: A) -> ShmBox<T, A> {
        let boxed = Self::new_uninit_in(alloc);
        boxed.write(x)
    }

    pub fn try_new_in(x: T, alloc: A) -> Result<ShmBox<T, A>, AllocError> {
        let boxed = Self::try_new_uninit_in(alloc)?;
        Ok(boxed.write(x))
    }

    pub fn new_uninit_in(alloc: A) -> ShmBox<MaybeUninit<T>, A> {
        let layout = Layout::new::<mem::MaybeUninit<T>>();
        // NOTE: Prefer match over unwrap_or_else since closure sometimes not inlineable.
        // That would make code size bigger.
        match ShmBox::try_new_uninit_in(alloc) {
            Ok(m) => m,
            Err(_) => handle_alloc_error(layout),
        }
    }

    pub fn try_new_uninit_in(alloc: A) -> Result<ShmBox<MaybeUninit<T>, A>, AllocError> {
        let ptr = if T::IS_ZST {
            NonNull::dangling()
        } else {
            let layout = Layout::new::<MaybeUninit<T>>();
            alloc.allocate(layout)?.cast()
        };
        unsafe { Ok(ShmBox::from_raw_in(ptr.as_ptr(), alloc)) }
    }

    pub fn new_zeroed_in(alloc: A) -> ShmBox<MaybeUninit<T>, A> {
        let layout = Layout::new::<mem::MaybeUninit<T>>();
        // NOTE: Prefer match over unwrap_or_else since closure sometimes not inlineable.
        // That would make code size bigger.
        match ShmBox::try_new_zeroed_in(alloc) {
            Ok(m) => m,
            Err(_) => handle_alloc_error(layout),
        }
    }

    pub fn try_new_zeroed_in(alloc: A) -> Result<ShmBox<MaybeUninit<T>, A>, AllocError> {
        let ptr = if T::IS_ZST {
            NonNull::dangling()
        } else {
            let layout = Layout::new::<mem::MaybeUninit<T>>();
            alloc.allocate_zeroed(layout)?.cast()
        };
        unsafe { Ok(ShmBox::from_raw_in(ptr.as_ptr(), alloc)) }
    }

    pub fn pin_in(x: T, alloc: A) -> Pin<ShmBox<T, A>> {
        ShmBox::into_pin(ShmBox::new_in(x, alloc))
    }
}

impl<T: ?Sized, A: ShmAllocator> ShmBox<T, A> {
    pub fn into_pin(self) -> Pin<ShmBox<T, A>> {
        unsafe { Pin::new_unchecked(self) }
    }

    pub fn as_ref(&self) -> &T {
        self
    }

    pub fn as_ptr(&self) -> *const T {
        &raw const **self
    }

    pub fn as_mut_ptr(&mut self) -> *mut T {
        &raw mut **self
    }

    pub fn allocator(&self) -> &A {
        &self.1
    }

    pub unsafe fn from_raw_in(raw: *mut T, alloc: A) -> Self {
        ShmBox(unsafe { NonNull::new_unchecked(raw) }, alloc)
    }

    pub fn into_raw_with_allocator(b: Self) -> (*mut T, A) {
        let mut b = mem::ManuallyDrop::new(b);
        // We carefully get the raw pointer out in a way that Miri's aliasing model understands what
        // is happening: using the primitive "deref" of `Box`. In case `A` is *not* `Global`, we
        // want *no* aliasing requirements here!
        // In case `A` *is* `Global`, this does not quite have the right behavior; `into_raw`
        // works around that.
        let ptr = &raw mut **b;
        let alloc = unsafe { ptr::read(&b.1) };
        (ptr, alloc)
    }
}

impl<T, A: ShmAllocator> ShmBox<[T], A> {
    pub fn new_uninit_slice_in(len: usize, alloc: A) -> ShmBox<[MaybeUninit<T>], A> {
        let m = ShmBox::try_new_uninit_slice_in(len, alloc);
        m.unwrap()
    }

    pub fn new_zeroed_slice_in(len: usize, alloc: A) -> ShmBox<[MaybeUninit<T>], A> {
        let m = ShmBox::try_new_zeroed_slice_in(len, alloc);
        m.unwrap()
    }

    pub fn try_new_uninit_slice_in(
        len: usize,
        alloc: A,
    ) -> Result<ShmBox<[MaybeUninit<T>], A>, AllocError> {
        let ptr = if T::IS_ZST || len == 0 {
            NonNull::dangling()
        } else {
            let layout = match Layout::array::<mem::MaybeUninit<T>>(len) {
                Ok(l) => l,
                Err(_) => return Err(AllocError),
            };
            alloc.allocate(layout)?.cast()
        };
        let slice = ptr::slice_from_raw_parts_mut(ptr.as_ptr() as *mut MaybeUninit<T>, len);
        unsafe { Ok(ShmBox::from_raw_in(slice, alloc)) }
    }

    pub fn try_new_zeroed_slice_in(
        len: usize,
        alloc: A,
    ) -> Result<ShmBox<[MaybeUninit<T>], A>, AllocError> {
        let ptr = if T::IS_ZST || len == 0 {
            NonNull::dangling()
        } else {
            let layout = match Layout::array::<mem::MaybeUninit<T>>(len) {
                Ok(l) => l,
                Err(_) => return Err(AllocError),
            };
            alloc.allocate_zeroed(layout)?.cast()
        };
        let slice = ptr::slice_from_raw_parts_mut(ptr.as_ptr() as *mut MaybeUninit<T>, len);
        unsafe { Ok(ShmBox::from_raw_in(slice, alloc)) }
    }
}

impl<T, A: ShmAllocator> ShmBox<MaybeUninit<T>, A> {
    pub unsafe fn assume_init(self) -> ShmBox<T, A> {
        let (raw, alloc) = ShmBox::into_raw_with_allocator(self);
        unsafe { ShmBox::from_raw_in(raw as *mut T, alloc) }
    }

    pub fn write(self, value: T) -> ShmBox<T, A> {
        let mut this = self;
        unsafe {
            (*this).write(value);
            this.assume_init()
        }
    }
}

impl<T, A: ShmAllocator> ShmBox<[MaybeUninit<T>], A> {
    pub unsafe fn assume_init(self) -> ShmBox<[T], A> {
        let (raw, alloc) = ShmBox::into_raw_with_allocator(self);
        unsafe { ShmBox::from_raw_in(raw as *mut [T], alloc) }
    }
}

impl<T: ?Sized, A: ShmAllocator> Deref for ShmBox<T, A> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

impl<T: ?Sized, A: ShmAllocator> DerefMut for ShmBox<T, A> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}

unsafe impl<T: Send, A: ShmAllocator> Send for ShmBox<T, A> {}
unsafe impl<T: Sync, A: ShmAllocator> Sync for ShmBox<T, A> {}

/// A token that can be transferred between processes.
pub struct ShmToken<T, A: ShmAllocator>(isize, A, PhantomData<T>);

impl<T, A: ShmAllocator> ShmToken<T, A> {
    /// Returns the offset to the start memory region of the allocator.
    pub fn offset(&self) -> isize {
        self.0
    }

    pub fn from_raw(offset: isize, alloc: A) -> Self {
        ShmToken(offset, alloc, PhantomData)
    }
}

/// Safety: ShmToken is invariant across process.
unsafe impl<T, A: ShmAllocator> Send for ShmToken<T, A> {}
unsafe impl<T: Sync, A: ShmAllocator> Sync for ShmToken<T, A> {}

impl<T, A: ShmAllocator> From<ShmBox<T, A>> for ShmToken<T, A> {
    fn from(value: ShmBox<T, A>) -> Self {
        let (ptr, allocator) = ShmBox::into_raw_with_allocator(value);

        // Safety: the ptr is allocated by the allocator
        let offset = unsafe { allocator.offset(ptr) };

        ShmToken(offset, allocator, PhantomData)
    }
}

impl<T, A: ShmAllocator> From<ShmToken<T, A>> for ShmBox<T, A> {
    fn from(value: ShmToken<T, A>) -> Self {
        let ShmToken(offset, alloc, _) = value;

        let ptr = unsafe { alloc.get_aligned_ptr_mut::<T>(offset) };
        unsafe { ShmBox::from_raw_in(ptr.as_ptr(), alloc) }
    }
}
