use alloc::boxed;
use core::alloc::Layout;
use core::mem::MaybeUninit;
use core::ops::{Deref, DerefMut};
use core::ptr;
use core::sync::atomic::AtomicUsize;
use core::{mem, ptr::NonNull};
use std::alloc::handle_alloc_error;

use crate::arena::{Meta, MetaAlloc};
use crate::perlude::AllocError;

const fn is_zst<T>() -> bool {
    size_of::<T>() == 0
}

#[repr(C)]
pub struct PBox<T: ?Sized, A: MetaAlloc> {
    ptr: NonNull<T>,
    meta: Meta,
    alloc: A,
}

unsafe impl<T: ?Sized + Send, A: MetaAlloc + Send> Send for PBox<T, A> {}
unsafe impl<T: ?Sized + Sync, A: MetaAlloc + Sync> Sync for PBox<T, A> {}

impl<T: ?Sized, A: MetaAlloc> Drop for PBox<T, A> {
    fn drop(&mut self) {
        self.alloc.demalloc(self.meta);
    }
}

impl<T: ?Sized, A: MetaAlloc> Deref for PBox<T, A> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T: ?Sized, A: MetaAlloc> DerefMut for PBox<T, A> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.ptr.as_mut() }
    }
}

impl<T, A: MetaAlloc> PBox<T, A> {
    pub fn new_in(x: T, alloc: A) -> PBox<T, A> {
        let boxed = Self::new_uninit_in(alloc);
        boxed.write(x)
    }

    pub fn try_new_in(x: T, alloc: A) -> Result<PBox<T, A>, AllocError> {
        let boxed = Self::try_new_uninit_in(alloc)?;
        Ok(boxed.write(x))
    }
    pub fn new_uninit_in(alloc: A) -> PBox<mem::MaybeUninit<T>, A> {
        let layout = Layout::new::<mem::MaybeUninit<T>>();
        // NOTE: Prefer match over unwrap_or_else since closure sometimes not inlineable.
        // That would make code size bigger.
        match PBox::try_new_uninit_in(alloc) {
            Ok(m) => m,
            Err(_) => alloc::alloc::handle_alloc_error(layout),
        }
    }

    pub fn try_new_uninit_in(alloc: A) -> Result<PBox<mem::MaybeUninit<T>, A>, AllocError> {
        if is_zst::<T>() {
            return Ok(PBox::null(alloc));
        }

        let layout = Layout::new::<mem::MaybeUninit<T>>();
        let meta = alloc.malloc_by(layout).map_err(|_| AllocError)?;
        Ok(PBox::from_meta(meta, alloc))
    }

    pub fn from_meta(meta: Meta, alloc: A) -> PBox<mem::MaybeUninit<T>, A> {
        let ptr = meta.as_ptr_of();
        PBox { ptr, meta, alloc }
    }

    #[inline]
    pub const fn null(alloc: A) -> Self {
        let ptr = NonNull::dangling();
        let meta = Meta::null();
        unsafe { PBox::from_raw_ptr(ptr.as_ptr(), meta, alloc) }
    }

    // pub fn new_zeroed_in(alloc: A) -> ShmBox<MaybeUninit<T>, A> {
    //     let layout = Layout::new::<mem::MaybeUninit<T>>();
    //     // NOTE: Prefer match over unwrap_or_else since closure sometimes not inlineable.
    //     // That would make code size bigger.
    //     match LocalBox::try_new_zeroed_in(alloc) {
    //         Ok(m) => m,
    //         Err(_) => handle_alloc_error(layout),
    //     }
    // }

    // pub fn try_new_zeroed_in(alloc: A) -> Result<ShmBox<MaybeUninit<T>, A>, AllocError> {
    //     let ptr = if T::IS_ZST {
    //         NonNull::dangling()
    //     } else {
    //         let layout = Layout::new::<mem::MaybeUninit<T>>();
    //         alloc.allocate_zeroed(layout)?.cast()
    //     };
    //     unsafe { Ok(ShmBox::from_raw_in(ptr.as_ptr(), alloc)) }
    // }
}

impl<T: ?Sized, A: MetaAlloc> PBox<T, A> {
    #[inline]
    pub fn token(self) -> Token {
        Token::token(self)
    }

    #[inline]
    pub fn as_ref(&self) -> &T {
        self
    }

