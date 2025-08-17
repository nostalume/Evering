#![cfg(feature = "unix")]
#![cfg(test)]

use std::os::fd::OwnedFd;

use crate::os::unix::{FdBackend, FdConfig, FdShmSpec, MFdFlags, ProtFlags};
use crate::shm_alloc::{ShmAllocError, ShmHeader, ShmSpinTlsf};
use crate::shm_box::ShmBox;

type TestShm<'a> = ShmSpinTlsf<'a, FdShmSpec<OwnedFd>, FdBackend>;

fn create<P: nix::NixPath + ?Sized>(
    name: &P,
    size: usize,
) -> Result<TestShm<'_>, ShmAllocError<FdShmSpec<OwnedFd>, FdBackend>> {
    let cfg =
        FdConfig::default_from_mem_fd(name, MFdFlags::empty()).map_err(ShmAllocError::MapError)?;
    let m = TestShm::init_or_load(
        FdBackend,
        None,
        size,
        ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
        cfg,
    )?;
    Ok(m)
}

fn load_spec<'a>(m: &'a TestShm<'a>) -> ShmBox<[u8; 100], &'a TestShm<'a>> {
    loop {
        match unsafe { m.spec::<_>() } {
            Some(u) => break u,
            None => {
                let u = ShmBox::new_in([1u8; 100], &m);
                m.init_spec(u);
            }
        }
    }
}

#[test]
fn spec_test() {
    let m = create("test", 0x1000).unwrap();
    let u = load_spec(&m);
    assert_eq!(u[0], 1);
}
