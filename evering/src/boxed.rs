use core::alloc::Layout;
use core::mem;
use core::ops::{Deref, DerefMut};
use core::ptr::{self, NonNull};
use core::sync::atomic::AtomicUsize;

use crate::mem::{AllocError, MemAllocator, Meta, handle_alloc_error};
use crate::token::{Token, TokenOf};
use crate::{msg, token};

const fn is_zst<T>() -> bool {
    size_of::<T>() == 0
}

pub struct PBox<T: ?Sized, A: MemAllocator> {
    ptr: NonNull<T>,
    meta: A::Meta,
    alloc: A,
}

unsafe impl<T: ?Sized + Send, A: MemAllocator + Send> Send for PBox<T, A> {}
unsafe impl<T: ?Sized + Sync, A: MemAllocator + Sync> Sync for PBox<T, A> {}

impl<T: core::fmt::Debug + ?Sized, A: MemAllocator> core::fmt::Debug for PBox<T, A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized, A: MemAllocator> Drop for PBox<T, A> {
    fn drop(&mut self) {
        unsafe {
            core::ptr::drop_in_place(self.ptr.as_ptr());
            let layout = Layout::for_value_raw(self.ptr.as_ptr());
            if layout.size() != 0 {
                let meta = mem::replace(&mut self.meta, Meta::null());
                self.alloc.demalloc(meta, layout);
            }
        }
    }
}

impl<T: ?Sized, A: MemAllocator> Deref for PBox<T, A> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T: ?Sized, A: MemAllocator> DerefMut for PBox<T, A> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.ptr.as_mut() }
    }
}

impl<T: ?Sized + token::PointeeIn + msg::Message, A: MemAllocator> PBox<T, A> {
    #[inline]
    pub fn token(self) -> Token<A::Meta> {
        let (token, alloc) = self.token_with();
        mem::forget(alloc);
        token
    }

    #[inline]
    pub fn token_with(self) -> (Token<A::Meta>, A) {
        let (token_of, alloc) = self.token_of_with();
        (token_of.into(), alloc)
    }
}

impl<T: ?Sized + token::PointeeIn, A: MemAllocator> PBox<T, A> {
    #[inline]
    pub fn token_of(self) -> TokenOf<T, A::Meta> {
        let (token, alloc) = self.token_of_with();
        mem::forget(alloc);
        token
    }

    #[inline]
    pub fn token_of_with(self) -> (TokenOf<T, A::Meta>, A) {
        let (ptr, meta, alloc) = Self::into_raw_ptr(self);
        let token = unsafe { TokenOf::from_raw(meta, ptr) };
        (token, alloc)
    }
}

impl<T, A: MemAllocator> PBox<T, A> {
    pub fn new_in(x: T, alloc: A) -> PBox<T, A> {
        let boxed = PBox::new_uninit_in(alloc);
        boxed.write(x)
    }

    pub fn try_new_in(x: T, alloc: A) -> Result<PBox<T, A>, AllocError> {
        let boxed = PBox::try_new_uninit_in(alloc)?;
        Ok(boxed.write(x))
    }

    #[inline]
    pub fn null(alloc: A) -> Self
    where
        A::Meta: Meta,
    {
        let ptr = NonNull::dangling();
        let meta = A::Meta::null();
        unsafe { PBox::from_raw_ptr(ptr.as_ptr(), meta, alloc) }
    }
}

