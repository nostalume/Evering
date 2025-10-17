


// #[repr(C)]
// pub struct ShmBox<T: ?Sized, A: MemBase>(NonNull<T>, A);

// impl<T: ?Sized, A: MemBase> Drop for ShmBox<T, A> {
//     fn drop(&mut self) {
//         let ptr = self.0;

//         unsafe {
//             let layout = Layout::for_value_raw(ptr.as_ptr());
//             if layout.size() != 0 {
//                 self.1.deallocate(ptr.cast(), layout);
//             }
//         }
//     }
// }

// impl<T, A: MemBase> ShmBox<T, A> {
//     pub fn take(self) -> T {
//         let ShmBox(ptr, _) = self;
//         unsafe { ptr::read(ptr.as_ptr()) }
//     }

//     pub fn into_token(self) -> ShmToken<T, A, ShmSized> {
//         self.into()
//     }

//     pub fn new_in(x: T, alloc: A) -> ShmBox<T, A> {
//         let boxed = Self::new_uninit_in(alloc);
//         boxed.write(x)
//     }

//     pub fn try_new_in(x: T, alloc: A) -> Result<ShmBox<T, A>, AllocError> {
//         let boxed = Self::try_new_uninit_in(alloc)?;
//         Ok(boxed.write(x))
//     }

//     pub fn new_uninit_in(alloc: A) -> ShmBox<MaybeUninit<T>, A> {
//         let layout = Layout::new::<mem::MaybeUninit<T>>();
//         // NOTE: Prefer match over unwrap_or_else since closure sometimes not inlineable.
//         // That would make code size bigger.
//         match ShmBox::try_new_uninit_in(alloc) {
//             Ok(m) => m,
//             Err(_) => handle_alloc_error(layout),
//         }
//     }

//     pub fn try_new_uninit_in(alloc: A) -> Result<ShmBox<MaybeUninit<T>, A>, AllocError> {
//         let ptr = if T::IS_ZST {
//             NonNull::dangling()
//         } else {
//             let layout = Layout::new::<MaybeUninit<T>>();
//             alloc.allocate(layout)?.cast()
//         };
//         unsafe { Ok(ShmBox::from_raw_in(ptr.as_ptr(), alloc)) }
//     }

//     pub fn new_zeroed_in(alloc: A) -> ShmBox<MaybeUninit<T>, A> {
//         let layout = Layout::new::<mem::MaybeUninit<T>>();
//         // NOTE: Prefer match over unwrap_or_else since closure sometimes not inlineable.
//         // That would make code size bigger.
//         match ShmBox::try_new_zeroed_in(alloc) {
//             Ok(m) => m,
//             Err(_) => handle_alloc_error(layout),
//         }
//     }

//     pub fn try_new_zeroed_in(alloc: A) -> Result<ShmBox<MaybeUninit<T>, A>, AllocError> {
//         let ptr = if T::IS_ZST {
//             NonNull::dangling()
//         } else {
//             let layout = Layout::new::<mem::MaybeUninit<T>>();
//             alloc.allocate_zeroed(layout)?.cast()
//         };
//         unsafe { Ok(ShmBox::from_raw_in(ptr.as_ptr(), alloc)) }
//     }

//     pub fn pin_in(x: T, alloc: A) -> Pin<ShmBox<T, A>> {
//         ShmBox::into_pin(ShmBox::new_in(x, alloc))
//     }
// }

// impl<T: ?Sized, A: MemBase> ShmBox<T, A> {
//     pub fn into_pin(self) -> Pin<ShmBox<T, A>> {
//         unsafe { Pin::new_unchecked(self) }
//     }

//     pub fn as_ref(&self) -> &T {
//         self
//     }

//     pub fn as_ptr(&self) -> *const T {
//         &raw const **self
//     }

//     pub fn as_mut_ptr(&mut self) -> *mut T {
//         &raw mut **self
//     }

//     pub fn allocator(&self) -> &A {
//         &self.1
//     }

//     pub unsafe fn from_raw_in(raw: *mut T, alloc: A) -> Self {
//         ShmBox(unsafe { NonNull::new_unchecked(raw) }, alloc)
//     }

