# Token Module Invariants

This document outlines the critical assumptions and invariants in the token module (`evering/src/token.rs`) that, if violated, can lead to Undefined Behavior (UB), panics, or logical corruption. These focus on type-erased token safety, metadata consistency, and cross-process communication.

## Metadata and Type Safety Invariants

### Metadata Accuracy (Critical)
- **PointeeIn::metadata()**: Returned `Metadata` must exactly match the pointer's type (Sized for single values, Slice with correct length for arrays). Incorrect metadata causes UB in `as_ptr()` transmute operations.
- **Metadata::as_ptr()**: The `raw` pointer must correspond to the `Metadata` variant. Mismatched metadata (e.g., treating a sized pointer as a slice) causes UB through invalid transmutation.
- **Metadata::from_ptr()**: Assumes the input pointer's metadata is correctly extractable. Invalid pointers lead to incorrect metadata and subsequent UB.

### Pointer Validity (Critical)
- **TokenOf::from_raw()**: The provided `ptr` must be a valid, properly aligned pointer to an allocated instance of `T`. Invalid pointers cause UB in all token operations.
- **TokenOf::as_ptr()**: Assumes the allocator and meta produce a valid base pointer. Invalid base pointers cause UB.
- **detokenize()**: The reconstructed pointer must be safe to use as assumed by the callback function `f`. Incorrect reconstruction leads to UB.

## Type Erasure and Identification Invariants

### Type ID Consistency (High)
- **Token::identify()**: The stored `id` must match `T::TYPE_ID` for successful identification. Mismatched IDs cause incorrect type casting and UB.
- **Message Trait Bounds**: Types used in tokens must correctly implement `Message` with valid `TYPE_ID`. Incorrect implementations cause runtime type confusion.
- **From<TokenOf<T, M>> for Token<M>`**: Assumes `T` implements `Message`. Missing implementations cause compile errors but may lead to UB if bypassed.

## Allocation and Memory Invariants

### Allocator Consistency (Critical)
- **TokenOf::boxed()**: The allocator `A` must be the same instance used for the original allocation of `meta`. Different allocators cause UB in pointer reconstruction.
- **Meta Validity**: `meta` must represent a valid, non-null allocation. Null or invalid metadata causes UB in `recall_by()`.
- **Memory Layout**: Assumes the recalled pointer has the correct layout for type `T`. Layout mismatches cause UB in dereference.

## Header and Envelope Invariants

### Envelope Integrity (Medium)
- **PackToken Operations**: Header `H` must remain consistent with token operations. Inconsistent headers cause logical corruption in tagged operations.
- **Tag Operations**: `Tag` and `TagRef` implementations must handle values correctly. Incorrect tag handling causes panics or data corruption.
- **ReqId Composition**: `id` and `header` must be properly combined. Invalid composition causes incorrect request identification.

## Unsafe Operation Invariants

### Transmutation Safety (Critical)
- **mem::transmute_copy()**: Used in `as_ptr()` assumes exact type and size matching. Size or type mismatches cause UB.
- **Pointer Casting**: `ptr::slice_from_raw_parts_mut()` assumes valid length and base pointer. Invalid lengths cause UB.
- **NonNull Construction**: `NonNull::new_unchecked()` assumes non-null pointers. Null pointers cause UB.

## Identification and Composition Invariants

### ID Management (High)
- **Identified Trait**: `compose()` and `decompose()` must preserve ID and header integrity. Incorrect preservation causes request routing failures.
- **Id Uniqueness**: IDs must be unique within their context. Duplicate IDs cause logical corruption in request handling.
- **Envelope Defaults**: `with_default()` assumes `H::default()` produces valid envelopes. Invalid defaults cause initialization errors.

## Cross-Process Communication Invariants

### Serialization Safety (Critical)
- **Token Transfer**: Tokens must be safely serializable/deserializable across processes. Incorrect serialization causes UB in remote reconstruction.
- **Metadata Preservation**: Slice lengths and type information must survive transfer. Lost metadata causes UB in detokenization.
- **Allocator Compatibility**: Remote allocators must produce equivalent memory layouts. Layout differences cause UB.

## Error Handling and Panic Invariants

### Failure Conditions (Medium)
- **Type Mismatches**: Operations assuming type compatibility (e.g., `identify()`) must handle failures gracefully. Unhandled mismatches cause panics.
- **Allocation Failures**: Implicit allocations in token operations must succeed. Failures cause panics if not handled.
- **Bounds Checking**: Slice operations assume valid bounds. Out-of-bounds access causes UB.

Violating these invariants can result in type confusion, memory corruption, data races across processes, or silent undefined behavior. Always validate metadata accuracy, pointer validity, and type consistency when working with tokens in the Evering IPC framework.</content>
<parameter name="filePath">/home/nostal/proj/Evering/docs/invariants/token.md