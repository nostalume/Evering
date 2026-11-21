//! The Windows transport under a handover: a message-mode named pipe that carries one offer, plus the
//! handles that offer names. `bind` creates the pipe with `FILE_FLAG_FIRST_PIPE_INSTANCE` and one
//! instance, so a name already taken is the listener's failure rather than something to delete;
//! `accept` waits to the deadline, switches the instance to blocking message mode and then refuses
//! anything whose client pid is not the retained child; `send` duplicates every resource *into the
//! child's handle table* and writes those remote numbers into the message, and `rollback` closes the
//! remote copies if any later step fails. `connect` and `server_pid` are the client's mirror of the same
//! two checks.
//!
//! The remote numbers are the obligation this file cannot test: a handle value means something only in
//! the process that will read it, so a sender that wrote its own number instead of a duplicated one
//! would hand the child a valid handle to the wrong object, and `recv` — which turns each number
//! straight into an `OwnedHandle` — has nothing left to compare. Its partner is the framing, shared with
//! the Unix leg through `crate::process`'s magic and the same eight-byte header, and the message must
//! stay inside `MAX_MESSAGE` or `WriteFile` reports a short write; the header's size also bounds what a
//! `Listener` can admit at all, since one instance serves one child. The two messages and the two pid
//! questions have since been crossed by real processes on this host — the offer by
//! `tests/seat_announcement.rs`, whose child reads a session out of numbers written into its own handle
//! table, and the receipt by `tests/arrival_receipt_windows.rs`. What has not is `rollback`, which runs
//! only after a duplication or a write that failed, and the `MAX_MESSAGE` ceiling that turns a longer
//! offer into a short write.
//!
//! The receipt crosses the same instance in the other direction, one message with the arrival tag and no
//! handles in it, and what it asks for is the read `accept` already does under a deadline — with a sleep
//! instead of a spin, because the writer has work to do before it reports. Here the credit is not a field
//! of the message at all: it is `GetNamedPipeClientProcessId`, which names the process that owns the
//! client end of this instance, and that answer is only as good as the single instance the `Listener`
//! created. Widen `nMaxInstances` and a receipt would arrive from whichever client happened to connect,
//! which is the coupling between this file's read and `bind` that no test on either host would point at.

use std::{
    ffi::c_void,
    io,
    os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::{
        CloseHandle, DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle,
        ERROR_BROKEN_PIPE, ERROR_PIPE_CONNECTED, ERROR_PIPE_LISTENING, GENERIC_READ, GENERIC_WRITE,
        GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    },
    Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING,
        PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
    },
    System::{
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId,
            GetNamedPipeServerProcessId, PIPE_NOWAIT, PIPE_READMODE_MESSAGE,
            PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_WAIT, PeekNamedPipe,
            SetNamedPipeHandleState,
        },
        Threading::GetCurrentProcess,
    },
};

use crate::process::{Bootstrap, Introduction, MAX_BOOTSTRAP, MAX_RESOURCES, Supervisor};

const HEADER: usize = 8;
const MAX_MESSAGE: usize = HEADER + MAX_BOOTSTRAP + MAX_RESOURCES * size_of::<usize>();
static NEXT_PIPE: AtomicU64 = AtomicU64::new(0);

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

fn raw(handle: &OwnedHandle) -> HANDLE {
    handle.as_raw_handle().cast()
}

pub struct Listener {
    handle: OwnedHandle,
    name: String,
}

#[derive(Debug)]
pub struct Socket(OwnedHandle);

/// One all-or-nothing received resource set.
#[derive(Debug)]
pub struct Offer {
    bootstrap: Bootstrap,
    resources: Box<[OwnedHandle]>,
}

