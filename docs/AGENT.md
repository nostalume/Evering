# Evering Agent Guide

## Goal

Evering develops a general, operating-system-independent model for bounded
inter-process communication over shared memory. The intended fast path uses
relocatable allocation metadata, fixed-capacity shared queues, typed ownership
transfer, and generational correlation without placing process-local
capabilities in shared storage.

Optimize only after preserving the cross-process model. Monomorphism, const
generics, concrete policies, and inlining are deliberate tools, but they do not
justify weakening ownership, layout, provenance, initialization, or recovery
rules.

Evering integrates with asynchronous runtimes but is not itself a runtime. Keep
queue transport, progress notification, scheduling, and runtime adaptation
conceptually separable.

## Technology

- Rust 2024 edition on nightly Rust.
- One root `evering` crate with default standard-library support and a verified
  `no_std + alloc` configuration when default features are disabled.
- Additive `map`, `notify`, and `process` adapter capabilities, with `os` as
  their native-platform sugar; Tokio and tracing remain orthogonal features.
- Const generics and nightly const-trait features for statically specialized
  layouts and policies.
- Atomic bounded queues and generational registries for shared coordination.
- A synchronized variable-size talc allocator with region-relative metadata and
  a session-scoped heap view for typed payload operations.
- Allocator-layout-identified tokens with checked extent and alignment admission
  for typed move semantics.
- Unix file-descriptor and memory-mapping support through `nix`.
- Optional Linux event-counter and Windows manual-event notification adapters,
  with Tokio registration kept process-local.
- Criterion-style benchmark infrastructure with external IPC comparisons.

## Working principles

- Begin with domain concepts and ownership. Explain why the current design exists
  before proposing a replacement API.
- Treat shared bytes as a protocol. Identify which values are persistent,
  transferable, process-local, and reconstructable.
- Treat `Message` and `Envelope` implementations as unsafe portability proofs;
  reject process-local representation and ownership at that boundary.
- Keep the mapping root responsible only for region identity and mapping
  lifetime. Let each persistent layout own and validate its schema and immutable
  typed information; do not introduce a session-wide manifest.
- Treat expected-identity and discovery mappings as attach-only. A peer must not
  initialize a missing layout.
- Keep platform mapping policy at construction. A source yields one linear
  mapping owner; admitted layouts and sessions must not regain address, flag,
  handle, backend, or source-error parameters.
- Keep portable identity numeric and generational. Keep wakers, futures,
  reference-counted handles, and mapping addresses local.
- Prefer explicit lifecycle states over relying on Rust destruction across a
  process boundary.
- Preserve rejected ownership in every fallible transfer API. Quarantine
  resources whose remaining ownership cannot be proved empty.
- Do not infer cross-process correctness from thread tests or a shared mapping in
  one process.
- Preserve monomorphism where a type changes shared interpretation or hot-path
  behavior. Erase construction-only policy once its owned runtime effect has
  been admitted.
- Performance claims require current benchmarks and a correctness oracle.

## Verification expectations

Changes to observable behavior should establish the missing behavior first,
then implement the smallest coherent correction.

At minimum, select evidence from:

- mock-backend tests for layout and allocator logic;
- Unix mapping tests for platform behavior;
- registry and queue concurrency tests;
- type-mismatch and stale-generation negative tests;
- real two-process tests with different mapping bases;
- queue-full, disconnect, cancellation, and abrupt-peer-death tests;
- wrong-allocator, invalid-identity, duplicate-completion, and quarantine tests;
- release-mode benchmarks only after correctness checks pass.

Do not describe an unexecuted platform path, benchmark, or process boundary as
verified.

## Immediate goal

The immediate goal is semantic truth at the actual process boundary:

- prove that independently mapped peers can attach to the same session;
- prove that registry identifiers and allocator metadata remain valid at
  different mapping bases;
- extend fixed-size and dynamically allocated token round trips with explicit
  wrong-layout rejection;
- define bounded behavior for disconnect and peer death;
- model close/send and cancellation/completion interleavings beyond thread
  stress, and continue the workspace-wide unsafe `Send`/`Sync` audit.

The next boundary is public bootstrap exchange: transfer mapping and
notification resources atomically, retain exact child identity for supervision,
and keep recovery behind explicit proof that the peer can no longer access the
mapping. Broader backends and performance comparison follow that evidence.
