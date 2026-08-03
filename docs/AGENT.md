# Evering Agent Guide

## Goal

Evering develops a general, operating-system-independent model for bounded
inter-process communication over shared memory. The intended fast path uses
recoverable Pool capabilities, fixed-capacity shared queues, typed ownership
transfer, and runtime schema admission without placing process-local
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
- Atomic bounded queues and a generational Directory for shared coordination.
- A synchronized Talc GeneralHeap isolated to Directory layouts and explicit
  PBox values; it is not recoverable transfer storage.
- Recoverable Pools with immutable geometry, lifecycle-authoritative slots, and
  private Pool-identified Tokens with checked type, extent, and alignment.
- Unix file-descriptor and memory-mapping support through `nix`.
- Optional Linux event-counter and Windows manual-event notification adapters,
  with Tokio registration kept process-local.
- A bounded experiment harness with JSONL/Markdown/SVG artifacts and local-socket
  comparison; new baselines are added only with matched semantics.

## Working principles

- Begin with domain concepts and ownership. Explain why the current design exists
  before proposing a replacement API.
- Treat shared bytes as a protocol. Identify which values are persistent,
  transferable, process-local, and reconstructable.
- Treat `Repr` and `Layout` implementations as unsafe portability proofs;
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
- Keep Session protocol-neutral. Select protocol at typed Channel creation or
  Port admission, and keep custom recovery handlers explicit.
- Resolve queued Pool identity internally during Channel removal. Release Pool
  storage before recycling its Queue slot; never accept a caller-selected Pool.

## Verification expectations

Changes to observable behavior should establish the missing behavior first,
then implement the smallest coherent correction.

At minimum, select evidence from:

- mock-backend tests for layout and allocator logic;
- Unix mapping tests for platform behavior;
- Directory, Pool, and queue concurrency tests;
- type-mismatch and stale-generation negative tests;
- real two-process tests with different mapping bases;
- queue-full, disconnect, cancellation, and abrupt-peer-death tests;
- wrong-allocator, invalid-identity, duplicate-completion, and quarantine tests;
- release-mode benchmarks only after correctness checks pass.

Do not describe an unexecuted platform path, benchmark, or process boundary as
verified.

## Reduction and delivery discipline

- Production, inline tests, source tests, integration tests, benchmarks,
  benchmark tests, examples, shipped docs, and ignored plans have separate LOC
  ledgers. Never move code between them to manufacture a reduction.
- A private helper must own validation, authority, or cleanup and delete more
  repetition than it adds. Do not introduce macros or traits for one-use
  mechanical sharing.
- Rustdoc states public contracts. Inline comments preserve nonlocal safety,
  atomic-ordering, or recovery proofs; delete syntax narration and copied
  architecture prose.
- Ordinary CI owns format, warning-denied Clippy, docs, feature checks, package
  inspection, and real process behavior on hosted Linux and Windows. Timed studies
  remain isolated and emit inspectable artifacts.
- Version `0.1.0` is published on crates.io. Do not tag or upload another version
  without separate explicit release authorization.

## Current goal

Keep the public Session, Pool, Channel/Port, process-resource exchange, Signals,
close, remove, and practical indexer flow coherent while freezing wire owners,
schema revisions, failure authority, documentation, and CI evidence. Minor API
changes are accepted only when they remove duplicated validation or make a
linear transition harder to misuse without adding hot-path work.
