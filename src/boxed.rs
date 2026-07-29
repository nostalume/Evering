use core::alloc::Layout;
use core::mem;
use core::ops::{Deref, DerefMut};
use core::ptr::{self, NonNull};

use crate::mem::{AllocError, MemAllocator, Meta, TransferAllocator};
use crate::token;
use crate::token::TokenOf;

const fn is_zst<T>() -> bool {
    size_of::<T>() == 0
}

#[doc(hidden)]
// Generic only inside the crate; public ownership is `PBox<'a, T>`.
pub struct PBoxIn<T: ?Sized, A: MemAllocator> {
    ptr: NonNull<T>,
    meta: A::Meta,
    alloc: A,
}

unsafe impl<T: ?Sized + Send, A: MemAllocator + Send> Send for PBoxIn<T, A> {}
unsafe impl<T: ?Sized + Sync, A: MemAllocator + Sync> Sync for PBoxIn<T, A> {}

impl<T: core::fmt::Debug + ?Sized, A: MemAllocator> core::fmt::Debug for PBoxIn<T, A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized, A: MemAllocator> Drop for PBoxIn<T, A> {
    fn drop(&mut self) {
        unsafe {
            let layout = Layout::for_value_raw(self.ptr.as_ptr());
            if layout.size() == 0 {
                core::ptr::drop_in_place(self.ptr.as_ptr());
                return;
            }
            let meta = mem::replace(&mut self.meta, Meta::null());
            if let Err(meta) = self.alloc.release(self.ptr.as_ptr(), meta, layout) {
                mem::forget(meta);
            }
        }
    }
}

impl<T: ?Sized, A: MemAllocator> Deref for PBoxIn<T, A> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T: ?Sized, A: MemAllocator> DerefMut for PBoxIn<T, A> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.ptr.as_mut() }
    }
}

impl<T: ?Sized + token::Shape, A: TransferAllocator> PBoxIn<T, A> {
    #[inline]
    pub fn token_of(self) -> TokenOf<T, A::Meta> {
        let (token, alloc) = self.token_of_with();
        mem::forget(alloc);
        token
    }

    #[inline]
    pub fn token_of_with(self) -> (TokenOf<T, A::Meta>, A) {
        let (ptr, meta, alloc) = Self::into_raw_ptr(self);
        let token = unsafe { TokenOf::from_raw(alloc.layout_id(), meta, ptr) };
        (token, alloc)
    }
}

impl<T, A: MemAllocator> PBoxIn<T, A> {
    #[cfg(test)]
    pub(crate) fn new_in(x: T, alloc: A) -> PBoxIn<T, A> {
        let boxed = PBoxIn::new_uninit_in(alloc);
        boxed.write(x)
    }

    #[cfg(test)]
    pub(crate) fn try_new_in(x: T, alloc: A) -> Result<PBoxIn<T, A>, AllocError> {
        let boxed = PBoxIn::try_new_uninit_in(alloc)?;
        Ok(boxed.write(x))
    }

    #[inline]
    pub fn null(alloc: A) -> Self
    where
        A::Meta: Meta,
    {
        let ptr = NonNull::dangling();
        let meta = A::Meta::null();
        unsafe { PBoxIn::from_raw_ptr(ptr.as_ptr(), meta, alloc) }
    }
}

impl<T: ?Sized, A: MemAllocator> PBoxIn<T, A> {
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

    pub fn release(self) -> Result<A, Self> {
        let (ptr, meta, alloc) = Self::into_raw_ptr(self);
        let layout = unsafe { Layout::for_value_raw(ptr) };
        if layout.size() == 0 {
            unsafe { core::ptr::drop_in_place(ptr) };
            return Ok(alloc);
        }
        match unsafe { alloc.release(ptr, meta, layout) } {
            Ok(()) => Ok(alloc),
            Err(meta) => Err(unsafe { Self::from_raw_ptr(ptr, meta, alloc) }),
        }
    }
}
impl<T, A: MemAllocator> PBoxIn<[T], A> {
    #[inline]
    pub(crate) fn try_new_slice_in<F: FnMut(usize) -> T>(
        len: usize,
        mut f: F,
        alloc: A,
    ) -> Result<PBoxIn<[T], A>, AllocError> {
        let mut uninit = PBoxIn::try_new_uninit_slice_in(len, alloc)?;
        for (i, elm) in uninit.iter_mut().enumerate() {
            elm.write(f(i));
        }
        Ok(unsafe { uninit.assume_init() })
    }
}

