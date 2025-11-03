
use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    ops::Deref,
    sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering},
};

use crate::header::Metadata;

const FREE: u8 = 0;
const INITIALIZING: u8 = 1;
const ACTIVE: u8 = 2;
const INACTIVE: u8 = 3;

#[repr(C)]
struct Entry<T> {
    data: UnsafeCell<MaybeUninit<T>>,
    rc: AtomicUsize,
    state: AtomicU8,
}

#[repr(transparent)]
struct EntryGuard<'a, T> {
    e: &'a Entry<T>,
}

impl<T> Entry<T> {
    pub const fn null() -> Self {
        Self {
            data: UnsafeCell::new(MaybeUninit::uninit()),
            rc: AtomicUsize::new(0),
            state: AtomicU8::new(FREE),
        }
    }

    unsafe fn get(&self) -> T {
        unsafe { self.data.replace(MaybeUninit::uninit()).assume_init() }
    }

    unsafe fn get_ref(&self) -> &T {
        unsafe { (*self.data.get()).assume_init_ref() }
    }

    unsafe fn write(&self, data: T) {
        unsafe { (*self.data.get()).write(data) };
    }

    pub fn init(&self, data: T) -> Result<(), T> {
        if self
            .state
            .compare_exchange_weak(FREE, INITIALIZING, Ordering::SeqCst, Ordering::Acquire)
            .is_ok()
        {
            return Err(data);
        }

        unsafe {
            self.write(data);
        }

        // state suggests that rc is 0.
        self.rc.store(0, Ordering::Relaxed);
        self.state.store(ACTIVE, Ordering::Release);
        Ok(())
    }

    pub fn acquire<'a>(&'a self) -> Option<EntryGuard<'a, T>> {
        if self.state.load(Ordering::Acquire) == ACTIVE {
            self.rc.fetch_add(1, Ordering::Relaxed);
            Some(EntryGuard { e: self })
        } else {
            None
        }
    }

    pub fn reset<F: FnOnce(T)>(&self, f: F) {
        if self
            .state
            .compare_exchange_weak(INACTIVE, FREE, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            let data = unsafe { self.get() };
            f(data)
        }
    }
}

impl<T> Clone for EntryGuard<'_, T> {
    fn clone(&self) -> Self {
        self.e.rc.fetch_add(1, Ordering::Relaxed);
        Self { e: self.e }
    }
}

impl<T> Drop for EntryGuard<'_, T> {
    fn drop(&mut self) {
        self.e.rc.fetch_sub(1, Ordering::Release);
    }
}

impl<T> Deref for EntryGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.e.get_ref() }
    }
}

#[repr(C)]
struct Registry<T, const N: usize> {
    magic: crate::header::MAGIC,
    counts: AtomicU32,
    entries: [Entry<T>; N],
}

impl<T, const N: usize> core::fmt::Debug for Registry<T, N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Registry")
            .field("entry counts", &self.counts)
            .finish()
    }
}

impl<T, const N: usize> crate::header::Layout for Registry<T, N> {
    type Config = ();
    fn init(&mut self, _cfg: ()) -> crate::header::HeaderStatus {
        self.counts.store(0, Ordering::Relaxed);
        self.entries = [const { Entry::null() }; N];
        self.with_magic();

        crate::header::HeaderStatus::Initialized
    }

    fn attach(&self) -> crate::header::HeaderStatus {
        if self.valid_magic() {
            crate::header::HeaderStatus::Initialized
        } else {
            crate::header::HeaderStatus::Uninitialized
        }
    }
}

impl<T, const N: usize> crate::header::Metadata for Registry<T, N> {
    const MAGIC_VALUE: crate::header::MAGIC = 0x1234;

    fn valid_magic(&self) -> bool {
        self.magic == Self::MAGIC_VALUE
    }

    fn with_magic(&mut self) {
        self.magic = Self::MAGIC_VALUE
    }
}