    #[inline]
    pub fn as_ptr(&self) -> *const T {
        &raw const **self
    }

    #[inline]
    pub fn as_mut_ptr(&mut self) -> *mut T {
        &raw mut **self
    }

    #[inline]
    pub const fn allocator(&self) -> &A {
        &self.alloc
    }

    #[inline]
    pub fn leak<'a>(b: Self) -> (&'a mut T, Meta)
    where
        A: 'a,
    {
        let (ptr, meta, alloc) = PBox::into_raw_ptr(b);
        mem::forget(alloc);
        unsafe { (&mut *ptr, meta) }
    }

    #[inline]
    const fn into_raw(b: Self) -> (Meta, A) {
        let b = mem::ManuallyDrop::new(b);
        let m = unsafe { ptr::read(&b.meta) };
        let alloc = unsafe { ptr::read(&b.alloc) };
        (m, alloc)
    }

    #[inline]
    fn into_raw_ptr(b: Self) -> (*mut T, Meta, A) {
        let mut b = mem::ManuallyDrop::new(b);
        let ptr = &raw mut **b;
        let m = unsafe { ptr::read(&b.meta) };
        let alloc = unsafe { ptr::read(&b.alloc) };
        (ptr, m, alloc)
    }

    #[inline]
    const unsafe fn from_raw_ptr(ptr: *mut T, meta: Meta, alloc: A) -> Self {
        unsafe {
            let ptr = NonNull::new_unchecked(ptr);
            Self { ptr, meta, alloc }
        }
    }
}

impl<T, A: MetaAlloc> PBox<[T], A> {
    pub fn copy_from_slice(src: &[T], alloc: A) -> PBox<[T], A> {
        let len = src.len();
        let mut m = PBox::new_uninit_slice_in(len, alloc);
        unsafe {
            let dst = m.as_mut_ptr().as_mut_ptr();
            let src = src.as_ptr().cast();
            dst.copy_from_nonoverlapping(src, len);
            m.assume_init()
        }
    }

    pub fn new_uninit_slice_in(len: usize, alloc: A) -> PBox<[mem::MaybeUninit<T>], A> {
        let m = PBox::try_new_uninit_slice_in(len, alloc);
        m.unwrap()
    }

    // pub fn new_zeroed_slice_in(len: usize, alloc: A) -> LocalBox<[mem::MaybeUninit<T>], A> {
    //     let m = LocalBox::try_new_zeroed_slice_in(len, alloc);
    //     m.unwrap()
    // }

    pub fn try_new_uninit_slice_in(
        len: usize,
        alloc: A,
    ) -> Result<PBox<[mem::MaybeUninit<T>], A>, AllocError> {
        let meta = if is_zst::<T>() || len == 0 {
            Meta::null()
        } else {
            let layout = match Layout::array::<mem::MaybeUninit<T>>(len) {
                Ok(l) => l,
                Err(_) => return Err(AllocError),
            };
            alloc.malloc_by(layout).map_err(|_| AllocError)?
        };

        let ptr = meta.as_ptr_of();
        let slice = ptr::slice_from_raw_parts_mut(ptr.as_ptr() as *mut mem::MaybeUninit<T>, len);
        unsafe { Ok(PBox::from_raw_ptr(slice, meta, alloc)) }
    }
    // pub fn try_new_zeroed_slice_in(
    //     len: usize,
    //     alloc: A,
    // ) -> Result<LocalBox<[mem::MaybeUninit<T>], A>, AllocError> {
    //     let ptr = if T::IS_ZST || len == 0 {
    //         NonNull::dangling()
    //     } else {
    //         let layout = match Layout::array::<mem::MaybeUninit<T>>(len) {
    //             Ok(l) => l,
    //             Err(_) => return Err(AllocError),
    //         };
    //         alloc.allocate_zeroed(layout)?.cast()
    //     };
    //     let slice = ptr::slice_from_raw_parts_mut(ptr.as_ptr() as *mut MaybeUninit<T>, len);
    //     unsafe { Ok(LocalBox::from_raw_in(slice, alloc)) }
    // }
}

impl<T, A: MetaAlloc> PBox<mem::MaybeUninit<T>, A> {
    #[inline]
    pub unsafe fn assume_init(self) -> PBox<T, A> {
        let (ptr, meta, alloc) = PBox::into_raw_ptr(self);
        unsafe { PBox::from_raw_ptr(ptr as *mut T, meta, alloc) }
    }

