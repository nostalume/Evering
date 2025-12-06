#![cfg(feature = "unix")]
#![cfg(test)]

// use std::os::fd::OwnedFd;
// use std::path::Path;

use crate::mem::{Access, MemBlkBuilder, MemBlkHandle};
use crate::os::FdBackend;
use crate::os::unix::{AddrSpec, ProtFlags, UnixFd};
use crate::perlude::Session;
use crate::perlude::allocator::{MemAlloc, Optimistic};
use crate::tests::{prob, tracing_init};

type UnixMemHandle = MemBlkHandle<AddrSpec, FdBackend>;
type UnixAlloc = MemAlloc<Optimistic, AddrSpec, FdBackend>;
type UnixSession<'a, H, const N: usize> = Session<Optimistic, H, N, AddrSpec, FdBackend>;

const SIZE: usize = 0x20000;
// const CHECK: u8 = 42;
fn mock_handle(name: &str, size: usize) -> UnixMemHandle {
    let fd = UnixFd::memfd(name, size, false).expect("should create");
    let builder = MemBlkBuilder::from_backend(FdBackend);
    builder.shared(size, Access::ReadWrite, fd).unwrap()
}

// fn create_mem<P: nix::NixPath + ?Sized>(path: &P, size: usize) -> Result<TestShm, Error> {
//     let cfg =
//         UnixFdConf::default_mem(path, SIZE, MFdFlags::empty()).map_err(ShmAllocError::MapError)?;
//     let m = TestShm::init_or_load(
//         FdBackend::new(),
//         None,
//         size,
//         ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
//         cfg,
//     )?;
//     Ok(m)
// }

// fn spec_test<const N: usize>(m: &TestShm, idx: usize) {
//     type SLICE<const N: usize> = [u8; N];
//     let slice = [CHECK; N];
//     let u = loop {
//         match unsafe { m.spec_ref::<SLICE<N>>(idx) } {
//             Some(u) => break u,
//             None => {
//                 let u = ShmBox::new_in(slice, &m);
//                 m.init_spec(u, idx);
//             }
//         }
//     };
//     assert!(u.iter().all(|&x| x == CHECK))
// }

// fn box_test<const N: usize>(m: &TestShm) {
//     let slice = [CHECK; N];
//     let u = ShmBox::copy_from_slice(&slice, m);
//     assert!(u.iter().all(|&x| x == CHECK));
//     let t = u.into_token();
//     let u = ShmBox::from(t);
//     assert!(u.iter().all(|&x| x == CHECK));
// }

// fn multi_test<const N: usize>() {
//     use std::thread;

//     const NAME: &str = "10";
//     type SLICE<const N: usize> = [u8; N];
//     let slice = [CHECK; N];
//     let base = create_named(NAME, SIZE).unwrap();
//     {
//         let u = ShmBox::new_in(slice, &base);
//         base.init_spec(u, 0);
//     }

//     // for i in 0..10 {
//     //     // dbg!(i);
//     //     let m = create_named(NAME, SIZE).unwrap();
//     //     let spec = unsafe { m.spec_ref::<SLICE<N>>(0) }.expect("spec missing");
//     //     assert!(spec.iter().all(|&x| x == CHECK));
//     // }

//     // let handles: Vec<_> = (0..4)
//     //     .map(|_| {
//     //         thread::spawn(|| {
//     //             let m = create_named(NAME, SIZE).unwrap();
//     //             let spec = unsafe { m.spec_ref::<[u8; 16]>(0) }.expect("spec missing in thread");
//     //             assert!(spec.iter().all(|&x| x == 42));
//     //         })
//     //     })
//     //     .collect();

//     // for h in handles {
//     //     h.join().unwrap()
//     // }
//     UnixFdConf::clean_named(NAME).unwrap();
// }

// #[test]
// fn area_alloc() {
//     let m = create_mem("test", SIZE).unwrap();
//     for i in 0..4 {
//         const LEN: usize = 32;
//         spec_test::<LEN>(&m, i);
//         box_test::<LEN>(&m);
//     }
// }

// #[test]
// fn multi_alloc() {
//     const LEN: usize = 64;
//     multi_test::<LEN>()
// }
