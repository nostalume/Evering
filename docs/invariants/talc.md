# Talc Module Invariants

This document outlines the critical assumptions and invariants in the talc module (`evering/src/talc.rs`) that, if violated, can lead to Undefined Behavior (UB), panics, or logical corruption. These focus on offset-based memory management, chunk metadata integrity, and allocator thread safety.

## Offset-Based Pointer Safety Invariants

### Relocatable Pointer Validity (Critical)
- **Rel::from_raw()**: The provided `ptr` must be within the same allocation as `base_ptr`, and `ptr >= base_ptr`. Violating this causes UB in offset calculations and pointer reconstruction.
- **Rel::as_raw()** and **Rel::as_ptr()**: The `base_ptr` must match the original base used for creation, and the resulting pointer must be within valid memory bounds. Incorrect base pointers cause UB.
- **Rel<[T]>::as_raw()** and **Rel<[T]>::as_ptr()**: The `len` parameter must accurately represent the slice length. Incorrect lengths cause UB in slice operations.

### Memory Layout Assumptions (Critical)
- **Pointer Arithmetic**: All offset calculations assume wrapping arithmetic is safe and pointers remain within allocated regions. Out-of-bounds offsets cause UB.
- **Alignment Requirements**: Chunk bases, tags, and metadata structures must maintain proper alignment. Misaligned accesses cause UB.
- **Size Consistency**: Chunk sizes stored in headers/tails must match actual memory ranges. Inconsistent sizes cause UB in boundary calculations.

## Chunk and Metadata Integrity Invariants

### Tag Structure Validity (Critical)
- **Tag::init()**: `chunk_base` must be a valid, aligned pointer within the heap, and `heap_base` must be consistent. Invalid bases cause UB in relative addressing.
- **Tag::from_acme()** and **Tag::from_alloc_base()**: Acme pointers must point to valid tag locations. Incorrect acme calculations cause UB.
- **Flag Consistency**: `is_above_free` and `is_allocated` flags must accurately reflect chunk state. Incorrect flags cause logical corruption in allocation decisions.

### Free List Structure Invariants (Critical)
- **FreeNode Insertion/Removal**: Node pointers (`next`, `prev_next`) must be valid and correctly linked. Invalid links cause infinite loops or UB in traversals.
- **FreeHead/FreeTail Consistency**: Size fields (`size_low`, `size_high`) must match and represent valid chunk sizes. Inconsistent sizes cause UB in chunk reconstruction.
- **Chunk Boundaries**: Base and acme pointers must define valid, non-overlapping ranges. Invalid boundaries cause UB in adjacent chunk access.

### Chunk Range Validity (High)
- **Chunk::from_endpoint()**: Base must be less than acme, and the range must meet minimum chunk size requirements. Invalid ranges cause panics or UB.
- **Chunk::split_prefix/suffix()**: Split operations assume sufficient space for minimum chunks. Insufficient space causes panics.
- **Size Calculations**: Chunk sizes must be calculable from headers/tails without overflow. Size overflows cause panics.

## Allocation and Deallocation Invariants

### Layout Validity (High)
- **TalcMeta::allocate()**: Requested `layout` must have valid size and alignment. Invalid layouts cause panics in allocation functions.
- **Size Requirements**: Allocated sizes must include space for tags and metadata. Insufficient sizes cause UB in tag placement.
- **Alignment Constraints**: Requested alignments must be compatible with chunk structure. Incompatible alignments cause allocation failures or UB.

### Deallocation Safety (Critical)
- **TalcMeta::deallocate()**: The `ptr` must have been previously allocated by this allocator with the matching `layout`. Mismatched pointers/layouts cause UB or memory corruption.
- **Chunk Merging**: Adjacent free chunks must be correctly identified and merged. Failed merging causes memory leaks.
- **Tag Updates**: Deallocation must correctly update tag flags and free list links. Incorrect updates cause logical corruption.

## Binning and Free List Management Invariants

### Bin Configuration Consistency (Medium)
- **BinConfig Parameters**: `LINEAR_STAGES`, `BYTES_PER_LINEAR`, and `LOG_DIVS` must produce valid bin indices and sizes. Invalid configurations cause panics in bin calculations.
- **Bin Index Mapping**: Size-to-bin mappings must be deterministic and within array bounds. Out-of-bounds indices cause panics.
- **Stage Transitions**: Linear to exponential bin transitions must be seamless. Incorrect transitions cause allocation inefficiencies or failures.

### Free List Operations (High)
- **Availability Bits**: The `avails` array must accurately reflect free node availability. Incorrect bits cause allocation of occupied chunks.
- **Node Linking**: Insert/remove operations must maintain doubly-linked list integrity. Broken links cause traversal failures.
- **Concurrent Access**: `TalckMeta` assumes `Mutex` provides exclusive access. Unprotected concurrent modifications cause data races and UB.

## Thread Safety and Synchronization Invariants

### Mutex Protection (Critical)
- **TalckMeta Locking**: All operations on `TalcMeta` must be performed under the `Mutex`. Unlocked access causes data races and UB.
- **Lock Scope**: Locks must be held for the entire duration of metadata modifications. Partial locking causes inconsistent state.
- **Reentrancy**: Operations must not recursively acquire locks. Recursive locking causes deadlocks.

### Atomic Operations (High)
- **Availability Toggles**: `toggle_avail()` assumes atomic bit operations are thread-safe. Non-atomic access causes data races.
- **Ordering Guarantees**: Memory ordering in atomic operations must prevent visibility issues. Incorrect ordering causes stale reads.

## Initialization and Configuration Invariants

### Header Initialization (High)
- **TalckMeta::init()**: Configuration parameters `(Offset, Size)` must be valid for the heap layout. Invalid configs cause initialization failures.
- **Magic Number**: Header magic must match for validity checks. Mismatched magic causes corruption detection failures.
- **Attach Status**: Initialization status must be properly set and checked. Incorrect status causes UB in subsequent operations.

### Heap Size Requirements (Medium)
- **Minimum Heap Size**: Heap must be large enough for metadata and minimum chunks. Insufficient size causes allocation failures.
- **Bin Array Sizing**: Bin arrays must accommodate all configured bins. Undersized arrays cause out-of-bounds access.

## Error Handling and Validation Invariants

### Debug Assertions (Medium)
- **scan_errors()**: In debug builds, free list consistency must be maintained. Inconsistent lists cause panics.
- **Boundary Checks**: All pointer arithmetic must stay within heap bounds. Boundary violations cause UB.
- **Size Validations**: Chunk sizes must meet minimum requirements. Invalid sizes cause panics in operations.

### Allocation Results (Medium)
- **Success Guarantees**: Successful allocations must return valid, non-overlapping pointers. Invalid returns cause UB in user code.
- **Failure Handling**: Allocation failures must be properly propagated. Ignored failures cause panics or UB.

Violating these invariants can result in memory corruption, data races, deadlocks, panics, or silent undefined behavior. The TALC allocator relies on strict adherence to offset-based addressing and metadata consistency for safe operation.</content>
<parameter name="filePath">/home/nostal/proj/Evering/docs/invariants/talc.md