//     pub fn into_raw_with_allocator(b: Self) -> (*mut T, A) {
//         let mut b = mem::ManuallyDrop::new(b);
//         // We carefully get the raw pointer out in a way that Miri's aliasing model understands what
//         // is happening: using the primitive "deref" of `Box`. In case `A` is *not* `Global`, we
//         // want *no* aliasing requirements here!
//         // In case `A` *is* `Global`, this does not quite have the right behavior; `into_raw`
//         // works around that.
//         let ptr = &raw mut **b;
//         let alloc = unsafe { ptr::read(&b.1) };
//         (ptr, alloc)
//     }
// }

// impl<T, A: MemBase> ShmBox<[T], A> {
//     pub fn into_token(self) -> ShmToken<T, A, ShmSlice> {
//         self.into()
//     }

//     pub fn copy_from_slice(src: &[T], alloc: A) -> ShmBox<[T], A> {
//         let len = src.len();
//         let mut m = ShmBox::new_uninit_slice_in(len, alloc);
//         unsafe {
//             let dst = m.as_mut_ptr().as_mut_ptr();
//             let src_ = src.as_ptr().cast();
//             dst.copy_from_nonoverlapping(src_, len);
//             m.assume_init()
//         }
//     }

//     pub fn new_uninit_slice_in(len: usize, alloc: A) -> ShmBox<[MaybeUninit<T>], A> {
//         let m = ShmBox::try_new_uninit_slice_in(len, alloc);
//         m.unwrap()
//     }

//     pub fn new_zeroed_slice_in(len: usize, alloc: A) -> ShmBox<[MaybeUninit<T>], A> {
//         let m = ShmBox::try_new_zeroed_slice_in(len, alloc);
//         m.unwrap()
//     }

//     pub fn try_new_uninit_slice_in(
//         len: usize,
//         alloc: A,
//     ) -> Result<ShmBox<[MaybeUninit<T>], A>, AllocError> {
//         let ptr = if T::IS_ZST || len == 0 {
//             NonNull::dangling()
//         } else {
//             let layout = match Layout::array::<mem::MaybeUninit<T>>(len) {
//                 Ok(l) => l,
//                 Err(_) => return Err(AllocError),
//             };
//             alloc.allocate(layout)?.cast()
//         };
//         let slice = ptr::slice_from_raw_parts_mut(ptr.as_ptr() as *mut MaybeUninit<T>, len);
//         unsafe { Ok(ShmBox::from_raw_in(slice, alloc)) }
//     }

//     pub fn try_new_zeroed_slice_in(
//         len: usize,
//         alloc: A,
//     ) -> Result<ShmBox<[MaybeUninit<T>], A>, AllocError> {
//         let ptr = if T::IS_ZST || len == 0 {
//             NonNull::dangling()
//         } else {
//             let layout = match Layout::array::<mem::MaybeUninit<T>>(len) {
//                 Ok(l) => l,
//                 Err(_) => return Err(AllocError),
//             };
//             alloc.allocate_zeroed(layout)?.cast()
//         };
//         let slice = ptr::slice_from_raw_parts_mut(ptr.as_ptr() as *mut MaybeUninit<T>, len);
//         unsafe { Ok(ShmBox::from_raw_in(slice, alloc)) }
//     }
// }

// impl<T, A: MemBase> ShmBox<MaybeUninit<T>, A> {
//     pub unsafe fn assume_init(self) -> ShmBox<T, A> {
//         let (raw, alloc) = ShmBox::into_raw_with_allocator(self);
//         unsafe { ShmBox::from_raw_in(raw as *mut T, alloc) }
//     }

//     pub fn write(self, value: T) -> ShmBox<T, A> {
//         let mut this = self;
//         unsafe {
//             (*this).write(value);
//             this.assume_init()
//         }
//     }
// }

// impl<T, A: MemBase> ShmBox<[MaybeUninit<T>], A> {
//     pub unsafe fn assume_init(self) -> ShmBox<[T], A> {
//         let (raw, alloc) = ShmBox::into_raw_with_allocator(self);
//         unsafe { ShmBox::from_raw_in(raw as *mut [T], alloc) }
//     }
// }