impl<T: ?Sized, A: MemAllocator> PBox<T, A> {
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
    pub fn leak<'a>(b: Self) -> (&'a mut T, A::Meta)
    where
        A: 'a,
    {
        let (ptr, meta, alloc) = PBox::into_raw_ptr(b);
        mem::forget(alloc);
        unsafe { (&mut *ptr, meta) }
    }

    #[inline]
    pub const fn into_raw(b: Self) -> (A::Meta, A) {
        let b = mem::ManuallyDrop::new(b);
        let m = unsafe { ptr::read(&b.meta) };
        let alloc = unsafe { ptr::read(&b.alloc) };
        (m, alloc)
    }

    #[inline]
    pub fn into_raw_ptr(b: Self) -> (*mut T, A::Meta, A) {
        let mut b = mem::ManuallyDrop::new(b);
        let ptr = &raw mut **b;
        let m = unsafe { ptr::read(&b.meta) };
        let alloc = unsafe { ptr::read(&b.alloc) };
        (ptr, m, alloc)
    }

    #[inline]
    pub const unsafe fn from_raw_ptr(ptr: *mut T, meta: A::Meta, alloc: A) -> Self {
        unsafe {
            let ptr = NonNull::new_unchecked(ptr);
            Self { ptr, meta, alloc }
        }
    }

    pub fn drop_in(b: Self) -> A {
        let (ptr, meta, alloc) = Self::into_raw_ptr(b);
        unsafe {
            core::ptr::drop_in_place(ptr);
            let layout = Layout::for_value_raw(ptr);
            if layout.size() != 0 {
                alloc.demalloc(meta, layout);
            }
        }
        alloc
    }
}
impl<T, A: MemAllocator> PBox<[T], A> {
    #[inline]
    pub fn copy_elem(elem: T, len: usize, alloc: A) -> PBox<[T], A>
    where
        T: Clone,
    {
        Self::new_slice_in(len, |_| elem.clone(), alloc)
    }

    #[inline]
    pub fn new_slice_in<F: FnMut(usize) -> T>(len: usize, f: F, alloc: A) -> PBox<[T], A> {
        match Self::try_new_slice_in(len, f, alloc) {
            Ok(b) => b,
            Err(e) => {
                panic!("{}", e)
            }
        }
    }

    #[inline]
    pub fn try_new_slice_in<F: FnMut(usize) -> T>(
        len: usize,
        mut f: F,
        alloc: A,
    ) -> Result<PBox<[T], A>, AllocError> {
        let mut uninit = PBox::try_new_uninit_slice_in(len, alloc)?;
        for (i, elm) in uninit.iter_mut().enumerate() {
            elm.write(f(i));
        }
        Ok(unsafe { uninit.assume_init() })
    }

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
}

impl<T, A: MemAllocator> PBox<mem::MaybeUninit<T>, A> {
    pub fn new_uninit_in(alloc: A) -> Self {
        let layout = Layout::new::<mem::MaybeUninit<T>>();
        // NOTE: Prefer match over unwrap_or_else since closure sometimes not inlineable.
        // That would make code size bigger.
        match PBox::try_new_uninit_in(alloc) {
            Ok(m) => m,
            Err(_) => alloc::alloc::handle_alloc_error(layout),
        }
    }

    pub fn try_new_uninit_in(alloc: A) -> Result<Self, AllocError> {
        if is_zst::<T>() {
            return Ok(PBox::null(alloc));
        }

        let layout = Layout::new::<mem::MaybeUninit<T>>();
        let meta = alloc.malloc_by(layout).map_err(|_| AllocError)?;
        Ok(PBox::from_meta(meta, alloc))
    }

    pub fn from_meta(meta: A::Meta, alloc: A) -> Self {
        let ptr = meta.recall_by(&alloc).cast();
        PBox { ptr, meta, alloc }
    }

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

impl<T, A: MemAllocator> PBox<[mem::MaybeUninit<T>], A> {
    #[inline]
    pub fn new_uninit_slice_in(len: usize, alloc: A) -> PBox<[mem::MaybeUninit<T>], A> {
        match PBox::try_new_uninit_slice_in(len, alloc) {
            Ok(b) => b,
            Err(e) => {
                panic!("{}", e)
            }
        }
    }

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

        let ptr = meta.recall_by(&alloc).cast();
        let slice = ptr::slice_from_raw_parts_mut(ptr.as_ptr(), len);
        unsafe { Ok(PBox::from_raw_ptr(slice, meta, alloc)) }
    }

