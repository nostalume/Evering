use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering},
    task::{Context, Waker},
};

use spin::Mutex;

use alloc::sync::Arc;

mod state {
	// FREE -> WAKER -> COMPLETED -> FREE
	/// FREE: at initiation
    pub const FREE: u8 = 0;
	/// WAKER: with `waker`, without `payload`
	pub const WAKER:u8 = 1;
	/// COMPLETED: with `payload`, possibly with `waker`
    pub const COMPLETED: u8 = 3;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Id {
    idx: usize,
    live: u32,
}

struct Cache<T> {
    next_free: AtomicUsize,
    live: AtomicU32,
    state: AtomicU8,
    lock: Mutex<()>,
    waker: UnsafeCell<MaybeUninit<Waker>>,
    payload: UnsafeCell<MaybeUninit<T>>,
}

unsafe impl<T: Send> Send for Cache<T> {}
unsafe impl<T: Sync> Sync for Cache<T> {}

// impl<T> Cache<T> {
//     pub const fn null(next_free: usize) -> Self {
//         Self {
//             next_free: AtomicUsize::new(next_free),
//             live: AtomicU32::new(0),
//             state: AtomicU8::new(state::FREE),
//             lock: Mutex::new(()),
//             waker: UnsafeCell::new(MaybeUninit::uninit()),
//             payload: UnsafeCell::new(MaybeUninit::uninit()),
//         }
//     }

//     unsafe fn write_waker(&self, ctx: &Context<'_>) {
//         let new = ctx.waker().clone();

//         let mut prev = self.state.load(Ordering::Acquire);
//         loop {
//             match prev {
//                 state::INIT | state::WAITING => {
//                     match self.state.compare_exchange_weak(
//                         prev,
//                         state::WAITING,
//                         Ordering::AcqRel,
//                         Ordering::Acquire,
//                     ) {
//                         Ok(_) => break,
//                         Err(cur) => {
//                             prev = cur;
//                             continue;
//                         }
//                     }
//                 }
//                 // state::Completed: payload already exist
//                 _ => return,
//             }
//         }

//         let _ = self.lock.lock();
//         let old = unsafe { self.waker.replace(MaybeUninit::new(new)) };
//         if prev == WAITING {
//             let old = unsafe { old.assume_init() };
//             drop(old)
//         }
//     }

//     unsafe fn write_payload(&self, value: T) {
//         unsafe { self.payload.replace(MaybeUninit::new(value)) };
//         let prev = self.state.swap(state::COMPLETED, Ordering::AcqRel);

//         if prev == state::WAITING {
//             let _ = self.lock.lock();
//             let old = unsafe { self.waker.replace(MaybeUninit::uninit()) };
//             let old = unsafe { old.assume_init() };
//             old.wake()
//         }
//     }
	
// 	unsafe fn take_payload(&self) -> Option<T> {
// 		if self.state.compare_exchange(state::COMPLETED, state::TAKING, Ordering::AcqRel, Ordering::Acquire).is_err() {
// 			return None
// 		}

// 		let val = unsafe { self.payload.replace(MaybeUninit::uninit()) };
// 		let val = unsafe { val.assume_init() };
// 		self.state.store(state::FREE,Ordering::Release);
// 		Some(val)
// 	}
// }

pub struct CachePool<T, const N: usize> {
    inits: AtomicUsize,
    free_head: AtomicUsize,
    entries: [Cache<T>; N],
}

// pub struct Pool<const N: usize>(Arc<Registry<>>)
