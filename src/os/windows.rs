//! The Windows object that stands for a region's bytes and the view of it this process reads: `Section`
//! is a page-file file mapping whose protection is fixed for the whole object at `anonymous`, and
//! `Source::map` opens a view of it, narrowing — never widening — with the request's own `Access`.
//! `from_owned_handle` adopts a section handed over from another process and `borrow` relabels one
//! without a second handle; nothing here mutates shared state. A view is released by `release`, which
//! unmaps it, while `Section`'s own drop only closes this process's handle — the object stays alive for
//! as long as another process holds one, so a caller cannot make a peer's bytes disappear by dropping.
//!
//! Two things this path does not do, both of which the layers above assume of the backend generally.
//! Placement is not asked of the kernel — `MapViewOfFile` takes no address, so where a view lands is the
//! allocator's choice, and `mem::REGION_ALIGN`'s non-unix value of `1 << 16` is the section allocation
//! granularity rather than something arranged here; a pool whose blocks have to sit at the same residue
//! in two processes is therefore relying on the kernel, not on this file. And the two arms keep
//! different reach with the same refusal: the bound a section is created with limits *every* process
//! that maps it, while the Unix arm records the maker's word and can only bind the maker — which is
//! why `Section::map` checks a claim the kernel would enforce anyway, and why a handle adopted from
//! another process records no bound at all here either. None of this has been executed on the host
//! writing it.

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
    /// The access the section was created under, when this process created it. `None` is the received
    /// case: the protection is the object's own and the kernel answers for it, but nobody here can
    /// read it back, so no claim is made about it.
    bound: Option<Access>,
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
            bound: Some(access),
        })
    }

    pub fn from_owned_handle(handle: OwnedHandle) -> Self {
        Self {
            handle,
            bound: None,
        }
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
            bound: self.bound,
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
        // What limits a view is the protection the section was created with, and the kernel enforces
        // that for every process. This first check is the promise this process made itself: a section
        // it made under one word is not mapped under a wider one. A handle that arrived from another
        // process claims nothing, so the kernel is the only one answerable for it.
        if !self
            .bound
            .is_none_or(|bound| bound.contains(request.access))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the mapping asks for access wider than the section was created under",
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
