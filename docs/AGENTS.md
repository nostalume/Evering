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

- Rust 2024 edition on stable Rust, pinned to `1.100.0` in [`rust-toolchain.toml`](../rust-toolchain.toml),
  which is also the stated `rust-version`: the compilers below it were run and
  fail on `allocator_api`, `unsafe_cell_access`, and a `const`
  `UnsafeCell::as_ref_unchecked`.
- One root `evering` crate with default standard-library support and a verified
  `no_std + alloc` configuration when default features are disabled.
- Additive `map`, `notify`, and `process` adapter capabilities, with `os` as
  their native-platform sugar; Tokio and tracing remain orthogonal features.
- Const generics and const-evaluated schema-key arithmetic for statically
  specialized layouts and policies. No `const` traits: the type-level identity is
  a `const fn` over values, and the shape rule takes a pointer's metadata, so it
  is evaluated where the metadata exists.
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
- Name a shared word where it is declared. No shared field carries visibility
  outside its module, so a reader elsewhere reaches it through something the
  declaring module publishes: a value accessor, or, for a layout computed from a
  base address the way pools and queue records are, an accessor that names the
  location. A new hand-out of raw shared bytes is that second shape and owes a
  reason at its declaration.
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

A claim that needs no kernel object is written once, in [`src/tests.rs`](../src/tests.rs), and each
backend tree hands it a view. A copy in [`src/tests/mock`](../src/tests/mock) or [`src/tests/unix`](../src/tests/unix)
keeps passing while the two readings drift apart, so
[`tests/backend_adapters.rs`](../tests/backend_adapters.rs) fails the day a name both trees spell stops
delegating.

Do not describe an unexecuted platform path, benchmark, or process boundary as
verified.

An example is a process, so it is gated as one: a CI row runs it and asserts on
the lines its legs print and on its exit code. No test target can take that place,
because a leg that hands over is a leg that spawns, and a spawning leg names its
child's role as an option on `current_exe()` — which the test harness refuses
before any role body could answer for it.

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
  inspection, and real process behavior. Not every gate runs on every host, and the
  division is a fact about the workflow rather than a promise: format and Clippy read the
  source once, so they run on hosted Linux alone
  (`grep -n "run: cargo fmt" .github/workflows/ci.yml` matches one line, and the Windows job
  has no step invoking either), while the docs gate, the test arms, and the examples run as
  processes on both hosts. Timed studies
  remain isolated and emit inspectable artifacts.
- Version `0.1.0` is published on crates.io. Do not tag or upload another version
  without separate explicit release authorization.

## Current goal

Keep the public Session, Pool, Channel/Port, process-resource exchange, Signals,
close, remove, and practical indexer flow coherent while freezing wire owners,
schema revisions, failure authority, documentation, and CI evidence. Minor API
changes are accepted only when they remove duplicated validation or make a
linear transition harder to misuse without adding hot-path work.

## Which page owns which statement

Each kind of statement has one owner, and a second copy of it is a defect rather
than a convenience:

- [`docs/architecture.md`](architecture.md) owns the model and its invariants — what the layers are,
  and which rules a change must not break.
- [`docs/stability.md`](stability.md) owns every "this surface was checked" claim. A verdict row
  cites checks that could disagree: a file that consumes the surface from outside
  the crate, plus something that compiles or runs it under a different
  configuration. Two arms of one host pair are one check. A row whose check is not
  in the repository says so in the row rather than citing an artifact that does
  not ship.
- [`docs/porting.md`](porting.md) owns what a base implementing the two seams has to arrange.
- [`docs/abi.md`](abi.md) owns which shared-byte facts a reader outside this crate has to reproduce, and
  the shape each is handed over in. A fact that fails that page's test is a constraint on the
  design rather than an entry on it.
- [`README.md`](../README.md) owns the first-run path: what to install, what to run, and what a
  printed measurement means.
- This page owns engineering rules: toolchain, gates, naming, verification, and
  delivery.

Whether a design choice is settled, and what work follows from it, belongs to none
of these pages and is not published with the crate.

A change to what a page states moves every page that states something about it into the same
commit. An edit that leaves another page describing the old shape is an incomplete change, not
a follow-up task.

Where two pages disagree, the owning page of that kind of statement wins: the verdict
page about a check, the architecture page about the model, the porting page about a
port's obligations, the ABI page about what a peer has to reproduce, this page about an
engineering rule. About the shape of the code, the source wins over every one of them.
A page may cite another page's statement, which is not a second copy; restating it in
one's own words is, and is the defect this section exists to prevent.

Between a document and the source, an anchor names a symbol (`src/mem.rs:
Map::reserve`), never a line number: line numbers rot while symbol lookups
resolve. A commit is cited by its delivery block, because squashing renumbers the
rest.