// impl<T, A: MemBase> Debug for ShmBox<T, A> {
//     fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
//         f.debug_tuple("ShmBox").field(&self.0).finish()
//     }
// }

// impl<T: ?Sized, A: MemBase> Deref for ShmBox<T, A> {
//     type Target = T;
//     fn deref(&self) -> &Self::Target {
//         unsafe { self.0.as_ref() }
//     }
// }

// impl<T: ?Sized, A: MemBase> DerefMut for ShmBox<T, A> {
//     fn deref_mut(&mut self) -> &mut Self::Target {
//         unsafe { self.0.as_mut() }
//     }
// }

// unsafe impl<T, A: MemBase> Send for ShmBox<T, A> {}
// unsafe impl<T: Sync, A: MemBase> Sync for ShmBox<T, A> {}

// pub trait AsShmToken: Sealed {}
// pub struct ShmSized;
// impl Sealed for ShmSized {}
// impl AsShmToken for ShmSized {}
// pub struct ShmSlice(usize);
// impl Sealed for ShmSlice {}
// impl AsShmToken for ShmSlice {}

// /// A token that can be transferred between processes.
// pub struct ShmToken<T, A: MemBase, S: AsShmToken>(isize, A, S, PhantomData<T>);

// impl<T, A: MemBase> ShmToken<T, A, ShmSized> {
//     pub fn into_box(self) -> ShmBox<T, A> {
//         self.into()
//     }
//     /// Returns the offset to the start memory region of the allocator.
//     pub fn offset(&self) -> isize {
//         self.0
//     }
// }

// impl<T, A: MemBase> ShmToken<T, A, ShmSlice> {
//     pub fn into_box(self) -> ShmBox<[T], A> {
//         self.into()
//     }
//     pub fn len(&self) -> usize {
//         self.2.0
//     }
// }

// /// Safety: ShmToken is invariant across process.
// unsafe impl<T, A: MemBase, S: AsShmToken> Send for ShmToken<T, A, S> {}
// unsafe impl<T: Sync, A: MemBase, S: AsShmToken> Sync for ShmToken<T, A, S> {}

// impl<T, A: MemBase, S: AsShmToken> Debug for ShmToken<T, A, S> {
//     fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
//         f.debug_tuple("ShmToken").field(&self.0).finish()
//     }
// }

// impl<T, A: MemBase> From<ShmBox<T, A>> for ShmToken<T, A, ShmSized> {
//     fn from(value: ShmBox<T, A>) -> Self {
//         let (ptr, allocator) = ShmBox::into_raw_with_allocator(value);

//         // Safety: the ptr is allocated by the allocator
//         let offset = unsafe { allocator.offset(ptr) };

//         ShmToken(offset, allocator, ShmSized, PhantomData)
//     }
// }

// impl<T, A: MemBase> From<ShmToken<T, A, ShmSized>> for ShmBox<T, A> {
//     fn from(value: ShmToken<T, A, ShmSized>) -> Self {
//         let ShmToken(offset, alloc, _, _) = value;

//         let ptr = unsafe { alloc.get_aligned_ptr_mut::<T>(offset) };
//         unsafe { ShmBox::from_raw_in(ptr.as_ptr(), alloc) }
//     }
// }

// impl<T, A: MemBase> From<ShmBox<[T], A>> for ShmToken<T, A, ShmSlice> {
//     fn from(value: ShmBox<[T], A>) -> Self {
//         let len = value.len();
//         let (ptr, allocator) = ShmBox::into_raw_with_allocator(value);

//         let offset = unsafe { allocator.offset(ptr) };
//         ShmToken(offset, allocator, ShmSlice(len), PhantomData)
//     }
// }

// impl<T, A: MemBase> From<ShmToken<T, A, ShmSlice>> for ShmBox<[T], A> {
//     fn from(value: ShmToken<T, A, ShmSlice>) -> Self {
//         let ShmToken(offset, alloc, l, _) = value;
//         let len = l.0;

//         let ptr = unsafe { alloc.get_aligned_slice_mut(offset, len) };
//         unsafe { ShmBox::from_raw_in(ptr.as_ptr(), alloc) }
//     }
// }