    #[inline]
    pub unsafe fn assume_init(self) -> PBox<[T], A> {
        let (ptr, meta, alloc) = PBox::into_raw_ptr(self);
        unsafe { PBox::from_raw_ptr(ptr as *mut [T], meta, alloc) }
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

pub struct PArc<T: ?Sized, A: MemAllocator> {
    ptr: NonNull<PArcIn<T>>,
    meta: A::Meta,
    alloc: A,
}

unsafe impl<T: ?Sized + Send, A: MemAllocator + Send> Send for PArc<T, A> {}
unsafe impl<T: ?Sized + Sync, A: MemAllocator + Sync> Sync for PArc<T, A> {}

impl<T: ?Sized + core::fmt::Debug, A: MemAllocator> core::fmt::Debug for PArc<T, A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized, A: MemAllocator> Deref for PArc<T, A> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner().data
    }
}

impl<T: ?Sized, A: MemAllocator> AsRef<T> for PArc<T, A> {
    fn as_ref(&self) -> &T {
        self
    }
}

impl<T: ?Sized, A: MemAllocator> Unpin for PArc<T, A> {}

impl<T: ?Sized, A: MemAllocator + Clone> Clone for PArc<T, A> {
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

impl<T: ?Sized, A: MemAllocator> Drop for PArc<T, A> {
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
        // unsafe { self.drop_in() }
    }
}

impl<T: ?Sized, A: MemAllocator> PArc<T, A> {
    // #[inline]
    // pub fn token(self) -> PArcTokenOf<T, A> {
    //     let (token, alloc) = self.token_with();
    //     mem::forget(alloc);
    //     token
    // }

    // #[inline]
    // pub fn token_with(self) -> (PArcTokenOf<T, A>, A) {
    //     let (ptr, m, alloc) = PArc::into_raw_ptr(self);
    //     let token = unsafe { TokenOf::from_raw(m.forget(), ptr::metadata(ptr.as_ptr())) };
    //     (token, alloc)
    // }

    // #[inline]
    // pub fn detoken(t: PArcTokenOf<T, A>, alloc: A) -> Self {
    //     unsafe {
    //         PArcTokenOf::<T, A>::detokenize(t, alloc, |meta, ptr, alloc| {
    //             PArc::from_inner(NonNull::new_unchecked(ptr), meta, alloc)
    //         })
    //     }
    // }

    #[inline]
    const fn into_raw(a: Self) -> (A::Meta, A) {
        let a = mem::ManuallyDrop::new(a);
        let m = unsafe { ptr::read(&a.meta) };
        let alloc = unsafe { ptr::read(&a.alloc) };
        (m, alloc)
    }

    #[inline]
    const fn into_raw_ptr(a: Self) -> (NonNull<PArcIn<T>>, A::Meta, A) {
        let a = mem::ManuallyDrop::new(a);
        let m = unsafe { ptr::read(&a.meta) };
        let alloc = unsafe { ptr::read(&a.alloc) };
        (a.ptr, m, alloc)
    }

    #[inline]
    const unsafe fn from_inner(ptr: NonNull<PArcIn<T>>, meta: A::Meta, alloc: A) -> Self {
        Self { ptr, meta, alloc }
    }

    #[inline]
    const unsafe fn from_raw_ptr(ptr: *mut PArcIn<T>, meta: A::Meta, alloc: A) -> Self {
        unsafe { Self::from_inner(NonNull::new_unchecked(ptr), meta, alloc) }
    }
}

impl<T, A: MemAllocator> PArc<T, A> {
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
                    meta.recall_by(&alloc)
                        .cast::<PArcIn<mem::MaybeUninit<T>>>()
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
                    meta.recall_by(&alloc)
                        .cast::<PArcIn<mem::MaybeUninit<T>>>()
                        .as_ptr()
                },
            )
            .map_err(|_| AllocError)?
        };

        unsafe { Ok(PArc::from_raw_ptr(ptr, meta, alloc)) }
    }
}

