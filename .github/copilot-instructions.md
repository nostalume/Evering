# Evering AI Coding Guidelines

## Architecture Overview
Evering is an asynchronous communication framework inspired by io_uring, designed for efficient submit-complete patterns in concurrent systems. It consists of three main components:

- **uring**: Concurrent MPMC channels with async/sync/bare variants for submitter-completer communication
- **driver**: Caches for async operation state management (locked/unlocked implementations)
- **shm**: Shared memory allocators and data structures for IPC

Key abstractions:
- `Channel<T, U>`: Generic channel for sending T and receiving U
- `BridgeTmpl<D, T, R>`: Combines driver D with channel T for role R (Submit/Receive)
- `ShmBox<T>`: Shared memory allocated boxes with offset-based tokens

## Development Workflow
- **Build**: Requires Rust nightly (edition 2024). Use `cargo build --release` for optimized builds
- **Test**: Run `cargo test` across workspace members. Benchmarks in `bench/ipc-bench/` use Criterion
- **Debug**: Watch for rustc ICEs (internal compiler errors) - several present in repo. Use `RUST_BACKTRACE=1` for traces
- **Features**: Enable `std` feature for std library support; `tracing` for logging

## Code Patterns & Conventions
- **Sealed Traits**: Use `Sealed` marker traits to prevent external implementations (e.g., `AsShmToken`)
- **no_std Default**: Core library is no_std; enable `std` feature for full functionality
- **Shared Memory Layout**: Use `#[repr(C)]` for structures crossing process boundaries
- **Role Markers**: Employ `PhantomData<R>` for compile-time role distinctions (Submit/Receive)
- **Unsafe Patterns**: Prefer offset-based access in shm; use `UnsafeCell` for interior mutability
- **Error Handling**: Custom result types with specific error variants (e.g., `ShmStatus::Corrupted`)

## Examples
- **Channel Usage**: `type Submitter<S: UringSpec> = Channel<S::SQE, S::CQE>;`
- **Bridge Creation**: `type SubmitBridge<D> = BridgeTmpl<D, Submitter<D>, Submit>;`
- **Shm Allocation**: `let token = ShmToken::<T, A, ShmSized>(offset, allocator, ShmSized, PhantomData);`

## Dependencies & Integration
- **Async Runtime**: Tokio for async operations
- **Concurrency**: Crossbeam for sync channels, Spin for locks
- **OS Abstraction**: Nix for Unix system calls
- **Memory**: Custom allocators (blink/gma/tlsf) for shared memory

Reference: [development.md](docs/development.md) for detailed design docs</content>
<parameter name="filePath">/home/nostal/proj/Evering/.github/copilot-instructions.md