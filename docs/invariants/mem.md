# Memory Module Invariants

This document outlines the critical assumptions and invariants in the memory module (`evering/src/mem/`) that, if violated, can lead to Undefined Behavior (UB) or panics. These are primarily related to allocator operations, memory mappings, and pointer manipulations.

## General Memory Safety Assumptions

### Pointer Validity and Bounds
- **Pointer Allocation Check**: In `MemOps::offset()`, the provided `ptr` must be allocated within the memory area. Violating this causes UB due to invalid pointer arithmetic.
- **Memory Area Validity**: All `obtain_*` methods assume the memory area is valid and accessible. Invalid or unmapped memory leads to UB.
- **Instance Validity**: `obtain_by_offset`, `obtain_by_addr`, `obtain_slice_by_offset`, and `obtain_slice_by_addr` require that the target memory contains a valid instance of the requested type. Reading uninitialized or incorrectly typed memory causes UB.

### Address and Offset Calculations
- **Base Pointer Validity**: `AddrSpan::as_ptr()` and `AddrSpan::as_mut_ptr()` assume `base_ptr` is a valid base address. Invalid base pointers cause UB.
- **Offset Bounds**: Offsets must not exceed the memory area's size. Out-of-bounds offsets can cause UB or panics in arithmetic operations.
- **Alignment Requirements**: Type-specific alignment must be respected in offset calculations to avoid UB.

## Allocator-Related Invariants

### Memory Mapping and Permissions
- **Mapping Validity**: `RawMap::from_ptr()` and `RawMap::from_raw()` assume the provided start address and size represent a valid, mapped memory region. Unmapped regions cause UB.
- **Permission Checks**: Methods like `reserve()` and `commit()` require appropriate access permissions (e.g., WRITE). Insufficient permissions result in errors but may cause UB if bypassed.
- **Header Initialization**: `commit()` assumes the `header` pointer points to a valid, properly aligned memory location for the header type. Invalid headers lead to UB or initialization failures.

### Allocation Operations
- **Layout Compatibility**: `MemAlloc::malloc_by()` and related methods assume the requested `Layout` is valid and compatible with the allocator's constraints. Invalid layouts may cause panics or UB.
- **Deallocation Safety**: `MemDealloc::demalloc()` requires that the provided `meta` and `layout` match the original allocation. Mismatched parameters cause UB or memory corruption.
- **Allocator State**: Allocators assume internal state (e.g., free lists, metadata) remains consistent. Concurrent modifications without proper synchronization cause UB.

### Shared Memory Specifics
- **Cross-Process Validity**: In shared memory contexts, pointers and offsets must remain valid across process boundaries. Process-specific assumptions (e.g., address spaces) can lead to UB if violated.
- **Atomic Operations**: Shared memory structures using atomics assume proper alignment and access patterns. Misaligned accesses cause UB.
- **Reference Counting**: `RcHeader` and related structures assume reference counts are managed correctly. Underflow or overflow causes panics or UB.

## Handle and Layout Invariants

### MapHandle Safety
- **Pointer Validity**: `MapHandle::from_raw()` assumes the provided `ptr` is valid and points to a properly initialized instance. Invalid pointers cause UB.
- **Lifetime Management**: Handles assume the underlying memory remains mapped for the handle's lifetime. Premature unmapping causes UB.
- **Type Safety**: Mapping operations (`map`, `try_map`, `may_map`) assume type compatibility. Incorrect type assumptions cause UB.

### MapLayout Operations
- **Offset Progression**: `reserve()` and `push()` assume offsets increase monotonically and stay within bounds. Invalid offsets cause panics or UB.
- **Configuration Validity**: `commit()` and `push()` require valid configuration parameters for header initialization. Invalid configs lead to initialization errors or UB.
- **Area Consistency**: Layout operations assume the underlying `RawMap` remains unchanged during use. Concurrent modifications cause UB.

## Error Handling and Recovery
- **Error Propagation**: Methods that return `Result` assume errors are handled appropriately. Ignoring errors can lead to inconsistent state and subsequent UB.
- **Panic Conditions**: Operations like array layout creation (`Layout::array`) can panic on invalid inputs (e.g., zero-sized arrays). Inputs must be validated beforehand.

## Concurrency Invariants
- **Synchronization**: Shared structures (e.g., `RcHeader`) assume proper synchronization primitives are used. Race conditions cause UB.
- **Atomic Access**: Atomic fields assume correct ordering and access patterns. Incorrect usage leads to UB.
- **Counter Management**: `SuspendMap` and counters assume balanced acquire/release operations. Imbalances cause resource leaks or UB.

## Platform-Specific Assumptions
- **Address Space**: Memory addresses assume platform-specific validity (e.g., virtual address ranges). Invalid addresses cause UB.
- **OS Integration**: Mmap operations assume OS-level memory management behaves correctly. System-level failures can propagate as UB.

Violating these invariants can result in crashes, data corruption, security vulnerabilities, or silent incorrect behavior. Always validate inputs and maintain proper state management when using these APIs.</content>
<parameter name="filePath">/home/nostal/proj/Evering/docs/invariants/mem.md