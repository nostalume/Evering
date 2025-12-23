# Talc Module Design

This document describes the structural organization of the talc module (`evering/src/talc.rs`) and how it enables efficient, relocatable memory allocation in the Evering framework.

## Module Responsibilities

### Offset-Based Memory Addressing
The `Rel<T>` type provides relocatable pointers represented as offsets from a base pointer, enabling memory regions to be moved or remapped without invalidating internal references.

**What it does:**
- Encapsulates pointer arithmetic as offset calculations
- Supports both sized and unsized types (slices)
- Provides safe reconstruction of pointers from offsets

**What it does not do:**
- Manage memory allocation or deallocation
- Validate pointer validity at runtime
- Handle cross-process address translation

### Chunk Metadata Management
`Tag`, `FreeHead`, `FreeTail`, and `Chunk` structures maintain metadata for memory chunks, tracking allocation status, sizes, and boundaries.

**What it does:**
- Stores allocation flags and size information at chunk boundaries
- Enables efficient chunk splitting and merging
- Supports bidirectional traversal of free lists

**What it does not do:**
- Perform actual memory mapping or protection
- Handle concurrent access (requires external synchronization)
- Validate metadata consistency at runtime

### Free List Organization
`FreeNode` implements an intrusive doubly-linked list for tracking free memory chunks, organized into bins for size-based allocation.

**What it does:**
- Maintains sorted lists of free chunks by size
- Supports O(1) insertion and removal operations
- Enables efficient best-fit allocation strategies

**What it does not do:**
- Handle memory fragmentation directly
- Provide automatic defragmentation
- Support arbitrary size queries

### Binning Strategy Configuration
`BinConfig` provides compile-time configuration for organizing free chunks into size-based bins, balancing allocation speed and memory utilization.

**What it does:**
- Defines linear and exponential binning stages
- Pre-calculates bin mappings for fast size-to-bin conversion
- Optimizes for common allocation patterns

**What it does not do:**
- Adapt binning strategy at runtime
- Handle custom allocation policies
- Provide memory usage statistics

### Thread-Safe Allocation Interface
`TalckMeta` adds mutex-based synchronization to `TalcMeta`, while `Talc` provides the high-level allocation interface implementing `MemAlloc` and `MemDealloc`.

**What it does:**
- Ensures thread-safe access to allocator metadata
- Provides standard allocation/deallocation operations
- Integrates with the broader memory management system

**What it does not do:**
- Manage underlying memory mapping
- Provide advanced allocation features (realloc, etc.)
- Handle out-of-memory conditions gracefully

## Data Flow and Control Flow

### Allocation Flow
1. User requests allocation with `Layout` (size + alignment)
2. `Talc::allocate()` acquires lock on `TalckMeta`
3. Size maps to bin index using `BinConfig::bin_idx()`
4. `TalcMeta::acquire_chunk()` searches free list for suitable chunk
5. Chunk splits if necessary, metadata updates
6. Returns offset-based `Meta` for allocated region

### Deallocation Flow
1. User provides pointer and layout to `Talc::deallocate()`
2. Lock acquired, pointer converted to chunk information
3. Adjacent free chunks identified and merged
4. Chunk inserted into appropriate free list bin
5. Metadata updated, lock released

### Free List Maintenance Flow
- Allocation: Remove chunk from free list, update availability bits
- Deallocation: Insert chunk into free list, merge adjacent chunks
- Binning: Size determines bin placement for O(1) access

## Component Interactions

### Integration with Memory Mapping
- `MapTalc` integrates with `MapLayout` for OS-backed memory regions
- Header initialization claims initial heap space
- Base pointer provides reference for offset calculations

### Integration with Header System
- `Header<TalckMeta>` manages allocator state persistence
- Configuration parameters set heap bounds and binning
- Status tracking ensures proper initialization

### Integration with Broader Allocation System
- Implements `MemAlloc`/`MemDealloc` for compatibility
- `Meta` type provides offset-based allocation metadata
- Thread safety enables use in concurrent contexts

## Structural Patterns

### Intrusive Data Structures
- Free lists use chunk headers as list nodes
- Eliminates separate allocation for metadata
- Enables constant-time operations on free chunks

### Compile-Time Configuration
- Bin configuration computed at compile time
- Eliminates runtime overhead for bin calculations
- Allows optimization for specific allocation patterns

### Offset-Based Addressing
- All internal pointers stored as offsets
- Enables heap relocation without pointer fixup
- Simplifies serialization and cross-process sharing

### Layered Synchronization
- `TalcMeta` provides unsynchronized core logic
- `TalckMeta` adds mutex for thread safety
- Allows both single-threaded and concurrent usage

## Limitations and Constraints

### Memory Layout Assumptions
- Fixed header sizes and alignments
- Minimum chunk sizes for metadata overhead
- No support for custom metadata extensions

### Concurrency Model
- Coarse-grained locking with single mutex
- May limit scalability for high-contention scenarios
- No fine-grained locking for specific operations

### Allocation Policies
- Fixed binning strategy, no runtime adaptation
- Best-fit within bins, no global optimization
- Limited support for large allocations

### Error Handling
- Simple error propagation without recovery
- No out-of-memory handling beyond failure returns
- Debug assertions for consistency checking

### Platform Dependencies
- Assumes word-sized atomic operations
- Relies on specific alignment requirements
- No portability guarantees across architectures</content>
<parameter name="filePath">/home/nostal/proj/Evering/docs/design/talc.md