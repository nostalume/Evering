# What a surface here can be relied on for

This page names, for each public surface, a verdict and the checks in this repository
that pin it. It is not a release, a compatibility window, or a deprecation policy;
released boundaries are named by tags, and there are two so far (`v0.1.0`, `v0.2.0`). The
argument behind a verdict is not here either — a row that moved is explained in the
commit that moved it, and the same-commit rule in [AGENTS.md](AGENTS.md) is the whole
notification promise here.

This page holds every "this was checked" sentence about a surface of this crate, and
[AGENTS.md](AGENTS.md) assigns the other kinds of statement their pages.

## The three verdicts

| Verdict | What it means | How you learn it moved |
| --- | --- | --- |
| checked | Two independent checks pin the shape a consumer implements or calls. | Your build fails. |
| provisional | Usable today; the shape still moves. | Named in this row, in the commit that moves it. |
| absent | Not present in that build configuration. | Nothing to break. |

Two checks count as independent only if they could disagree: a file that consumes the
surface from outside the crate, plus something that compiles or runs it under a
different configuration. Two arms of one host pair are one check, not two. `checked`
never means "will not change" — it means a change that reaches you makes your build
fail rather than surprises your runtime. No row here uses an adjective we cannot point
a command at, and a row whose check is missing says so in the row rather than resting
on an artifact that is not in this repository.

## Surfaces

