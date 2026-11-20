//! The Windows arm of `crate::os::event`: one manual-reset event object behind an `Arc`, owned twice so
//! `Ring` may set it and `Event` reset it, with the handle closed exactly once when the last local owner
//! goes. `Ring::notify` is `SetEvent` and `Event::clear` is `ResetEvent`; the `Send`/`Sync` claims on
//! `Handle` are what let the wait layer park on one thread while another rings, and they rest on the
//! kernel accepting concurrent calls on one handle rather than on anything in this file.
//!
//! A manual-reset object is one bit shared by every holder, so `clear` takes the pending notification
//! from *all* of them at once — including a peer in another process that received the handle through a
//! handover and had not woken yet. Nothing here holds two processes, and the other two arms of this
//! module hold a byte queue and a counter, so a change to this file cannot be checked against them from
//! a Windows run either.

use std::{
    io,
    os::windows::io::{
        AsHandle, AsRawHandle, BorrowedHandle, IntoRawHandle, OwnedHandle, RawHandle,
    },
    sync::Arc,
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::Threading::{CreateEventW, ResetEvent, SetEvent},
};

use crate::notify::Notify;

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
    /// Takes ownership of a received notification handle.
    pub fn from_owned_handle(handle: OwnedHandle) -> Self {
        Self(Arc::new(Handle(handle.into_raw_handle().cast())))
    }
}

impl Event {
    /// Takes ownership of a received wait handle.
    pub fn from_owned_handle(handle: OwnedHandle) -> Self {
        Self(Arc::new(Handle(handle.into_raw_handle().cast())))
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

impl AsHandle for Event {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        unsafe { BorrowedHandle::borrow_raw(self.as_raw_handle()) }
    }
}

impl AsHandle for Ring {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        unsafe { BorrowedHandle::borrow_raw(self.as_raw_handle()) }
    }
}

#[cfg(test)]
mod tests {
    use super::{Event, event};
    use crate::notify::Notify;

    use windows_sys::Win32::{Foundation::WAIT_OBJECT_0, System::Threading::WaitForSingleObject};

    /// Asks the kernel whether the event is set without taking anything: a wait with no
    /// timeout reports the state and leaves it. `Event::clear` cannot report this because
    /// the wake-set clause needs an already-reset event to succeed.
    fn signaled(event: &Event) -> bool {
        unsafe { WaitForSingleObject(event.handle(), 0) == WAIT_OBJECT_0 }
    }

    #[test]
    fn one_clear_consumes_every_pending_kick() {
        let (ring, event) = event().unwrap();
        assert!(!signaled(&event), "a fresh latch holds nothing");
        ring.notify().unwrap();
        ring.notify().unwrap();
        ring.notify().unwrap();
        assert!(signaled(&event), "three kicks are one bit, not a count");
        event.clear().unwrap();
        assert!(
            !signaled(&event),
            "one clear must reset the latch every waiter shares"
        );
    }
}