impl Listener {
    pub fn bind() -> io::Result<Self> {
        let serial = NEXT_PIPE.fetch_add(1, Ordering::Relaxed);
        let name = format!(r"\\.\pipe\evering-{}-{serial}", std::process::id());
        let handle = unsafe {
            CreateNamedPipeW(
                wide(&name).as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_MESSAGE
                    | PIPE_READMODE_MESSAGE
                    | PIPE_NOWAIT
                    | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                MAX_MESSAGE as u32,
                MAX_MESSAGE as u32,
                0,
                core::ptr::null(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            handle: unsafe { OwnedHandle::from_raw_handle(handle.cast()) },
            name,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn accept(self, child: &Supervisor, deadline: Instant) -> io::Result<Socket> {
        let handle = raw(&self.handle);
        loop {
            if unsafe { ConnectNamedPipe(handle, core::ptr::null_mut()) } != 0 {
                break;
            }
            match unsafe { GetLastError() } {
                ERROR_PIPE_CONNECTED => break,
                ERROR_PIPE_LISTENING if Instant::now() < deadline => std::thread::yield_now(),
                ERROR_PIPE_LISTENING => return Err(io::ErrorKind::TimedOut.into()),
                _ => return Err(io::Error::last_os_error()),
            }
        }
        let mode = PIPE_READMODE_MESSAGE | PIPE_WAIT;
        if unsafe {
            SetNamedPipeHandleState(handle, &mode, core::ptr::null_mut(), core::ptr::null_mut())
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut process = 0;
        if unsafe { GetNamedPipeClientProcessId(handle, &mut process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if process != child.id() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "control peer is not the retained child",
            ));
        }
        Ok(Socket(self.handle))
    }
}

impl Socket {
    pub fn connect(name: &str) -> io::Result<Self> {
        let handle = unsafe {
            CreateFileW(
                wide(name).as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                core::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                core::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(unsafe { OwnedHandle::from_raw_handle(handle.cast()) }))
        }
    }

    /// What the kernel says about the process that owns the server end of this pipe instance.
    ///
    /// The mirror of `Listener::accept`'s check, and needed beside it: `FILE_FLAG_FIRST_PIPE_INSTANCE`
    /// makes a name that is already taken a failure for the listener rather than something to delete,
    /// but a process that connects to a pipe it does not own would still be handed whatever that pipe
    /// sends. This is the read that tells it apart.
    pub fn server_pid(&self) -> io::Result<u32> {
        let mut server = 0;
        if unsafe { GetNamedPipeServerProcessId(raw(&self.0), &mut server) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(server)
    }

    pub fn send(
        &self,
        child: &Supervisor,
        bootstrap: &Bootstrap,
        resources: &[BorrowedHandle<'_>],
    ) -> io::Result<()> {
        if resources.len() > MAX_RESOURCES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "too many resources",
            ));
        }
        let current = unsafe { GetCurrentProcess() };
        let target = child.handle();
        let mut remote = Vec::with_capacity(resources.len());
        for resource in resources {
            let mut duplicated = core::ptr::null_mut();
            if unsafe {
                DuplicateHandle(
                    current,
                    resource.as_raw_handle().cast(),
                    target,
                    &mut duplicated,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                )
            } == 0
            {
                rollback(target, &remote);
                return Err(io::Error::last_os_error());
            }
            remote.push(duplicated);
        }

        let mut message = Vec::with_capacity(
            HEADER + bootstrap.as_ref().len() + remote.len() * size_of::<usize>(),
        );
        message.extend_from_slice(&crate::process::HANDOFF_MAGIC);
        message.extend_from_slice(&(bootstrap.as_ref().len() as u16).to_le_bytes());
        message.extend_from_slice(&(remote.len() as u16).to_le_bytes());
        message.extend_from_slice(bootstrap.as_ref());
        for handle in &remote {
            message.extend_from_slice(&(*handle as usize).to_le_bytes());
        }
        self.write(&message)
            .inspect_err(|_| rollback(target, &remote))
    }

    /// Writes exactly one message, and reports a short write as the failure it is.
    fn write(&self, message: &[u8]) -> io::Result<()> {
        let mut written = 0;
        if unsafe {
            WriteFile(
                raw(&self.0),
                message.as_ptr().cast(),
                message.len() as u32,
                &mut written,
                core::ptr::null_mut(),
            )
        } == 0
            || written as usize != message.len()
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Writes the member identity this process arrived as: the arrival tag, the text, and no handles.
    ///
    /// Nothing is duplicated for the report, because nothing travels with it — which is also what makes
    /// the message-mode write safe: one message, one boundary, and the server's read takes the whole
    /// thing or reports the client gone.
    pub fn report(&self, announcement: &str) -> io::Result<()> {
        if announcement.len() > MAX_BOOTSTRAP {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "receipt text too long",
            ));
        }
        let mut message = Vec::with_capacity(HEADER + announcement.len());
        message.extend_from_slice(&crate::process::ARRIVAL_MAGIC);
        message.extend_from_slice(&(announcement.len() as u16).to_le_bytes());
        message.extend_from_slice(&0u16.to_le_bytes());
        message.extend_from_slice(announcement.as_bytes());
        self.write(&message)
    }

    /// The process that owns the client end of this instance, as the kernel answers for the pipe.
    ///
    /// `Listener::accept` asks this once before it will hand the instance over; asking again is what lets
    /// a receipt be attributed to the process the caller is about to reap rather than to the pipe.
    pub fn client_pid(&self) -> io::Result<u32> {
        let mut client = 0;
        if unsafe { GetNamedPipeClientProcessId(raw(&self.0), &mut client) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(client)
    }

    /// Reads the one receipt the client of this instance writes, bounded by `deadline`.
    ///
    /// The bound is a `PeekNamedPipe` loop, the shape `accept` already uses for the same problem on this
    /// host: the instance sits in blocking message mode and the only way to give up on a deadline is to
    /// ask how much is queued before committing to the read. Between peeks this sleeps rather than spins
    /// the way `accept` does, because `accept` waits for a client that may already be blocked in
    /// `CreateFileW` while this one waits for a process that has a session to join first, and a caller's
    /// budget of seconds should not cost a core.
    ///
    /// The two faces of a peer that will not report answer apart, and `tests/arrival_receipt_windows.rs`
    /// runs both on this host. A client that met, wrote nothing and exited fails the peek with
    /// `ERROR_BROKEN_PIPE`, which is the instance itself saying nobody is left to report; a client that
    /// connected and has not gone leaves the peek reporting an empty pipe, and only the caller's bound ends
    /// that read. A message written before the client left is *not* lost with it — the peek reports it and
    /// the read takes it — so this leg's `UnexpectedEof` means nobody reported rather than nobody could
    /// still be reached. The Unix leg has no equivalent of the first face: a datagram pair reports a
    /// peer's departure as nothing at all, so both of its silences end at the deadline, and
    /// `UnexpectedEof` on that leg belongs to a seqpacket endpoint. Both faces here mean nobody is going to
    /// report, and a caller that gives up for its own reasons is stopped by the bound only in the second.
    pub fn arrival(&self, deadline: Instant) -> io::Result<Introduction> {
        loop {
            let mut queued = 0;
            let peeked = unsafe {
                PeekNamedPipe(
                    raw(&self.0),
                    core::ptr::null_mut(),
                    0,
                    core::ptr::null_mut(),
                    &mut queued,
                    core::ptr::null_mut(),
                )
            };
            if peeked == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the client closed the rendezvous without reporting its arrival",
                    ));
                }
                return Err(error);
            }
            if queued > 0 {
                return self.read_receipt();
            }
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Takes one message and refuses anything that is not a well-formed receipt.
    ///
    /// A zero-handle read is not attempted after a peek reported nothing, so a `ReadFile` failure here is
    /// the client leaving between the peek and the read: the same `ERROR_BROKEN_PIPE` answer, one step
    /// later.
    fn read_receipt(&self) -> io::Result<Introduction> {
        let mut message = [0; MAX_MESSAGE];
        let mut read = 0;
        if unsafe {
            ReadFile(
                raw(&self.0),
                message.as_mut_ptr().cast(),
                message.len() as u32,
                &mut read,
                core::ptr::null_mut(),
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            return Err(if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the client closed the rendezvous without reporting its arrival",
                )
            } else {
                error
            });
        }
        let read = read as usize;
        if read < HEADER || message[..4] != crate::process::ARRIVAL_MAGIC {
            return Err(invalid("malformed receipt header"));
        }
        let len = u16::from_le_bytes([message[4], message[5]]) as usize;
        let declared = u16::from_le_bytes([message[6], message[7]]) as usize;
        if len > MAX_BOOTSTRAP || read != HEADER + len || declared != 0 {
            return Err(invalid("malformed receipt length"));
        }
        let text = std::str::from_utf8(&message[HEADER..read])
            .map_err(|_| invalid("the receipt text is not a report at all"))?;
        Introduction::announced(text).ok_or_else(|| invalid("the receipt names no member identity"))
    }