impl<T, A: MemAllocator> PBoxIn<mem::MaybeUninit<T>, A> {
    #[cfg(test)]
    pub(crate) fn new_uninit_in(alloc: A) -> Self {
        let layout = Layout::new::<mem::MaybeUninit<T>>();
        // NOTE: Prefer match over unwrap_or_else since closure sometimes not inlineable.
        // That would make code size bigger.
        match PBoxIn::try_new_uninit_in(alloc) {
            Ok(m) => m,
            Err(_) => alloc::alloc::handle_alloc_error(layout),
        }
    }

    pub(crate) fn try_new_uninit_in(alloc: A) -> Result<Self, AllocError> {
        if is_zst::<T>() {
            return Ok(PBoxIn::null(alloc));
        }

        let layout = Layout::new::<mem::MaybeUninit<T>>();
        let meta = alloc.alloc(layout).map_err(|_| AllocError)?;
        Ok(PBoxIn::from_meta(meta, alloc))
    }

    pub fn from_meta(meta: A::Meta, alloc: A) -> Self {
        let ptr = meta.recall_by(&alloc).cast();
        PBoxIn { ptr, meta, alloc }
    }

    #[inline]
    pub unsafe fn assume_init(self) -> PBoxIn<T, A> {
        let (ptr, meta, alloc) = PBoxIn::into_raw_ptr(self);
        unsafe { PBoxIn::from_raw_ptr(ptr as *mut T, meta, alloc) }
    }

    #[inline]
    pub fn write(self, value: T) -> PBoxIn<T, A> {
        let mut this = self;
        unsafe {
            (*this).write(value);
            this.assume_init()
        }
    }
}

impl<T, A: MemAllocator> PBoxIn<[mem::MaybeUninit<T>], A> {
    pub(crate) fn try_new_uninit_slice_in(
        len: usize,
        alloc: A,
    ) -> Result<PBoxIn<[mem::MaybeUninit<T>], A>, AllocError> {
        let meta = if is_zst::<T>() || len == 0 {
            Meta::null()
        } else {
            let layout = match Layout::array::<mem::MaybeUninit<T>>(len) {
                Ok(l) => l,
                Err(_) => return Err(AllocError),
            };
            alloc.alloc(layout).map_err(|_| AllocError)?
        };

        let ptr = meta.recall_by(&alloc).cast();
        let slice = ptr::slice_from_raw_parts_mut(ptr.as_ptr(), len);
        unsafe { Ok(PBoxIn::from_raw_ptr(slice, meta, alloc)) }
    }

    #[inline]
    pub unsafe fn assume_init(self) -> PBoxIn<[T], A> {
        let (ptr, meta, alloc) = PBoxIn::into_raw_ptr(self);
        unsafe { PBoxIn::from_raw_ptr(ptr as *mut [T], meta, alloc) }
    }
}

pub type PBox<'a, T> = PBoxIn<T, crate::talc::RefTalc<'a>>;

