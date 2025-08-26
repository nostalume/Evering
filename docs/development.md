# Development

This record the current design of `evering`.

## Evering

`evering` contains the implementation of channel or uring-like data structure; `driver/bridge` as the data cache for asynchronous submit-complete logic.

- `Sealed`: Provide a private trait to forbid others implement the exposed trait. 

---

- **uring**: concurrent efficient `mpmc` oriented uring.
	- asynch: `async-channel` based with event-listener equipped channel.
	- sync: `crossbeam` based channel.
	- bare: `lfqueue` based with compile time size aiming for _Ipc_.(A less-burden layout)
	
- `ISender`, `IReceiver`: provide the trait of send and receive.
- `UringSpec`: the `UringSpec::SQE/CQE` for user to implement its concrete uring. 

Common Design:

```rust
pub struct Channel<T, U> {
    s: Sender<T>,
    r: Receiver<U>,
}

pub type Submitter<S: UringSpec> = Channel<S::SQE, S::CQE>;
pub type Completer<S: UringSpec> = Channel<S::CQE, S::SQE>;
```

---

- **driver**: cache for asynchronous submit-complete logic.
	- `locked`: `Arc<Mutex<T>>` based, currently `T=Slab`.
	- `unlocked`: `Arc<T>` based, currently `T=LockFreeObjectPool`.
	- `cell`: a data structure `IdCell` which wrap the identifier for driver with the data.
	- `op_cache`: contains `locked` and `unlocked` version of cache.
	
- `Driver`: the trait of driver: `register/complete` cache in asynchronous runtime.
- `Dring`: A additional sealing structure to wrap `SQE/CQE` as `IdCell<Id,SQE>` for a given `Driver`.
- `BridgeTmpl`: contains the driver and the submitter to submit and receive the submitted operation.

```rust
pub struct BridgeTmpl<D: Driver, T: ISender + IReceiver + Clone, R: Role> {
    driver: D,
    sq: T,
    _marker: PhantomData<R>,
}
```

The whole interface is built upon `BridgeTmpl` for `submit/complete` as two isolated part by a `ZST`(zero size type), even it's for a single submitter, to allow parallel.

One define the driver based on the uring as below:

```rust
use uring::asynch::{ Completer as UCompleter, Submitter as USubmitter },

pub type Submitter<D> = USubmitter<Dring<D>>;
pub type Completer<D> = UCompleter<Dring<D>>;

pub type Bridge<D, R> = BridgeTmpl<D, Submitter<D>, R>;
pub type SubmitBridge<D> = Bridge<D, Submit>;
pub type ReceiveBridge<D> = Bridge<D, Receive>;

type UringBridge<D> = (SubmitBridge<D>, ReceiveBridge<D>, Completer<D>);
```

## Evering Shm

`evering-shm` establishes the shared memory related functionalities.

The basic design is a shared memory initiated by os backend with a continuous layout of `header|allocator`, controlling the allocation of shared memory.

---

- `Sealed`: Provide a private trait to forbid others implement the exposed trait.

---

- `malloc`: allocator behavoir in shared memeory.
	- `IAllocator`: A self-contained allocator api, especially `Arc<A>` isn't supported as a `Allocator`.
	- `blink/gma/tlsf`: interface implementation for specific allocator.
	- `ShmAllocator`: allocator in shared memory with offset based data acquirement ability.
	- `ShmInit`: allocator that can utilize a continuous memory block.
- `header`: header in shared memory.
	- `Header`: `RwLock<T>` based with necessary information to identify a correct shared memory.

```rust
#[repr(C)]
pub struct HeaderIn {
    magic: u16,
    status: ShmStatus,
    rc: AtomicU32,
    spec: [Option<isize>; 5], // a specific predefined offset that can acquire data unsafely.
}

#[repr(transparent)]
pub struct Header(RwLock<HeaderIn>);

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShmStatus {
    Initializing = 1,
    Initialized = 2,
    Corrupted = 3, // optional
}
```

- `boxed`: allocated data structure in shared memory.
	- `ShmBox`: a `Box`-like data structure.
	- `ShmToken`: offset based for IPC with compiled time size.

```rust
pub trait AsShmToken: Sealed {}
pub struct ShmSized;
impl Sealed for ShmSized {}
impl AsShmToken for ShmSized {}
pub struct ShmSlice(usize);
impl Sealed for ShmSlice {}
impl AsShmToken for ShmSlice {}

/// A token that can be transferred between processes.
pub struct ShmToken<T, A: ShmAllocator, S: AsShmToken>(isize, A, S, PhantomData<T>);
```

- `area`: continuous shared memory related data structure.
	- `ShmSpec`: basic spec of os addr/flags.
	- `ShmBackend`: a specific backend based on `ShmSpec`, for example, `FdBackend` implements `Windows` and `Unix`.
	- `ShmArea`: a area with range, flags, and backend. `backend` should be `Clone` to across threads.

- `os`: os related backend to initiate shared memory.
	- `unix`: `Unix` related info.