    pub fn recv(&self, expected: usize) -> io::Result<Offer> {
        if expected > MAX_RESOURCES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "too many expected resources",
            ));
        }
        let mut message = [0; MAX_MESSAGE];
        let mut read = 0;
        if unsafe {
            ReadFile(
                raw(&self.0),
                message.as_mut_ptr().cast(),
                message.len() as u32,
                &mut read,
                core::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let read = read as usize;
        if read < HEADER || message[..4] != crate::process::HANDOFF_MAGIC {
            return Err(invalid("malformed control header"));
        }
        let len = u16::from_le_bytes([message[4], message[5]]) as usize;
        let declared = u16::from_le_bytes([message[6], message[7]]) as usize;
        if len > MAX_BOOTSTRAP || declared > MAX_RESOURCES {
            return Err(invalid("control bounds exceeded"));
        }
        let needed = HEADER + len + declared * size_of::<usize>();
        if read != needed {
            return Err(invalid("malformed control length"));
        }
        let mut resources = Vec::with_capacity(declared);
        for bytes in message[HEADER + len..]
            .as_chunks::<{ size_of::<usize>() }>()
            .0
        {
            let value = usize::from_le_bytes(*bytes);
            resources.push(unsafe { OwnedHandle::from_raw_handle(value as *mut c_void) });
        }
        if declared != expected {
            return Err(invalid("resource count mismatch"));
        }
        let bootstrap = Bootstrap::new(&message[HEADER..HEADER + len])
            .map_err(|_| invalid("bootstrap too large"))?;
        Ok(Offer {
            bootstrap,
            resources: resources.into_boxed_slice(),
        })
    }
}

fn rollback(target: HANDLE, remote: &[HANDLE]) {
    let current = unsafe { GetCurrentProcess() };
    for handle in remote {
        let mut local = core::ptr::null_mut();
        if unsafe {
            DuplicateHandle(
                target,
                *handle,
                current,
                &mut local,
                0,
                0,
                DUPLICATE_SAME_ACCESS | DUPLICATE_CLOSE_SOURCE,
            )
        } != 0
        {
            unsafe {
                CloseHandle(local);
            }
        }
    }
}

impl Offer {
    pub fn bootstrap(&self) -> &Bootstrap {
        &self.bootstrap
    }

    pub fn resources(&self) -> &[OwnedHandle] {
        &self.resources
    }

    pub fn into_parts(self) -> (Bootstrap, Box<[OwnedHandle]>) {
        (self.bootstrap, self.resources)
    }
}
