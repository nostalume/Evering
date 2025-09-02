#![cfg(feature = "unix")]
#![cfg(test)]

use std::os::fd::OwnedFd;

use crate::boxed::{ShmBox, ShmToken};
use crate::os::FdBackend;
use crate::os::unix::{MFdFlags, ProtFlags, UnixFdConf};
use crate::perlude::{AsShmAlloc, AsShmAllocError, ShmAllocError, ShmHeader};

type TestShm = AsShmAlloc<FdBackend<OwnedFd>>;
type Error = AsShmAllocError<FdBackend<OwnedFd>>;

const SIZE: usize = 0x10000;
const SLICE: &[u8] = &[1u8; 100];
const SLICE2: &[u8] = &[2u8; 100];

fn create<P: nix::NixPath + ?Sized>(name: &P, size: usize) -> Result<TestShm, Error> {
    let cfg = UnixFdConf::default_mem_fd(name, SIZE, MFdFlags::empty())
        .map_err(ShmAllocError::MapError)?;
    let m = TestShm::init_or_load(
        FdBackend::new(),
        None,
        size,
        ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
        cfg,
    )?;
    Ok(m)
}

fn load_spec(m: &TestShm) {
    let u = loop {
        match unsafe { m.spec_ref::<[u8; 100]>(0) } {
            Some(u) => break u,
            None => {
                let u = ShmBox::new_in([1u8; 100], &m);
                m.init_spec(u, 0);
            }
        }
    };
    for i in u.iter() {
        assert_eq!(*i, 1);
    }
}

fn box_test(m: &TestShm) {
    let u = ShmBox::copy_from_slice(SLICE, m);
    for i in u.iter() {
        assert_eq!(*i, 1);
    }
    let t: ShmToken<u8, _, _> = u.into();
    let mut u = ShmBox::from(t);
    for i in u.iter() {
        assert_eq!(*i, 1);
    }
    u.as_mut().copy_from_slice(SLICE2);
    for i in u.iter() {
        assert_eq!(*i, 2);
    }
}

#[test]
fn spec_test() {
    let m = create("test", SIZE).unwrap();
    load_spec(&m);
    box_test(&m);
}
