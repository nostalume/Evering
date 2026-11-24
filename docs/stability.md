# What a surface here can be relied on for

This page names, for each public surface, a verdict and the checks in this repository
that pin it. It is not a release, a compatibility window, or a deprecation policy;
released boundaries are named by tags, and there is one so far (`v0.1.0`). The
argument behind a verdict is not here either — a row that moved is explained in the
commit that moved it, and this page moves in that same commit, which is the whole
notification promise here.

The split is a rule, not a habit. [architecture.md](architecture.md) owns the model
and its invariants; [porting.md](porting.md) owns what a base that implements the two
seams has to arrange; this page owns every "this was checked" sentence. Whether a
design choice is settled is recorded nowhere in the published tree. Where two pages
disagree: about a verdict, this page wins; about the model, architecture wins; about a
port's obligations, porting wins; and about the shape of the code, the source wins over
all three.

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
| `mapping::Source`, `mapping::Map` | checked | `tests/portable_contracts/static_source.rs` implements the seam the way a port does — one extent already in its own binary's image, no memfd, no descriptor, nothing to unmap; `section_source.rs` holds a Windows section to its declared access; `type_shape.rs` checks what the types alone answer. Independently, the core compiles where there is no libc to map with: `cargo check --no-default-features --target riscv32imac-unknown-none-elf` and `…--target aarch64-unknown-none`, both CI rows. |
| `notify::Notify`, `notify::Wait`, and what a latch must guarantee | checked | `tests/notification_latch/latch_clauses.rs` states the four clauses — sticky, coalescing without counting, cancelling takes nothing, one ring paired with one condition — and is what any new adapter, including a kernel-side provider, is measured against; `cross_process.rs` and both host backends execute them, so a wake that releases only its head waiter still re-examines shared truth. |
| Wire identity bytes (`PoolId`, `Port`, region ids) | checked | `tests/portable_contracts/identity_bytes.rs` fixes the exact words those occupy, the round trip that reads them back, and the malformed buffers that are refused rather than repaired. |
| Handover framing (`HANDOFF_MAGIC`, `ARRIVAL_MAGIC`, `FRAME_HEADER`, `FRAME_TAG`, `FRAME_LEN`, `FRAME_COUNT`) | provisional | Published as data: both tags, the width a frame's front is cut to, and the three offsets that tile it — the tag, then the bootstrap length and the resource count as little-endian `u16`s. `tests/handover_attestation/labels.rs` pins the tags apart, the offsets, and that `MAX_BOOTSTRAP` and `MAX_RESOURCES` still fit the field widths they are stamped into; a `const` assertion refuses a build where the offsets stop tiling the published width; every host writer and reader goes through the one `stamp_front`/`read_front` pair. Still not readable out of the crate: that a tag is a collision test between two frame kinds rather than a version a receiver negotiates. |
| Root-header layout (record, member-state encoding, geometry words) | provisional | Published as data: region alignment (`REGION_ALIGN`), the extent a block class requires (`BlockRange::required_extent`), and the cut of the one word a member's state and generation share (`layout::MEMBER_*`, pinned by `tests/portable_contracts/member_word.rs`) — so the word a non-Rust process writes is no longer something it has to copy out of `src`. Not published: which bytes belong to no field, the tail whose length differs between a 32- and a 64-bit target, and the byte order of an initial image. |
| A const-constructible initial region image | provisional, and its check is not in this repository | The typed route — a non-generic `const fn` over concrete `#[repr(C)]` atomics, placed in a `static` and handed to `Source` as `*mut u8` — compiled at both `*-none-elf` targets on stable while this row was written, and no file here compiles it today: the probe sits in a build directory, so `cargo clean` deletes the only evidence. Until that probe is a tracked test and the two CI rows above run it, this row is a claim about a toolchain, not about the crate. The same image as a byte array additionally needs the three unpublished header items above, and is not offered — no second implementation reads an initial image yet. |
| `layout` vocabulary (`SchemaKey`, `Repr`, `RegionId`, `recovery_handler`) | provisional | Names of identity and policy rather than of the wire, so recompiling is the entire consequence of a change. `tests/portable_contracts/type_shape.rs` is the in-tree consumer that has to spell `Repr` and `SchemaKey` correctly. |
| Public error types | checked as shapes | Every refusal prints and chains (`Display`, `Error::source`), and `cargo doc` under `-D warnings` gates the surface, including every intra-doc link to it — on both hosts, since the Windows job runs that row too. A link to an item only one platform builds is the failure that row exists to catch, which is why the death lane's second entrance is named in prose on the host that lacks it rather than linked. |
| Refusal sentence text | provisional | Integration legs compare the strings themselves — `tests/handover_admission` requires the same descriptor to be refused with the same words through both entrances — so a wording change is a test change, announced as one. |
| `process`, `runtime`, and the non-seam part of `os` | provisional | std- and `os`-gated, so a `no-std` reader never meets them, and their shape is what two hosts execute. The second death entrance is landed and Linux-only: `Writer`, `Death::credited`, and the `Witness::Process` arm behind them are gated to `target_os = "linux"`, so a host without it builds without them — the only witness it can hold is `Recorded`, which is why `Exited` carries its witness under a dead-code allowance there instead of reading it. |
| MSRV | a published fact, not a verdict | `rust-version = "1.100.0"`, with the same channel pinned in `rust-toolchain.toml`; the crate builds on stable and needs no nightly. |

## What a `no-std` build gets

With default features off you keep the byte, layout, storage, transport and
notification-model layers, plus the two seams above. `map`, `notify`, `process`, `os`,
`runtime`, `tokio`, `evidence`, `benchmark`, `plot` and `tracing` each ask for a host:
they add native mapping, doorbells, descriptor exchange, process supervision, async
waiting, or measurement. The word `no-std` in the crate's metadata means exactly this
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

## Rows still owed a check

- The initial-image row, until its probe is tracked; the CI rows above check the
  crate, not a `const` image placed in a `static`.
- Handover framing, until something other than this crate's own two host arms reads a
  frame. Both arms are the same pair of writers and readers, which is why the shared
  `stamp_front`/`read_front` owner is the fix and not the evidence.

## What this page does not promise

Compatibility windows, deprecation periods, byte-identical Rust ABIs across builds or
targets, recovery of ambiguous general-heap mutation, membership that cannot write,
automatic replay of application work, and performance outside a registered and matched
study condition. The behavioral reasons for those limits are in
[architecture.md](architecture.md); this page exists only to say which surfaces were
checked, against what, and how you would find out.