    #[inline]
    pub fn write(self, value: T) -> PBox<T, A> {
        let mut this = self;
        unsafe {
            (*this).write(value);
            this.assume_init()
        }
    }
}

impl<T, A: MetaAlloc> PBox<[mem::MaybeUninit<T>], A> {
    #[inline]
    pub unsafe fn assume_init(self) -> PBox<[T], A> {
        let (ptr, meta, alloc) = PBox::into_raw_ptr(self);
        unsafe { PBox::from_raw_ptr(ptr as *mut [T], meta, alloc) }
    }
}

impl<T, A: MetaAlloc> core::fmt::Debug for PBox<T, A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("LocalBox").field(&self.ptr).finish()
    }
}

const MAX_REFCOUNT: usize = (isize::MAX) as usize;

#[repr(C)]
struct PArcIn<T: ?Sized> {
    rc: AtomicUsize,
    data: T,
}

fn arcin_layout_of(layout: Layout) -> Layout {
    Layout::new::<PArcIn<()>>()
        .extend(layout)
        .unwrap()
        .0
        .pad_to_align()
}

unsafe impl<T: ?Sized + Send> Send for PArcIn<T> {}
unsafe impl<T: ?Sized + Sync> Sync for PArcIn<T> {}

pub struct PArc<T: ?Sized, A: MetaAlloc> {
    ptr: NonNull<PArcIn<T>>,
    meta: Meta,
    alloc: A,
}

unsafe impl<T: ?Sized + Send, A: MetaAlloc + Send> Send for PArc<T, A> {}
unsafe impl<T: ?Sized + Sync, A: MetaAlloc + Sync> Sync for PArc<T, A> {}

impl<T: ?Sized + core::fmt::Debug, A: MetaAlloc> core::fmt::Debug for PArc<T, A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized, A: MetaAlloc> Deref for PArc<T, A> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner().data
    }
}

impl<T: ?Sized, A: MetaAlloc> AsRef<T> for PArc<T, A> {
    fn as_ref(&self) -> &T {
        &**self
    }
}

impl<T: ?Sized, A: MetaAlloc> Unpin for PArc<T, A> {}

impl<T: ?Sized, A: MetaAlloc + Clone> Clone for PArc<T, A> {
    #[inline]
    fn clone(&self) -> Self {
        let old_size = self
            .inner()
            .rc
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        if old_size > MAX_REFCOUNT {
            core::panic!()
        }
        unsafe { Self::from_inner(self.ptr, self.meta.clone(), self.alloc.clone()) }
    }
}

impl<T: ?Sized, A: MetaAlloc> Drop for PArc<T, A> {
    fn drop(&mut self) {
        if self
            .inner()
            .rc
            .fetch_sub(1, core::sync::atomic::Ordering::Release)
            != 1
        {
            return;
        }

        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        unsafe { self.drop_in() }
    }
}

impl<T: ?Sized, A: MetaAlloc> PArc<T, A> {
    #[inline]
    const fn into_raw(a: Self) -> (Meta, A) {
        let a = mem::ManuallyDrop::new(a);
        let m = unsafe { ptr::read(&a.meta) };
        let alloc = unsafe { ptr::read(&a.alloc) };
        (m, alloc)
    }

    #[inline]
    const fn into_raw_ptr(a: Self) -> (NonNull<PArcIn<T>>, Meta, A) {
        let a = mem::ManuallyDrop::new(a);
        let m = unsafe { ptr::read(&a.meta) };
        let alloc = unsafe { ptr::read(&a.alloc) };
        (a.ptr, m, alloc)
    }

    #[inline]
    const unsafe fn from_inner(ptr: NonNull<PArcIn<T>>, meta: Meta, alloc: A) -> Self {
        Self { ptr, meta, alloc }
    }

    #[inline]
    const unsafe fn from_raw_ptr(ptr: *mut PArcIn<T>, meta: Meta, alloc: A) -> Self {
        unsafe { Self::from_inner(NonNull::new_unchecked(ptr), meta, alloc) }
    }
}

impl<T, A: MetaAlloc> PArc<T, A> {
    #[inline]
    pub fn new_in(data: T, alloc: A) -> PArc<T, A> {
        let x: PBox<_, A> = PBox::new_in(
            PArcIn {
                rc: AtomicUsize::new(1),
                data,
            },
            alloc,
        );
        let (ptr, meta, alloc) = PBox::into_raw_ptr(x);
        unsafe { Self::from_raw_ptr(ptr, meta, alloc) }
    }

