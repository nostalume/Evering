# Boxed Module Invariants

This document outlines the critical assumptions and invariants in the boxed module (`evering/src/boxed.rs`) that, if violated, can lead to Undefined Behavior (UB), panics, or logical corruption. These focus on smart pointer safety, allocation consistency, and reference counting.

## Pointer and Memory Safety Invariants

### Raw Pointer Validity (Critical)
- **PBox::from_raw_ptr()**: The provided `ptr` must be a valid, non-null pointer to a properly allocated and initialized instance of type `T`. Invalid pointers cause UB on dereference or drop.
- **PArc::from_raw_ptr()** and **PArc::from_inner()**: The `ptr` must point to a valid `PArcIn<T>` structure with correct alignment and initialization. Invalid pointers lead to UB.
- **Token reconstruction**: In token-based operations, reconstructed pointers must match the original allocation. Mismatched pointers cause UB.

### Metadata Consistency (Critical)
- **Allocation Metadata Match**: The `meta` field in `PBox` and `PArc` must exactly correspond to the allocation performed by the allocator `A`. Mismatched metadata causes UB during deallocation or access.
- **Layout Preservation**: Layouts used in `malloc_by()` and `demalloc()` must be identical. Inconsistent layouts lead to memory corruption or UB.
- **Allocator Identity**: The same allocator instance `A` must be used for allocation and deallocation. Using different allocators causes UB.

## Allocation and Deallocation Invariants

### Allocation Success Assumptions (High)
- **Allocation Failure Handling**: Methods like `new_in()` and `try_new_in()` assume allocation can fail gracefully. Ignoring allocation errors leads to panics or UB.
- **Layout Validity**: Provided `Layout` objects must be valid and represent realistic memory requirements. Invalid layouts cause panics in allocation functions.
- **Zero-Sized Type Handling**: Operations on ZSTs (zero-sized types) assume special null handling. Incorrect ZST treatment causes UB.

### Deallocation Safety (Critical)
- **Drop Execution**: `Drop` implementations assume the pointer and metadata are still valid. Premature invalidation causes double-free or UB.
- **Layout Matching**: `demalloc()` calls must use the exact layout from the original allocation. Mismatched layouts cause memory corruption.
- **Non-Zero Size Check**: Deallocation skips only truly zero-sized allocations. Incorrect size assumptions lead to memory leaks.

## Reference Counting Invariants (PArc Specific)

### Atomic Reference Count Management (Critical)
- **Reference Count Bounds**: The `rc` field must never exceed `MAX_REFCOUNT` (isize::MAX). Overflow causes panics in `Clone`.
- **Acquire-Release Ordering**: Increment and decrement operations must use correct atomic ordering. Incorrect ordering causes race conditions and UB.
- **Final Drop Execution**: When reference count reaches zero, the drop logic must execute atomically. Interrupted drops cause resource leaks or UB.

### Initialization Safety (High)
- **PArcIn Structure Validity**: The `PArcIn<T>` structure must be properly initialized with `rc = 1` and valid `data`. Uninitialized structures cause UB.
- **Layout Calculation**: `arcin_layout_of()` must produce valid layouts for the combined structure. Invalid layouts cause allocation failures or UB.

## Type Safety and Initialization Invariants

### Initialization State (Critical)
- **assume_init()**: Memory must be fully initialized before calling `assume_init()`. Reading uninitialized memory causes UB.
- **MaybeUninit Handling**: Operations on `MaybeUninit<T>` assume proper write-before-read patterns. Premature reads cause UB.
- **Slice Bounds**: Slice operations assume `len` is valid and matches allocated size. Out-of-bounds access causes UB.

### Trait Bounds (Medium)
- **PointeeIn and Message Traits**: Token-related methods assume types implement required traits. Missing implementations cause compile-time errors but may lead to runtime UB if bypassed.
- **Send/Sync Safety**: Unsafe `Send`/`Sync` implementations assume the allocator and data are thread-safe. Incorrect assumptions cause data races.

## Lifetime and Ownership Invariants

### Manual Memory Management (High)
- **ManuallyDrop Usage**: `into_raw()` and `into_raw_ptr()` assume `ManuallyDrop` prevents double-drop. Incorrect usage causes double-free.
- **Leak Safety**: `leak()` assumes the returned reference is used correctly. Improper use leads to use-after-free.
- **Forget Semantics**: `mem::forget()` calls assume ownership transfer. Forgotten ownership causes leaks or UB.

## Concurrency Invariants

### Atomic Operations (Critical)
- **Fetch Operations**: `fetch_add()` and `fetch_sub()` assume single-threaded or properly synchronized access. Unprotected concurrent access causes data races.
- **Fence Usage**: Memory fences must be placed correctly around reference count operations. Incorrect fencing causes visibility issues and UB.

## Error Handling Invariants

### Panic Conditions (Medium)
- **Allocation Errors**: Failed allocations in infallible methods (e.g., `new_uninit_in()`) cause panics. Error handling must be explicit.
- **Array Layout Errors**: `Layout::array()` failures must be handled. Unhandled errors cause panics.
- **Copy Operations**: `copy_from_slice()` assumes source and destination sizes match. Size mismatches cause UB.

Violating these invariants can result in memory corruption, data races, panics, or silent undefined behavior. Always validate allocator consistency, pointer validity, and proper initialization when using these smart pointer types.</content>
<parameter name="filePath">/home/nostal/proj/Evering/docs/invariants/boxed.md