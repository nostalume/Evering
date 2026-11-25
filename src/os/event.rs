//! The Windows arm of `crate::os::event`: one manual-reset event object behind an `Arc`, owned twice so
//! `Ring` may set it and `Event` reset it, with the handle closed exactly once when the last local owner
//! goes. `Ring::notify` is `SetEvent` and `Event::clear` is `ResetEvent`; the `Send`/`Sync` claims on
//! `Handle` are what let the wait layer park on one thread while another rings, and they rest on the
//! kernel accepting concurrent calls on one handle rather than on anything in this file.
//!
//! A manual-reset object is one bit shared by every holder, so `clear` takes the pending notification
//! from *all* of them at once — including a second holder made by a caller that duplicated the handle and
//! rebuilt an end of its own. That is what the types here refuse: `Ring` is `Clone`, because many writers
//! on one latch is the shape a bit is built for, while `Event` is not, because a consumer that shares its
//! readiness loses it to whichever holder reset the bit first. The handover moves its reading end, and
//! borrows its writing end, for the same reason. Nothing here holds two processes, and the other two arms
//! of this module hold a byte queue and a counter, so a change to this file cannot be checked against them
//! from a Windows run either.
//!
//! `Event::wait_any` is the one operation whose refusal belongs to this arm alone: the kernel admits 64
//! objects into a wait array, so a wider set is answered with an error here and with a park on the two
//! unix legs, which ask `poll` and meet no such ceiling. A caller with more peers than that has to hold
//! several sets and park on each, and nothing in this file can tell it so.

use std::{
    io,
    os::windows::io::{
        AsHandle, AsRawHandle, BorrowedHandle, IntoRawHandle, OwnedHandle, RawHandle,
    },
    sync::Arc,
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0},
    System::Threading::{CreateEventW, INFINITE, ResetEvent, SetEvent, WaitForMultipleObjects},
};

use crate::notify::Notify;

/// The kernel's own ceiling on one wait array (`MAXIMUM_WAIT_OBJECTS`), written here rather than
/// imported so the `notify` feature does not grow a Windows module for one number the kernel has never
/// changed.
const MAX_LATCHES: usize = 64;

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

#[derive(Debug)]
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

    /// Parks this thread until one of `events` is ready, consumes that latch's readiness and reports
    /// which index it was.
    ///
    /// One park reports one latch and never a count: this latch is one bit shared by every holder, so
    /// there is nothing behind a ready answer that a number could describe, and the rest of the set keeps
    /// whatever readiness it had until a park names it. The slice is borrowed exclusively because a reset
    /// takes the pending kick from every holder at once, and a park that reported a latch somebody else
    /// had already served would be a wake that went missing with no second park to notice.
    ///
    /// The kernel takes a whole array into one wait and admits 64 objects in it (`MAXIMUM_WAIT_OBJECTS`),
    /// so a wider set is refused rather than waited on in pieces: a second array waited only after the
    /// first reported nothing would leave a peer's latch unanswered until some other peer rang at all.
    pub fn wait_any(events: &mut [Event]) -> io::Result<usize> {
        if events.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a wait over no doorbell has nothing to be signalled by",
            ));
        }
        if events.len() > MAX_LATCHES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "one Windows wait array holds at most 64 objects",
            ));
        }
        let index = {
            let handles: Vec<HANDLE> = events.iter().map(Event::handle).collect();
            // SAFETY: every handle in the array is borrowed from an `Event` this call holds exclusively
            // for the whole park, so none can be closed while the kernel waits on it, and the count is
            // that array's own length, within the ceiling checked above.
            let signaled = unsafe {
                WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE)
            };
            if signaled == WAIT_FAILED {
                return Err(io::Error::last_os_error());
            }
            let index = (signaled - WAIT_OBJECT_0) as usize;
            if index >= events.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the wait answered with an object outside the set it was given",
                ));
            }
            index
        };
        events[index].clear()?;
        Ok(index)
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
    use super::{Event, MAX_LATCHES, event};
    use crate::notify::Notify;

    use std::io;
    use windows_sys::Win32::{Foundation::WAIT_OBJECT_0, System::Threading::WaitForSingleObject};

    /// Asks the kernel whether the event is set without taking anything: a wait with no
    /// timeout reports the state and leaves it. `Event::clear` cannot report this because
    /// the wake-set clause needs an already-reset event to succeed.
    fn signaled(event: &Event) -> bool {
        unsafe { WaitForSingleObject(event.handle(), 0) == WAIT_OBJECT_0 }
    }

    #[test]
    fn a_set_wider_than_one_wait_array_is_refused() {
        let (mut latches, _rings): (Vec<_>, Vec<_>) = (0..=MAX_LATCHES)
            .map(|_| event().unwrap())
            .map(|(ring, event)| (event, ring))
            .unzip();
        let error = Event::wait_any(&mut latches).unwrap_err();
        assert_eq!(
            error.kind(),
            io::ErrorKind::InvalidInput,
            "65 latches are not waited on in two arrays, because the second would be reached only after \
             the first reported nothing"
        );
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
