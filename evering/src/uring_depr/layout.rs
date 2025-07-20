use core::{alloc::Layout, ptr::NonNull};

pub(crate) unsafe fn alloc_buffer<T>(size: usize) -> NonNull<T> {
    let layout = Layout::array::<T>(size).unwrap();
    NonNull::new(unsafe { alloc::alloc::alloc(layout) })
        .unwrap_or_else(|| alloc::alloc::handle_alloc_error(layout))
        .cast()
}

pub(crate) unsafe fn alloc_buffer_2<T>(size: usize) -> NonNull<[T]> {
    let layout = Layout::array::<T>(size)
        .ok()
        .unwrap_or_else(|| alloc::alloc::handle_alloc_error(Layout::array::<T>(size).unwrap()));

    let ptr = NonNull::new(unsafe { alloc::alloc::alloc(layout) } as *mut T)
        .unwrap_or_else(|| alloc::alloc::handle_alloc_error(layout));
    return NonNull::slice_from_raw_parts(ptr, size);
}

pub(crate) unsafe fn alloc<T>() -> NonNull<T> {
    let layout = Layout::new::<T>();
    NonNull::new(unsafe { alloc::alloc::alloc(layout) })
        .unwrap_or_else(|| alloc::alloc::handle_alloc_error(layout))
        .cast()
}

pub(crate) unsafe fn dealloc_buffer<T>(ptr: NonNull<T>, size: usize) {
    let layout = Layout::array::<T>(size).unwrap();
    unsafe { alloc::alloc::dealloc(ptr.as_ptr().cast(), layout) }
}

pub(crate) unsafe fn dealloc<T>(ptr: NonNull<T>) {
    let layout = Layout::new::<T>();
    unsafe { alloc::alloc::dealloc(ptr.as_ptr().cast(), layout) }
}
