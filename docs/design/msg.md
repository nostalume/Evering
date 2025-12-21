# Message Module Design

This document describes the structural organization of the message module (`evering/src/msg.rs`) and how it supports safe, type-aware inter-process communication in the Evering framework.

## Module Responsibilities

### Type Identification System
The `type_id` submodule provides a compile-time type identification mechanism using FNV-1a hashing to generate unique `TypeId` values for types. It implements the `TypeTag` trait for primitive types and provides combinators for compound types (slices, references, options).

**What it does:**
- Generates deterministic, collision-resistant type identifiers
- Supports generic type composition through hash combination
- Provides compile-time type safety for message routing

**What it does not do:**
- Perform runtime type checking or reflection
- Handle dynamic type registration
- Provide human-readable type names

### Message Semantics Framework
The `Message` trait defines types that can be safely transmitted as messages, with associated `Semantics` types that specify transfer behavior.

**What it does:**
- Enforces semantic constraints on message types
- Supports extensible semantics (currently `Move` for ownership transfer)
- Enables type-safe message construction and deconstruction

**What it does not do:**
- Define message serialization formats
- Handle message queuing or buffering
- Provide networking protocols

### Token-Based Message Handling
`MoveMsg<T>` provides utilities for converting between owned values and type-erased tokens, supporting both single values and slices.

**What it does:**
- Creates tokens from owned values using allocators
- Reconstructs owned values from tokens
- Handles slice-based bulk data transfer

**What it does not do:**
- Manage token lifetime or storage
- Perform validation beyond type identity
- Support non-move semantics

## Data Flow and Control Flow

### Message Creation Flow
1. User provides owned value `T: Message<Semantics = Move>`
2. `MoveMsg::new()` allocates `PBox<T, A>` using provided allocator
3. `PBox::token_with()` extracts `Token<M>` and returns allocator
4. Token can be transmitted (type-erased)

### Message Reception Flow
1. Receiver obtains `Token<M>` with matching `TypeId`
2. `Token::identify<T>()` performs type-checked downcast
3. `MoveMsg::detoken()` calls `TokenOf::boxed()` to reconstruct `PBox<T, A>`
4. Ownership transfers to receiver

### Type ID Generation Flow
- Primitive types: Direct FNV hash of type name
- Compound types: Hash combination of base hashes with discriminators
- Generic types: Recursive hash combination preserving type structure

## Component Interactions

### Integration with Token System
- `Message` trait bounds ensure tokens represent valid message types
- `TypeId` matching enables safe `Token::identify()` operations
- `MoveMsg` bridges allocation (`PBox`) and type erasure (`Token`)

### Integration with Allocation System
- `MoveMsg` requires `MemAllocator` for value boxing
- Token reconstruction depends on allocator compatibility
- Memory layout assumptions align with allocator guarantees

### Integration with Envelope System
- `Envelope` traits provide extensible metadata attachment
- `Tag` and `TagId` enable request/response correlation
- Headers can carry routing and control information

## Structural Patterns

### Trait-Based Extensibility
- `Message` and `Envelope` traits allow user-defined message types
- `TypeTag` enables automatic type ID generation for new types
- Semantic traits support future transfer modes (borrow, copy)

### Compile-Time Safety
- Type IDs computed at compile time prevent runtime type confusion
- Trait bounds enforce semantic correctness
- Generic constraints ensure allocator compatibility

### Separation of Concerns
- Type identification isolated in `type_id` submodule
- Message semantics defined orthogonally to types
- Envelope metadata decoupled from message content

## Limitations and Constraints

### Semantic Restrictions
- Currently limited to move semantics
- No support for shared or borrowed message passing
- Bulk operations restricted to slices

### Type System Boundaries
- Type IDs not preserved across compilation units
- No runtime type information beyond identity
- Generic type erasure loses concrete type parameters

### Performance Characteristics
- Hash-based type IDs have minimal computation overhead
- Token operations involve metadata copying
- Allocation required for all message transfers</content>
<parameter name="filePath">/home/nostal/proj/Evering/docs/design/msg.md