use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::ops::Deref;
use core::ops::DerefMut;
use core::pin::Pin;

use alloc::alloc::AllocError;
use alloc::boxed::Box;

use crate::shm_alloc::ShmAllocator;

#[repr(transparent)]
pub struct ShmBox<T: ?Sized, A: ShmAllocator>(Box<T, A>);

impl<T, A: ShmAllocator> ShmBox<T, A> {
    pub fn new_in(x: T, alloc: A) -> ShmBox<T, A> {
        let mut boxed = Box::new_uninit_in(alloc);
        boxed.write(x);
        unsafe { ShmBox(boxed.assume_init()) }
    }

    pub fn try_new_in(x: T, alloc: A) -> Result<ShmBox<T, A>, AllocError> {
        let mut boxed = Box::try_new_uninit_in(alloc)?;
        boxed.write(x);
        unsafe { Ok(ShmBox(boxed.assume_init())) }
    }

    pub fn new_uninit_in(alloc: A) -> ShmBox<MaybeUninit<T>, A> {
        ShmBox(Box::new_uninit_in(alloc))
    }

    pub fn try_new_uninit_in(alloc: A) -> Result<ShmBox<MaybeUninit<T>, A>, AllocError> {
        Ok(ShmBox(Box::try_new_uninit_in(alloc)?))
    }

    pub fn new_zeroed_in(alloc: A) -> ShmBox<MaybeUninit<T>, A> {
        ShmBox(Box::new_zeroed_in(alloc))
    }

    pub fn try_new_zeroed_in(alloc: A) -> Result<ShmBox<MaybeUninit<T>, A>, AllocError> {
        Ok(ShmBox(Box::try_new_zeroed_in(alloc)?))
    }

    pub unsafe fn from_raw_in(raw: *mut T, alloc: A) -> Self {
        ShmBox(unsafe { Box::from_raw_in(raw, alloc) })
    }

    pub fn pin_in(x: T, alloc: A) -> Pin<ShmBox<T, A>> {
        ShmBox::into_pin(ShmBox::new_in(x, alloc))
    }

    pub fn into_pin(self) -> Pin<ShmBox<T, A>> {
        unsafe { Pin::new_unchecked(self) }
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
        ShmBox(Box::new_uninit_slice_in(len, alloc))
    }

    pub fn new_zeroed_slice_in(len: usize, alloc: A) -> ShmBox<[MaybeUninit<T>], A> {
        ShmBox(Box::new_zeroed_slice_in(len, alloc))
    }

    pub fn try_new_uninit_slice_in(
        len: usize,
        alloc: A,
    ) -> Result<ShmBox<[MaybeUninit<T>], A>, AllocError> {
        Ok(ShmBox(Box::try_new_uninit_slice_in(len, alloc)?))
    }

    pub fn try_new_zeroed_slice_in(
        len: usize,
        alloc: A,
    ) -> Result<ShmBox<[MaybeUninit<T>], A>, AllocError> {
        Ok(ShmBox(Box::try_new_zeroed_slice_in(len, alloc)?))
    }
}

impl<T, A: ShmAllocator> ShmBox<MaybeUninit<T>, A> {
    pub unsafe fn assume_init(self) -> ShmBox<T, A> {
        ShmBox(unsafe { self.0.assume_init() })
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
        ShmBox(unsafe { self.0.assume_init() })
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
        Self(value)
    }
}

/// A token that can be transferred between processes.
pub struct ShmToken<T, A: ShmAllocator>(isize, A, PhantomData<T>);

impl<T, A: ShmAllocator> ShmToken<T, A> {
    /// Returns the offset to the start memory region of the allocator.
    pub fn offset(&self) -> isize {
        self.0
    }
}

/// Safety: ShmToken is invariant across process.
unsafe impl<T, A: ShmAllocator> Send for ShmToken<T, A> {}
unsafe impl<T: Sync, A: ShmAllocator> Sync for ShmToken<T, A> {}

impl<T, A: ShmAllocator> From<ShmBox<T, A>> for ShmToken<T, A> {
    fn from(value: ShmBox<T, A>) -> Self {
        let (ptr, allocator) = Box::<T, A>::into_non_null_with_allocator(value.0);

        // Safety: the ptr is allocated by the allocator
        let offset = unsafe { allocator.offset(ptr.as_ptr()) };

        ShmToken(offset, allocator, PhantomData)
    }
}

impl<T, A: ShmAllocator> From<ShmToken<T, A>> for ShmBox<T, A> {
    fn from(value: ShmToken<T, A>) -> Self {
        let ShmToken(offset, allocator, _) = value;

        let ptr = unsafe { allocator.get_aligned_ptr_mut::<T>(offset) };
        ShmBox(unsafe { Box::from_non_null_in(ptr, allocator) })
    }
}