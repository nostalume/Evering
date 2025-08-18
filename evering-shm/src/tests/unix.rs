#![cfg(feature = "unix")]
#![cfg(test)]

use std::os::fd::OwnedFd;

use crate::os::FdBackend;
use crate::os::unix::{UnixFdConf, MFdFlags, ProtFlags, UnixShm};
use crate::shm_alloc::{ShmAllocError, ShmHeader, ShmSpinTlsf};
use crate::shm_box::ShmBox;

type TestShm = ShmSpinTlsf<UnixShm, FdBackend<OwnedFd>>;
type Error = ShmAllocError<UnixShm, FdBackend<OwnedFd>>;

fn create<P: nix::NixPath + ?Sized>(
    name: &P,
    size: usize,
) -> Result<TestShm, Error> {
    let cfg =
        UnixFdConf::default_from_mem_fd(name, MFdFlags::empty()).map_err(ShmAllocError::MapError)?;
    let m = TestShm::init_or_load(
        FdBackend::new(),
        None,
        size,
        ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
        cfg,
    )?;
    Ok(m)
}

fn load_spec(m: &TestShm) -> ShmBox<[u8; 100], &TestShm> {
    loop {
        match unsafe { m.spec::<_>(0) } {
            Some(u) => break u,
            None => {
                let u = ShmBox::new_in([1u8; 100], &m);
                m.init_spec(u, 0);
            }
        }
    }
}

#[test]
fn spec_test() {
    let m = create("test", 0x1000).unwrap();
    let u = load_spec(&m);
    assert_eq!(u[42], 1);
}
