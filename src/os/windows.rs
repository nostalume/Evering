#[cfg(feature = "process")]
pub mod process;

use std::{
    io,
    os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
};

use core::ptr::NonNull;
use windows_sys::Win32::{
    Foundation::INVALID_HANDLE_VALUE,
    System::Memory::{
        CreateFileMappingW, FILE_MAP_EXECUTE, FILE_MAP_READ, FILE_MAP_WRITE, MapViewOfFile,
        PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE, PAGE_READONLY, PAGE_READWRITE, UnmapViewOfFile,
    },
};

use crate::mem::{Access, Map, Request, Source};

pub struct Section<F: AsHandle> {
    handle: F,
}

impl Section<OwnedHandle> {
    pub fn anonymous(size: usize, access: Access) -> io::Result<Self> {
        if size == 0 || !access.contains(Access::READ) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "section requires nonzero readable extent",
            ));
        }
        let protection = match (
            access.contains(Access::EXEC),
            access.contains(Access::WRITE),
        ) {
            (true, true) => PAGE_EXECUTE_READWRITE,
            (true, false) => PAGE_EXECUTE_READ,
            (false, true) => PAGE_READWRITE,
            (false, false) => PAGE_READONLY,
        };
        let size = size as u64;
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                core::ptr::null(),
                protection,
                (size >> 32) as u32,
                size as u32,
                core::ptr::null(),
            )
        };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            handle: unsafe { OwnedHandle::from_raw_handle(handle.cast()) },
        })
    }

    pub fn from_owned_handle(handle: OwnedHandle) -> Self {
        Self { handle }
    }
}

impl<F: AsHandle> AsHandle for Section<F> {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.handle.as_handle()
    }
}

impl<F: AsHandle> Section<F> {
    pub fn borrow(&self) -> Section<BorrowedHandle<'_>> {
        Section {
            handle: self.handle.as_handle(),
        }
    }
}

unsafe impl<F: AsHandle> Source for Section<F> {
    type Error = io::Error;

    fn map(self, request: Request) -> Result<Map, Self::Error> {
        if request.len == 0 || !request.access.contains(Access::READ) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "mapping requires nonzero readable extent",
            ));
        }
        let mut access = FILE_MAP_READ;
        if request.access.contains(Access::WRITE) {
            access |= FILE_MAP_WRITE;
        }
        if request.access.contains(Access::EXEC) {
            access |= FILE_MAP_EXECUTE;
        }
        let view = unsafe {
            MapViewOfFile(
                self.handle.as_handle().as_raw_handle().cast(),
                access,
                0,
                0,
                request.len,
            )
        };
        let start = NonNull::new(view.Value.cast()).ok_or_else(io::Error::last_os_error)?;
        Ok(unsafe { Map::from_raw_parts(start, request.len, request.access, release) })
    }
}

unsafe fn release(start: NonNull<u8>, _: usize) -> bool {
    let view = windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS {
        Value: start.as_ptr().cast(),
    };
    unsafe { UnmapViewOfFile(view) != 0 }
}