#[cfg(test)]
mod release_tests {
    use super::PBoxIn;
    use crate::{
        SchemaKey,
        mem::{MemAlloc, MemDealloc, Meta},
        msg::Repr,
    };
    use core::{
        alloc::Layout,
        ptr::NonNull,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    #[derive(Clone, Copy)]
    struct TestMeta {
        pointer: usize,
        size: usize,
        align: usize,
    }

    unsafe impl Repr for TestMeta {
        const SCHEMA: SchemaKey = SchemaKey::new(crate::SchemaId(0x5445_5354_4d45_5441), 1);
    }

    impl Meta for TestMeta {
        fn null() -> Self {
            Self {
                pointer: 1,
                size: 0,
                align: 1,
            }
        }

        fn is_null(&self) -> bool {
            self.size == 0
        }

        unsafe fn recall(&self, _: *const u8) -> NonNull<u8> {
            unsafe { NonNull::new_unchecked(self.pointer as *mut u8) }
        }

        fn layout_bytes(&self) -> Layout {
            Layout::from_size_align(self.size, self.align).unwrap()
        }
    }

    struct FailOnce {
        fail: &'static AtomicBool,
        releases: &'static AtomicUsize,
    }

    unsafe impl MemAlloc for FailOnce {
        type Meta = TestMeta;
        type Error = ();

        fn base_ptr(&self) -> *const u8 {
            core::ptr::null()
        }

        fn alloc(&self, layout: Layout) -> Result<Self::Meta, Self::Error> {
            let pointer = unsafe { alloc::alloc::alloc(layout) };
            NonNull::new(pointer)
                .map(|pointer| TestMeta {
                    pointer: pointer.addr().get(),
                    size: layout.size(),
                    align: layout.align(),
                })
                .ok_or(())
        }
    }

    unsafe impl MemDealloc for FailOnce {
        fn dealloc(&self, meta: Self::Meta, layout: Layout) -> Result<(), Self::Meta> {
            if !self.fail.swap(false, Ordering::AcqRel) {
                unsafe { alloc::alloc::dealloc(meta.pointer as *mut u8, layout) };
                Ok(())
            } else {
                Err(meta)
            }
        }

        unsafe fn release<T: ?Sized>(
            &self,
            pointer: *mut T,
            meta: Self::Meta,
            layout: Layout,
        ) -> Result<(), Self::Meta> {
            self.releases.fetch_add(1, Ordering::Relaxed);
            if self.fail.swap(false, Ordering::AcqRel) {
                return Err(meta);
            }
            unsafe {
                core::ptr::drop_in_place(pointer);
                alloc::alloc::dealloc(meta.pointer as *mut u8, layout);
            }
            Ok(())
        }
    }

    impl crate::mem::MemAllocator for FailOnce {}

    #[test]
    fn failed_release_returns_the_exact_live_box() {
        static FAIL: AtomicBool = AtomicBool::new(true);
        static RELEASES: AtomicUsize = AtomicUsize::new(0);
        RELEASES.store(0, Ordering::Relaxed);
        let value = PBoxIn::new_in(
            41_u64,
            FailOnce {
                fail: &FAIL,
                releases: &RELEASES,
            },
        );

        let value = match value.release() {
            Err(value) => value,
            Ok(_) => panic!("first release must be rejected"),
        };
        assert_eq!(*value, 41);
        assert!(value.release().is_ok());
        assert_eq!(RELEASES.load(Ordering::Relaxed), 2);
    }

    struct Droppy(&'static AtomicUsize);

    impl Drop for Droppy {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn rejected_drop_attempts_once_and_leaks_the_live_value() {
        static FAIL: AtomicBool = AtomicBool::new(true);
        static RELEASES: AtomicUsize = AtomicUsize::new(0);
        static DROPS: AtomicUsize = AtomicUsize::new(0);
        RELEASES.store(0, Ordering::Relaxed);
        DROPS.store(0, Ordering::Relaxed);

        drop(PBoxIn::new_in(
            Droppy(&DROPS),
            FailOnce {
                fail: &FAIL,
                releases: &RELEASES,
            },
        ));

        assert_eq!(RELEASES.load(Ordering::Relaxed), 1);
        assert_eq!(DROPS.load(Ordering::Relaxed), 0);
    }

    struct Zst;

    impl Drop for Zst {
        fn drop(&mut self) {
            DROPPED_ZST.fetch_add(1, Ordering::Relaxed);
        }
    }

    static DROPPED_ZST: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn zero_sized_release_drops_without_allocator_mutation() {
        static FAIL: AtomicBool = AtomicBool::new(true);
        static RELEASES: AtomicUsize = AtomicUsize::new(0);
        DROPPED_ZST.store(0, Ordering::Relaxed);
        RELEASES.store(0, Ordering::Relaxed);

        assert!(
            PBoxIn::new_in(
                Zst,
                FailOnce {
                    fail: &FAIL,
                    releases: &RELEASES,
                },
            )
            .release()
            .is_ok()
        );
        assert_eq!(DROPPED_ZST.load(Ordering::Relaxed), 1);
        assert_eq!(RELEASES.load(Ordering::Relaxed), 0);
    }
}