impl<T: ?Sized, A: MemAllocator> PArc<T, A> {
    #[inline]
    fn inner(&self) -> &PArcIn<T> {
        unsafe { self.ptr.as_ref() }
    }

    #[inline]
    pub fn as_ptr(this: &Self) -> *const T {
        let ptr: *mut PArcIn<T> = NonNull::as_ptr(this.ptr);
        unsafe { &raw mut (*ptr).data }
    }

    // #[inline(never)]
    // unsafe fn drop_in(&mut self) {
    //     unsafe {
    //         ptr::drop_in_place(&mut (*self.ptr.as_ptr()).data);

    //         let layout = self.ptr.
    //         let meta = mem::replace(&mut self.meta, Meta::null());
    //         self.alloc.demalloc(meta);
    //     }
    // }

    #[inline]
    unsafe fn allocate_by<E>(
        layout: Layout,
        allocate: impl FnOnce(Layout) -> Result<A::Meta, E>,
        to_arcin: impl FnOnce(&A::Meta) -> *mut PArcIn<T>,
    ) -> (*mut PArcIn<T>, A::Meta) {
        let layout = arcin_layout_of(layout);
        let meta = allocate(layout).unwrap_or_else(|_| handle_alloc_error(layout));
        unsafe { Self::init_arcin(meta, layout, to_arcin) }
    }

    #[inline]
    unsafe fn try_allocate_by<E>(
        layout: Layout,
        allocate: impl FnOnce(Layout) -> Result<A::Meta, E>,
        to_arcin: impl FnOnce(&A::Meta) -> *mut PArcIn<T>,
    ) -> Result<(*mut PArcIn<T>, A::Meta), E> {
        let layout = arcin_layout_of(layout);
        let meta = allocate(layout)?;
        unsafe { Ok(Self::init_arcin(meta, layout, to_arcin)) }
    }

    #[inline]
    unsafe fn init_arcin(
        meta: A::Meta,
        layout: Layout,
        to_arcin: impl FnOnce(&A::Meta) -> *mut PArcIn<T>,
    ) -> (*mut PArcIn<T>, A::Meta) {
        let inner = to_arcin(&meta);
        debug_assert_eq!(unsafe { Layout::for_value_raw(inner) }, layout);

        unsafe {
            (&raw mut (*inner).rc).write(AtomicUsize::new(1));
        }

        (inner, meta)
    }
}

// type ATokenOf<T, A> = TokenOf<T, SpanOf<MetaOf<A>>>;
// type PArcTokenOf<T, A> = ATokenOf<PArcIn<T>, A>;

// No static reflection, can't safely erase `Metadata` type
// while retains information
// pub struct TokenOf<T: ?Sized + ptr::Pointee, M> {
//     span: M,
//     metadata: T::Metadata,
// }

// impl<T: ?Sized + ptr::Pointee, M> TokenOf<T, M> {
//     #[inline(always)]
//     const unsafe fn from_raw(span: M, metadata: T::Metadata) -> Self {
//         TokenOf { span, metadata }
//     }

//     #[inline(always)]
//     const unsafe fn from_ptr(span: M, ptr: *const T) -> Self {
//         let metadata = ptr::metadata(ptr);
//         TokenOf { span, metadata }
//     }

//     unsafe fn detokenize<A: MemAllocator, Out>(
//         t: TokenOf<T, SpanOf<A::Meta>>,
//         alloc: A,
//         f: impl FnOnce(A::Meta, *mut T, A) -> Out,
//     ) -> Out {
//         let TokenOf { span, metadata } = t;
//         let meta = unsafe { A::Meta::resolve(span, alloc.base_ptr()) };
//         let thin_ptr = meta.as_uninit::<u8>();
//         let ptr = ptr::from_raw_parts_mut::<T>(thin_ptr.as_ptr(), metadata);
//         f(meta, ptr, alloc)
//     }
// }
//
