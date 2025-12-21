# Token Module Design

This document describes the structural organization of the token module (`evering/src/token.rs`) and how it enables safe type-erased inter-process communication in the Evering framework.

## Module Responsibilities

### Pointer Metadata Extraction
The `PointeeIn` trait and `Metadata` enum provide a mechanism to capture type information from pointers at runtime, distinguishing between sized values and slices.

**What it does:**
- Extracts length information from slice pointers
- Preserves type structure for reconstruction
- Enables type-safe pointer manipulation

**What it does not do:**
- Perform memory allocation or deallocation
- Validate pointer validity
- Handle custom DSTs beyond slices

### Typed Token Management
`TokenOf<T, M>` represents type-preserving tokens that can reconstruct typed smart pointers from metadata and allocator information.

**What it does:**
- Stores allocation metadata alongside type information
- Provides safe reconstruction of `PBox` instances
- Supports generic metadata types for different allocators

**What it does not do:**
- Manage token lifetime or storage
- Perform type checking beyond trait bounds
- Handle cross-process serialization

### Type-Erased Token System
`Token<M>` provides runtime type-erased tokens using `TypeId` for safe downcasting, enabling polymorphic message handling.

**What it does:**
- Enables type-safe identification and casting
- Supports extensible header attachment
- Provides null token construction for defaults

**What it does not do:**
- Define message semantics or protocols
- Handle token transmission mechanisms
- Provide dynamic type registration

### Header and Tagging System
`PackToken<H, M>` and related types allow attaching metadata envelopes to tokens for request correlation and routing.

**What it does:**
- Supports tag-based message classification
- Enables request/response identification
- Provides extensible envelope system

**What it does not do:**
- Define envelope semantics or validation
- Handle message queuing or prioritization
- Provide networking abstractions

## Data Flow and Control Flow

### Token Creation Flow
1. User provides `PBox<T, A>` with allocated value
2. `PBox::token_of_with()` extracts raw pointer and metadata
3. `TokenOf::from_raw()` captures type information
4. `TokenOf` converts to `Token<M>` via `From` implementation

### Token Reconstruction Flow
1. Receiver holds `Token<M>` with `TypeId`
2. `Token::identify<T>()` performs type-checked downcast
3. `TokenOf::boxed()` reconstructs `PBox<T, A>` using allocator
4. Ownership transfers to reconstructed smart pointer

### Header Attachment Flow
1. Base `Token<M>` created from typed token
2. `Token::with<H>()` wraps with envelope header
3. `PackToken` operations modify header in place
4. `PackToken::unpack()` separates token and header

## Component Interactions

### Integration with Message System
- `Message` trait provides `TypeId` for token identification
- `MoveMsg` uses tokens for ownership transfer
- Type safety enforced through trait bounds

### Integration with Allocation System
- `MemAllocator` provides metadata types and reconstruction
- `Meta` trait enables pointer recovery from metadata
- Allocator consistency ensures valid reconstruction

### Integration with Envelope System
- `Envelope` traits define header interfaces
- `Tag` and `TagId` enable metadata attachment
- Header operations maintain token integrity

## Structural Patterns

### Progressive Type Erasure
- `PBox` → `TokenOf` (typed) → `Token` (erased) → `PackToken` (with metadata)
- Each step preserves necessary information for reconstruction
- Type safety maintained through compile-time checks

### Trait-Based Polymorphism
- `PointeeIn` enables generic pointer handling
- `TypeTag` provides compile-time type identification
- `Envelope` allows extensible metadata systems

### Safe Reconstruction Protocol
- Metadata + allocator → pointer recovery
- Type ID matching prevents incorrect casting
- NonNull guarantees prevent null pointer issues

## Limitations and Constraints

### Type System Boundaries
- Limited to types implementing `PointeeIn` (sized + slices)
- No support for custom DST implementations
- Type IDs not preserved across compilation boundaries

### Memory Management Scope
- Tokens do not own allocated memory
- Reconstruction requires compatible allocator
- No automatic cleanup or lifetime management

### Semantic Restrictions
- No borrowing or sharing semantics
- Ownership transfer requires explicit reconstruction
- No support for cyclic or self-referential structures

### Performance Characteristics
- Metadata extraction has minimal runtime overhead
- Type ID comparison is hash-based
- Reconstruction involves allocator lookups</content>
<parameter name="filePath">/home/nostal/proj/Evering/docs/design/token.md