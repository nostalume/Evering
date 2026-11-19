use core::marker::PhantomData;
use std::{
    io,
    process::{Child, Command, ExitStatus},
};

pub const MAX_BOOTSTRAP: usize = 4096;
pub const MAX_RESOURCES: usize = 8;
pub(crate) const HANDOFF_MAGIC: [u8; 4] = *b"EVR1";

/// The one key by which a granted rendezvous is announced to the process it was granted to.
///
/// Its value names the rendezvous rather than locating it: on Linux the descriptor number the
/// granting process installed before `exec`, on Windows the pipe instance the listener created.
/// Knowledge of the value is not what authorises a handover — possession of the rendezvous is —
/// so this key carries no secret and the value is safe to place in the child's environment.
#[cfg(all(feature = "process", any(unix, windows)))]
pub(crate) const RENDEZVOUS: &str = "EVERING_RENDEZVOUS";

/// Windows only: the process id of the party that admitted the grant, which the receiver matches
/// against the kernel's own answer for the pipe server. Linux needs no second key because the
/// descriptor it was granted already reports its creator, and `getppid` is the check there.
#[cfg(all(feature = "process", windows))]
pub(crate) const INTRODUCER: &str = "EVERING_INTRODUCER";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bootstrap(Box<[u8]>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooLarge {
    pub len: usize,
    pub max: usize,
}

impl Bootstrap {
    pub fn new(bytes: impl AsRef<[u8]>) -> Result<Self, TooLarge> {
        let bytes = bytes.as_ref();
        if bytes.len() > MAX_BOOTSTRAP {
            return Err(TooLarge {
                len: bytes.len(),
                max: MAX_BOOTSTRAP,
            });
        }
        Ok(Self(bytes.into()))
    }

    pub fn into_boxed(self) -> Box<[u8]> {
        self.0
    }
}

impl AsRef<[u8]> for Bootstrap {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
#[test]
fn handoff_version_is_frozen() {
    assert_eq!(HANDOFF_MAGIC, *b"EVR1");
}

#[cfg(all(test, feature = "process", any(unix, windows)))]
#[test]
fn a_granted_rendezvous_is_announced_by_one_frozen_key() {
    assert_eq!(RENDEZVOUS, "EVERING_RENDEZVOUS");
}

/// Linear ownership of one exact child instance.
pub struct Supervisor {
    child: Child,
    status: Option<ExitStatus>,
}

/// Terminal evidence for the exact child retained by its supervisor.
pub struct Exit<'a> {
    status: &'a ExitStatus,
    _supervisor: PhantomData<&'a Supervisor>,
}

impl Supervisor {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        command.spawn().map(|child| Self {
            child,
            status: None,
        })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    #[cfg(windows)]
    pub(crate) fn handle(&self) -> windows_sys::Win32::Foundation::HANDLE {
        use std::os::windows::io::AsRawHandle;
        self.child.as_raw_handle().cast()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<Exit<'_>>> {
        if self.status.is_none() {
            self.status = self.child.try_wait()?;
        }
        Ok(self.status.as_ref().map(|status| Exit {
            status,
            _supervisor: PhantomData,
        }))
    }

    pub fn wait(&mut self) -> io::Result<Exit<'_>> {
        if self.status.is_none() {
            self.status = Some(self.child.wait()?);
        }
        Ok(Exit {
            status: self.status.as_ref().unwrap(),
            _supervisor: PhantomData,
        })
    }

    pub fn kill_wait(&mut self) -> io::Result<Exit<'_>> {
        if self.status.is_none() {
            match self.child.try_wait()? {
                Some(status) => self.status = Some(status),
                None => {
                    if let Err(kill) = self.child.kill() {
                        match self.child.try_wait()? {
                            Some(status) => {
                                self.status = Some(status);
                                return Ok(Exit {
                                    status: self.status.as_ref().unwrap(),
                                    _supervisor: PhantomData,
                                });
                            }
                            None => return Err(kill),
                        }
                    }
                    self.status = Some(self.child.wait()?);
                }
            }
        }
        Ok(Exit {
            status: self.status.as_ref().unwrap(),
            _supervisor: PhantomData,
        })
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        if self.status.is_some() {
            return;
        }
        match self.child.try_wait() {
            Ok(Some(status)) => self.status = Some(status),
            Ok(None) => {
                let _ = self.child.kill();
                self.status = self.child.wait().ok();
            }
            Err(_) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
}

impl Exit<'_> {
    pub fn status(&self) -> &ExitStatus {
        self.status
    }

    pub fn success(&self) -> bool {
        self.status.success()
    }
}
