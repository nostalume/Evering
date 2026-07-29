use std::{
    io,
    os::windows::io::{AsRawHandle, RawHandle},
    sync::Arc,
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::Threading::{CreateEventW, ResetEvent, SetEvent},
};

use crate::Notify;

#[derive(Debug)]
struct Handle(HANDLE);

// A kernel handle can be used concurrently by SetEvent, ResetEvent, and wait
// registration until the last local owner closes it.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

#[derive(Clone, Debug)]
pub struct Ring(Arc<Handle>);

#[derive(Clone, Debug)]
pub struct Event(Arc<Handle>);

/// Creates the two local owners of one sticky manual-reset event.
pub fn event() -> io::Result<(Ring, Event)> {
    let handle = unsafe { CreateEventW(core::ptr::null(), 1, 0, core::ptr::null()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    let handle = Arc::new(Handle(handle));
    Ok((Ring(handle.clone()), Event(handle)))
}

impl Notify for Ring {
    type Error = io::Error;

    fn notify(&self) -> Result<(), Self::Error> {
        if unsafe { SetEvent(self.0.0) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

impl Ring {
    /// Reconstructs a notification owner received from another process.
    ///
    /// # Safety
    ///
    /// `handle` must be an owned handle to a manual-reset event.
    pub unsafe fn from_owned_handle(handle: RawHandle) -> Self {
        Self(Arc::new(Handle(handle.cast())))
    }
}

impl Event {
    /// Reconstructs an event owner received from another process.
    ///
    /// # Safety
    ///
    /// `handle` must be an owned handle to a manual-reset event.
    pub unsafe fn from_owned_handle(handle: RawHandle) -> Self {
        Self(Arc::new(Handle(handle.cast())))
    }

    pub(crate) fn clear(&self) -> io::Result<()> {
        if unsafe { ResetEvent(self.0.0) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub(crate) fn handle(&self) -> HANDLE {
        self.0.0
    }
}

impl AsRawHandle for Event {
    fn as_raw_handle(&self) -> RawHandle {
        self.0.0.cast()
    }
}

impl AsRawHandle for Ring {
    fn as_raw_handle(&self) -> RawHandle {
        self.0.0.cast()
    }
}
