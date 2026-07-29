use std::{
    ffi::c_void,
    io,
    os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
    sync::atomic::{AtomicU64, Ordering},
};

use windows_sys::Win32::{
    Foundation::{
        CloseHandle, DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle,
        ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
        INVALID_HANDLE_VALUE,
    },
    Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING,
        PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
    },
    System::{
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_MESSAGE,
            PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_WAIT,
        },
        Threading::GetCurrentProcess,
    },
};

use crate::process::{Bootstrap, MAX_BOOTSTRAP, MAX_RESOURCES, Supervisor};

const HEADER: usize = 8;
const MAGIC: [u8; 4] = *b"EVR1";
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
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
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

    pub fn accept(self, child: &Supervisor) -> io::Result<Socket> {
        let handle = raw(&self.handle);
        if unsafe { ConnectNamedPipe(handle, core::ptr::null_mut()) } == 0
            && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED
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
        message.extend_from_slice(&MAGIC);
        message.extend_from_slice(&(bootstrap.as_ref().len() as u16).to_le_bytes());
        message.extend_from_slice(&(remote.len() as u16).to_le_bytes());
        message.extend_from_slice(bootstrap.as_ref());
        for handle in &remote {
            message.extend_from_slice(&(*handle as usize).to_le_bytes());
        }
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
            rollback(target, &remote);
            return Err(io::Error::last_os_error());
        }
        Ok(())
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
        if read < HEADER || message[..4] != MAGIC {
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
        for bytes in message[HEADER + len..].chunks_exact(size_of::<usize>()) {
            let value = usize::from_le_bytes(bytes.try_into().unwrap());
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
