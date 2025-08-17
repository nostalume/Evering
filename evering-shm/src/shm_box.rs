use core::marker::PhantomData;
use core::mem::ManuallyDrop;
use core::mem::MaybeUninit;
use core::ops::Deref;
use core::ops::DerefMut;
use core::pin::Pin;

#[cfg(feature = "nightly")]
use alloc::{alloc::AllocError, boxed::Box};
#[cfg(not(feature = "nightly"))]
use allocator_api2::{alloc::AllocError, boxed::Box};

use crate::shm_alloc::ShmAllocator;

#[repr(transparent)]
pub struct ShmBox<T: ?Sized, A: ShmAllocator>(ManuallyDrop<Box<T, A>>);

impl<T, A: ShmAllocator> ShmBox<T, A> {
    pub fn new_in(x: T, alloc: A) -> ShmBox<T, A> {
        let mut boxed = Box::new_uninit_in(alloc);
        boxed.write(x);
        let boxed = ManuallyDrop::new(unsafe { boxed.assume_init() });
        ShmBox(boxed)
    }

    pub fn try_new_in(x: T, alloc: A) -> Result<ShmBox<T, A>, AllocError> {
        let mut boxed = Box::try_new_uninit_in(alloc)?;
        boxed.write(x);
        let boxed = ManuallyDrop::new(unsafe { boxed.assume_init() });
        Ok(ShmBox(boxed))
    }

    pub fn new_uninit_in(alloc: A) -> ShmBox<MaybeUninit<T>, A> {
        ShmBox(ManuallyDrop::new(Box::new_uninit_in(alloc)))
    }

    pub fn try_new_uninit_in(alloc: A) -> Result<ShmBox<MaybeUninit<T>, A>, AllocError> {
        Ok(ShmBox(ManuallyDrop::new(Box::try_new_uninit_in(alloc)?)))
    }

    pub fn new_zeroed_in(alloc: A) -> ShmBox<MaybeUninit<T>, A> {
        ShmBox(ManuallyDrop::new(Box::new_zeroed_in(alloc)))
    }

    pub fn try_new_zeroed_in(alloc: A) -> Result<ShmBox<MaybeUninit<T>, A>, AllocError> {
        Ok(ShmBox(ManuallyDrop::new(Box::try_new_zeroed_in(alloc)?)))
    }

    pub unsafe fn from_raw_in(raw: *mut T, alloc: A) -> Self {
        ShmBox(ManuallyDrop::new(unsafe { Box::from_raw_in(raw, alloc) }))
    }

    pub fn pin_in(x: T, alloc: A) -> Pin<ShmBox<T, A>> {
        ShmBox::into_pin(ShmBox::new_in(x, alloc))
    }

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
        Box::allocator(&self.0)
    }
}

impl<T, A: ShmAllocator> ShmBox<[T], A> {
    pub fn new_uninit_slice_in(len: usize, alloc: A) -> ShmBox<[MaybeUninit<T>], A> {
        ShmBox(ManuallyDrop::new(Box::new_uninit_slice_in(len, alloc)))
    }

    pub fn new_zeroed_slice_in(len: usize, alloc: A) -> ShmBox<[MaybeUninit<T>], A> {
        ShmBox(ManuallyDrop::new(Box::new_zeroed_slice_in(len, alloc)))
    }

    pub fn try_new_uninit_slice_in(
        len: usize,
        alloc: A,
    ) -> Result<ShmBox<[MaybeUninit<T>], A>, AllocError> {
        Ok(ShmBox(ManuallyDrop::new(Box::try_new_uninit_slice_in(
            len, alloc,
        )?)))
    }

    pub fn try_new_zeroed_slice_in(
        len: usize,
        alloc: A,
    ) -> Result<ShmBox<[MaybeUninit<T>], A>, AllocError> {
        Ok(ShmBox(ManuallyDrop::new(Box::try_new_zeroed_slice_in(
            len, alloc,
        )?)))
    }
}

impl<T, A: ShmAllocator> ShmBox<MaybeUninit<T>, A> {
    pub unsafe fn assume_init(self) -> ShmBox<T, A> {
        let inner = ManuallyDrop::into_inner(self.0);
        ShmBox(ManuallyDrop::new(unsafe { inner.assume_init() }))
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
        let inner = ManuallyDrop::into_inner(self.0);
        ShmBox(ManuallyDrop::new(unsafe { inner.assume_init() }))
    }
}

impl<T, A: ShmAllocator> Deref for ShmBox<T, A> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T, A: ShmAllocator> DerefMut for ShmBox<T, A> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T, A: ShmAllocator> AsRef<Box<T, A>> for ShmBox<T, A> {
    fn as_ref(&self) -> &Box<T, A> {
        &self.0
    }
}

impl<T, A: ShmAllocator> From<Box<T, A>> for ShmBox<T, A> {
    fn from(value: Box<T, A>) -> Self {
        Self(ManuallyDrop::new(value))
    }
}

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
        let (ptr, allocator) =
            Box::<T, A>::into_non_null_with_allocator(ManuallyDrop::into_inner(value.0));

        // Safety: the ptr is allocated by the allocator
        let offset = unsafe { allocator.offset(ptr.as_ptr()) };

        ShmToken(offset, allocator, PhantomData)
    }
}

impl<T, A: ShmAllocator> From<ShmToken<T, A>> for ShmBox<T, A> {
    fn from(value: ShmToken<T, A>) -> Self {
        let ShmToken(offset, allocator, _) = value;

        let ptr = unsafe { allocator.get_aligned_ptr_mut::<T>(offset) };
        ShmBox(ManuallyDrop::new(unsafe {
            Box::from_non_null_in(ptr, allocator)
        }))
    }
}
