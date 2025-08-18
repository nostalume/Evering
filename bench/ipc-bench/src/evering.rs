extern crate evering_ipc;

use core::marker::PhantomData;
use core::mem::MaybeUninit;
use std::os::fd::{AsFd, OwnedFd};

use evering_ipc::shm::boxed::ShmToken;
use evering_ipc::shm::os::{FdBackend, UnixFdConf, UnixShm};
use evering_ipc::shm::tlsf::SpinTlsf;
use evering_ipc::uring::UringSpec;
use evering_ipc::{IpcAlloc, IpcHandle, IpcSpec};
use evering_ipc::driver::unlocked::PoolDriver;
use nix::sys::memfd::MFdFlags;
use super::*;

enum Sqe<I:IpcSpec> {
	Exit,
	Ping {
		ping:i32,
		req: ShmToken<[u8],IpcAlloc<I>>,
		resp: ShmToken<[MaybeUninit<u8>],IpcAlloc<I>>
	}
}

enum Rqe {
	Exited,
	Pong {pong:i32}
}

struct IpcInfo<I:IpcSpec>(PhantomData<I>);

impl<I:IpcSpec> UringSpec for IpcInfo<I> {
	type SQE = Sqe<I>;
	type CQE = Rqe;
}

type MyPoolDriver<I> = PoolDriver<IpcInfo<I>>;

struct MyIpcSpec<F>(PhantomData<F>);
impl<F: AsFd> IpcSpec for MyIpcSpec<F> {
	type A = SpinTlsf;
	type S = UnixShm;
	type M = FdBackend<F>;
}

const CAP:usize = 

type MyIpc<F> = IpcHandle<MyIpcSpec<F>, MyPoolDriver<MyIpcSpec<F>>, CAP>;

fn default_cfg(id: &str,bufsize:usize) ->  UnixFdConf<OwnedFd> {
	let shmid = shmid(id);
	let size = shmsize(bufsize);
	UnixFdConf::default_from_mem_fd(shmid.as_str(), size, MFdFlags::empty()).unwrap()
}

pub fn bench(id: &str, iters: usize, bufsize: usize) -> Duration {
	let s_cfg = default_cfg(id,bufsize);
	let c_cfg = s_cfg.clone();

	let elapsed = std::thread::scope(|s| {
		let server = s.spawn(|| {
			cur_block_on(async move {
				let handle = 
			})
		})

		let client = s.spawn(|| {
			cur_block_on(async move {
				let handle = 
			})
		})
	})
}