| Surface | Verdict | The checks it rests on |
| --- | --- | --- |
| `mapping::Source`, `mapping::Map` | checked | [`tests/portable_contracts/static_source.rs`](../tests/portable_contracts/static_source.rs) implements the seam the way a port does — one extent already in its own binary's image, no memfd, no descriptor, nothing to unmap; [`tests/portable_contracts/section_source.rs`](../tests/portable_contracts/section_source.rs) holds a Windows section to its declared access; [`tests/portable_contracts/type_shape.rs`](../tests/portable_contracts/type_shape.rs) checks what the types alone answer. Independently, the core compiles where there is no libc to map with: `cargo check --no-default-features --target riscv32imac-unknown-none-elf` and `…--target aarch64-unknown-none`, both CI rows. |
| `notify::Notify`, `notify::Wait`, and what a latch must guarantee | checked | [`tests/notification_latch/latch_clauses.rs`](../tests/notification_latch/latch_clauses.rs) states the four clauses — sticky, coalescing without counting, cancelling takes nothing, one ring paired with one condition, a pairing a handover carries across processes by moving its reading end and borrowing its writing end — and is what any new adapter, including a kernel-side provider, is measured against; [`tests/notification_latch/cross_process.rs`](../tests/notification_latch/cross_process.rs) and both host backends execute them, so a wake that releases only its head waiter still re-examines shared truth. |
| Wire identity bytes (`PoolId`, `Port`, region ids) | checked | [`tests/portable_contracts/identity_bytes.rs`](../tests/portable_contracts/identity_bytes.rs) fixes the exact words those occupy, the round trip that reads them back, and the malformed buffers that are refused rather than repaired. |
| Handover framing (`HANDOFF_MAGIC`, `ARRIVAL_MAGIC`, `FRAME_HEADER`, `FRAME_TAG`, `FRAME_LEN`, `FRAME_COUNT`) | provisional | Published as data: both tags, the width a frame's front is cut to, and the three offsets that tile it — the tag, then the bootstrap length and the resource count as little-endian `u16`s. [`tests/handover_attestation/labels.rs`](../tests/handover_attestation/labels.rs) pins the tags apart, the offsets, and that `MAX_BOOTSTRAP` and `MAX_RESOURCES` still fit the field widths they are stamped into; a `const` assertion refuses a build where the offsets stop tiling the published width; every host writer and reader goes through the one `stamp_front`/`read_front` pair. Still not readable out of the crate: that a tag is a collision test between two frame kinds rather than a version a receiver negotiates. |
| Root-header layout (record, member-state encoding, geometry words) | provisional | Published as data: region alignment (`REGION_ALIGN`), the extent a block class requires (`BlockRange::required_extent`), and the cut of the one word a member's state and generation share (`layout::MEMBER_*`, pinned by [`tests/portable_contracts/member_word.rs`](../tests/portable_contracts/member_word.rs)) — so a non-Rust process writes that word from published numbers rather than copying it out of `src`.<br>**Not published:** which bytes belong to no field, the tail whose length differs between a 32- and a 64-bit target, and the byte order of an initial image. |
| A const-constructible initial region image | provisional | **What runs it:** [`probe/bare_port`](../probe/bare_port/Cargo.toml) is a `#![no_std]` crate that depends on this one by path with default features off, and CI compiles it at a bare target (`cargo check --manifest-path probe/bare_port/Cargo.toml --target riscv32imac-unknown-none-elf`), so the claim is about the crate rather than about a toolchain.<br>**Both routes compile:** the typed one — a non-generic `const fn` over concrete `#[repr(C)]` atomics, placed in a `static` whose address is taken as the `*mut u8` that `Map::from_static` asks for — and the same image as `[u8; N]`, which constructs only where every byte belongs to a field, with the padding lengths computed from a twin type instead of copied from whichever host the author is sitting on. Two deliberate breakages showed the build reads the `const` assertions: a false gap bound, and a failing `static` initializer, each of which stops the bare-target build with `error[E0080]`.<br>**Why it stays provisional:** what no compile can pin is that the header type is not nameable from outside at all — `header::Record` and `header::RcHeader` are `pub(crate)` — so the image is the port's own code and the only cross-check is against two derived numbers, `layout::HEADER_PREFIX_BYTES` and `layout::HEADER_PREFIX_ALIGN`, which the probe's `const` assertion compares its own `Recorded` type's size and alignment to. A byte image of a real region still needs the three unpublished header items above.<br>**Not offered:** an image as API surface. Handing one out is an addition rather than a documentation change, and no second implementation reads an initial image. |
| `layout` vocabulary (`SchemaKey`, `Repr`, `RegionId`, `Metadata`, `recovery_handler`) | provisional | Names of identity, shape and policy rather than of the wire, so recompiling is the entire consequence of a change. [`tests/portable_contracts/type_shape.rs`](../tests/portable_contracts/type_shape.rs) is the in-tree consumer that has to spell `Repr` and `SchemaKey` correctly, and covers both answers a `?Sized` payload's shape gives — the slice metadata and the refusal of the wrong kind — so it is the first thing to fail if any of those stops being nameable. |
| Public error types | checked as shapes | Every refusal prints and chains (`Display`, `Error::source`), and `cargo doc` under `-D warnings` gates the surface, including every intra-doc link to it — on both hosts, since the Windows job runs that row too. A link to an item only one platform builds is the failure that row exists to catch, which is why the death lane's second entrance is named in prose on the host that lacks it rather than linked. |
| Refusal sentence text | provisional | Integration legs compare the strings themselves — [`tests/handover_admission`](../tests/handover_admission) requires the same descriptor to be refused with the same words through both entrances — so a wording change is a test change, announced as one. |
| `process`, `runtime`, and the non-seam part of `os` | provisional | std- and `os`-gated, so a `no-std` reader never meets them, and their shape is what two hosts execute. The second death entrance is landed and Linux-only: `Writer`, `Death::credited`, and the `Witness::Process` arm behind them are gated to `target_os = "linux"`, so a host without it builds without them — the only witness it can hold is `Recorded`, which is why `Exited` carries its witness under a dead-code allowance there instead of reading it. |
| A region shared by peers of different target width or byte order | provisional, and the second check that would settle it is not in this repository | **Refused, not unguarded:** the root record persists the word width, atomic width and byte order it was created with (`header::AbiProfile`), and `header::recorded_at` compares that profile against the reader's own before it reads any natural-width word, so a foreign geometry is refused with `Error::LayoutMismatch(LayoutField::Profile)` and the discovery route yields `Error::InvalidHeader` rather than a half-read region. The record's own bytes are target-independent — `#[repr(C)]`, align 1, `size_of` 35, both asserted in a `const` block — which is what makes the refusal reachable instead of a misread.<br>**Both entrances execute:** `header::admission_tests` composes one region, forges a recorded profile of the other word width, asserts the profile refusal on the member entrance, and reads the same region back through the discovery route.<br>**The check that is owed:** none of those tests supplies a profile another target's compiler wrote — each is forged on the host that reads it, which is why the row is one check rather than two. `grep -n i686 .github/workflows/ci.yml` answers a target-install line and one `cargo check` line, with no test target behind either. Cross-target sharing stays out of scope, which is why the quantities that differ between targets stay unpublished ([abi.md](abi.md)). |
| MSRV | a published fact, not a verdict | `rust-version = "1.100.0"`, with the same channel pinned in [`rust-toolchain.toml`](../rust-toolchain.toml); the crate builds on stable and needs no nightly. The floor is where the library became sufficient rather than a bound agreed to maintain, so raising it is a one-line decision and lowering it is a code change. |