    #[inline]
    pub fn new_uninit_in(alloc: A) -> PArc<mem::MaybeUninit<T>, A> {
        let (ptr, meta) = unsafe {
            PArc::<_, A>::allocate_by(
                Layout::new::<T>(),
                |layout| alloc.malloc_by(layout),
                |meta| {
                    meta.as_ptr_of::<PArcIn<T>>()
                        .cast::<PArcIn<MaybeUninit<T>>>()
                        .as_ptr()
                },
            )
        };

        unsafe { PArc::from_raw_ptr(ptr, meta, alloc) }
    }

    #[inline]
    pub fn try_new_in(data: T, alloc: A) -> Result<PArc<T, A>, AllocError> {
        let x: PBox<_, A> = PBox::try_new_in(
            PArcIn {
                rc: AtomicUsize::new(1),
                data,
            },
            alloc,
        )?;
        let (ptr, meta, alloc) = PBox::into_raw_ptr(x);
        unsafe { Ok(Self::from_raw_ptr(ptr, meta, alloc)) }
    }

    #[inline]
    pub fn try_new_uninit_in(alloc: A) -> Result<PArc<mem::MaybeUninit<T>, A>, AllocError> {
        let (ptr, meta) = unsafe {
            PArc::<_, A>::try_allocate_by(
                Layout::new::<T>(),
                |layout| alloc.malloc_by(layout),
                |meta| {
                    meta.as_ptr_of::<PArcIn<T>>()
                        .cast::<PArcIn<MaybeUninit<T>>>()
                        .as_ptr()
                },
            )
            .map_err(|_| AllocError)?
        };

        unsafe { Ok(PArc::from_raw_ptr(ptr, meta, alloc)) }
    }
}

impl<T: ?Sized, A: MetaAlloc> PArc<T, A> {
    #[inline]
    fn inner(&self) -> &PArcIn<T> {
        unsafe { self.ptr.as_ref() }
    }

    #[inline]
    pub fn as_ptr(this: &Self) -> *const T {
        let ptr: *mut PArcIn<T> = NonNull::as_ptr(this.ptr);
        unsafe { &raw mut (*ptr).data }
    }

    #[inline(never)]
    unsafe fn drop_in(&mut self) {
        unsafe {
            ptr::drop_in_place(&mut (*self.ptr.as_ptr()).data);
        }
    }

    #[inline]
    unsafe fn allocate_by<E>(
        layout: Layout,
        allocate: impl FnOnce(Layout) -> Result<Meta, E>,
        to_arcin: impl FnOnce(&Meta) -> *mut PArcIn<T>,
    ) -> (*mut PArcIn<T>, Meta) {
        let layout = arcin_layout_of(layout);
        let meta = allocate(layout).unwrap_or_else(|_| handle_alloc_error(layout));
        unsafe { Self::init_arcin(meta, layout, to_arcin) }
    }

    #[inline]
    unsafe fn try_allocate_by<E>(
        layout: Layout,
        allocate: impl FnOnce(Layout) -> Result<Meta, E>,
        to_arcin: impl FnOnce(&Meta) -> *mut PArcIn<T>,
    ) -> Result<(*mut PArcIn<T>, Meta), E> {
        let layout = arcin_layout_of(layout);
        let meta = allocate(layout)?;
        unsafe { Ok(Self::init_arcin(meta, layout, to_arcin)) }
    }

    #[inline]
    unsafe fn init_arcin(
        meta: Meta,
        layout: Layout,
        to_arcin: impl FnOnce(&Meta) -> (*mut PArcIn<T>),
    ) -> (*mut PArcIn<T>, Meta) {
        let inner = to_arcin(&meta);
        debug_assert_eq!(unsafe { Layout::for_value_raw(inner) }, layout);

        unsafe {
            (&raw mut (*inner).rc).write(AtomicUsize::new(1));
        }

        (inner, meta)
    }
}

pub struct Token(Meta);

impl Token {
    fn token<T: ?Sized, A: MetaAlloc>(b: PBox<T, A>) -> Self {
        let (m, _) = PBox::into_raw(b);
        Self(m)
    }
}

unsafe impl Send for Token {}
unsafe impl Sync for Token {}