## What a `no-std` build gets

With default features off you keep the byte, layout, storage, transport and
notification-model layers, plus the two seams above. `map`, `notify`, `process`, `os`,
`runtime`, `tokio`, `evidence`, `benchmark` and `plot` each ask for a host: they add
native mapping, doorbells, descriptor exchange, process supervision, async waiting, or
measurement. `tracing` asks for nothing but `std` and adds no capability: it builds the
`DEBUG` events that name each exit a declined `Session::reap` stopped at and each
mutation of the shared heap's free-space arrays, which is how those reasons reach a
caller that has no reason type to match on. It is a separate switch from the one that
actually checks anything — the heap's `scan_errors` sweep is compiled by profile, not by
feature — so a release build run with `tracing` on reads the bookkeeping and never sweeps
the arrays it describes. The word `no-std` in the crate's metadata means exactly this
section, and nothing else.

Nothing else about the core is host-gated. `Session::create`, `open_as`, `reserve_seat`,
`create_channel` and `adopt` are reachable with no libc, so such a port composes a
region, seats members, and transports without writing an adapter; the features remove
`impl std::error::Error` blocks, not methods.

What a port cannot get without the handover seam is proof that a member stopped: no
descriptor inheritance to grant, no peer credential to attest against, no exit status
to reap. Recovery there has one entrance, `Session::assume_dead`, and it is `unsafe`
because the caller rather than the crate is the authority for it. A static roster says
which slots are occupied; it never says an occupant stopped, so a roster narrows what
an assertion can be aimed at and does not replace the assertion. Membership therefore
arrives static, and `Observer`-style reading needs no membership at all, which is why a
monitor that cannot write shared truth is the one process-free shape that does exist.
What such a port is still owed by this crate is the layout row above, not a new
abstraction — and the arrangement a base has to sit in, which is not a layout fact at
all, is in [porting.md](porting.md).

## What the checks cover

The rows above span model-level interleavings, typed admission failures, Directory and Pool
generation reuse, queue contention and closure, malformed resource exchange, different-base
process mappings, asynchronous notification, and bounded worker recovery. Two arms are
exercised natively, `x86_64-unknown-linux-gnu` and `x86_64-pc-windows-msvc`; the arrival
receipt is executed on the first alone, because its tests name Unix, so that leg of the
handover seam is compiled but untested on Windows.

Examples run as processes rather than as prose. One owner holds the commands —
[`.github/scripts/ci-examples.sh`](../.github/scripts/ci-examples.sh) — both arms call it, and
each row asserts on the lines its legs print and on the exit code, which is what makes an
example gate-run instead of only compiled. By the rule above that is one check and not two,
since two arms of one host pair are one check.

Each adapter layer is also built and run at the width of its own feature: `map` alone runs a
test target on both arms, while `cargo check --no-default-features` and the three bare or
32-bit target rows run on the Linux arm alone. A layer that quietly needs a dependency
feature some wider layer turns on composes only in name, and that is the failure these
narrow rows exist to catch.

## Rows still owed a check

- The cross-geometry refusal's second check. One host already composes a region and
  admits a member whose recorded profile is not its own, through both entrances; what
  is owed is the same refusal against a record a build of the other width wrote, which
  needs an `i686` leg that executes rather than compiles — the CI row checks that the
  crate builds at that width, not that a 32-bit member and a 64-bit region refuse each
  other. The probe added to the row above does not supply this: it is compiled and never
  run, and the image it builds is compiled by the host that reads it.

## What this page does not promise

Compatibility windows, deprecation periods, byte-identical Rust ABIs across builds or
targets, recovery of ambiguous general-heap mutation, membership that cannot write,
automatic replay of application work, and performance outside a registered and matched
study condition. The behavioral reasons for those limits are in
[architecture.md](architecture.md); this page exists only to say which surfaces were
checked, against what, and how you would find out.

Handover framing carries a second kind of limit, and it is listed here rather than above
because no work would lift it: the two host arms are the same pair of writers and readers, so
the shared `stamp_front`/`read_front` owner removed a defect and did not add a check, and no
implementation outside this crate is expected to read a frame. The row therefore stays
`provisional` permanently. A check that will not be pursued is a limit on what the page can
say, not a debt this repository owes somebody.
