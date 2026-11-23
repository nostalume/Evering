# What a surface here can be relied on for

This page records what each public surface has been *checked against*, not what we
promise to keep. It is not a release, a compatibility window, or a deprecation
policy; released boundaries are named by tags, and there is one so far (`v0.1.0`).
When a row below moves, this page moves in the same commit, which is the whole
notification promise here.

`checked` never means "will not change". It means the consumer-side shape was
examined from outside the crate as well as from inside, so a change that reaches you
makes your build fail rather than surprises your runtime. No row here uses an
adjective we cannot point a command at.

## The three verdicts

| Verdict | What it means | How you learn it moved |
| --- | --- | --- |
| checked | Two independent checks pin the shape a consumer implements or calls. | Your build fails. |
| provisional | Usable today; the shape still moves. | Named in this row, in the commit that moves it. |
| absent | Not present in that build configuration. | Nothing to break. |

## Surfaces

| Surface | Verdict | The checks it rests on |
| --- | --- | --- |
| `mapping::Source`, `mapping::Map` | checked | `tests/portable_contracts/static_source.rs` implements the seam the way a port does — one extent already in its own binary's image, no memfd, no descriptor, nothing to unmap; `section_source.rs` holds the Windows section to its declared access; `type_shape.rs` checks what the types alone answer. Independently, the core compiles where there is no libc to map with: `cargo check --no-default-features --target riscv32imac-unknown-none-elf` and `…--target aarch64-unknown-none`. |
| `notify::Notify`, `notify::Wait`, and what a latch must guarantee | checked | `tests/notification_latch/latch_clauses.rs` states the four clauses — sticky, coalescing without counting, cancelling takes nothing, one ring paired with one condition — and is what any new adapter, including a kernel-side provider, is measured against; both host backends execute them, so a wake that releases only its head waiter still re-examines shared truth. |
| Wire identity bytes (`PoolId`, `Port`, region ids) | checked | `tests/portable_contracts/identity_bytes.rs` fixes the exact words those occupy, the round trip that reads them back, and the malformed buffers that are refused rather than repaired. |
| Handover framing (`HANDOFF_MAGIC`, `ARRIVAL_MAGIC`, `FRAME_*`) | provisional | Published as data: both tags, the width a frame's front is cut to (`process::FRAME_HEADER`), and the three offsets that tile it (`FRAME_TAG`, `FRAME_LEN`, `FRAME_COUNT`) — the tag, then the bootstrap length and the resource count as little-endian `u16`s. A `const` assertion refuses a build where the three offsets stop tiling the published width, and `tests/handover_attestation/labels.rs` pins the tags apart, the offsets, and the fact that `MAX_BOOTSTRAP` and `MAX_RESOURCES` still fit the field widths they are stamped into. Both host writers and both readers now go through one `stamp_front`/`read_front` pair, which is the point: a frame never crosses from one host's writer to the other's reader, so nothing but a shared owner could ever have told the two arms' hand-rolled copies apart. What another implementation still cannot read out of the crate is that the tag is a collision test between two frame kinds rather than a version a receiver negotiates. |
| Root-header layout (record, member-state encoding, geometry words) | provisional, and the reason it is provisional is specific | Published as data: region alignment (`REGION_ALIGN`), the extent a block class requires (`BlockRange::required_extent`), and the cut of the one word a member's state and generation share (`layout::MEMBER_*`, pinned by `tests/portable_contracts/member_word.rs`) — so the word a non-Rust process writes is no longer something it has to copy out of `src`. Not published: which bytes belong to no field, the tail whose length differs between a 32- and a 64-bit target, and the byte order of an initial image. The first two are target-dependent padding, the third is endianness; a port that hand-copies header bytes is guessing all three today. |
| A const-constructible initial region image | provisional; today only as a typed `static` | A non-generic `const fn` over concrete `#[repr(C)]` atomics, placed in a `static` and handed to `Source` as `*mut u8`, compiles at both `*-none-elf` targets on stable. The same image as a byte array additionally requires the three items named just above, and it is not offered because no consumer needs it yet — publishing it would freeze a field order for a reader who does not exist. |
| `layout` vocabulary (`SchemaKey`, `Repr`, `RegionId`, `recovery_handler`) | provisional | Names of identity and policy rather than of the wire: recompiling is the entire consequence of a change. |
| Public error types | checked as shapes | Every refusal prints and chains (`Display`, `Error::source`), and `cargo doc` under `-D warnings` gates the surface, including every intra-doc link to it — on both hosts, since the Windows job runs that row too now. A link to an item only one platform builds is the failure that row exists to catch, which is why the death lane's second entrance is named in prose on the host that lacks it rather than linked. |
| Refusal sentence text | provisional | Integration legs compare the strings themselves — `tests/handover_admission` requires the same descriptor to be refused with the same words through both entrances — so a wording change is a test change, announced as one. |
| `process`, `runtime`, and the non-seam part of `os` | provisional | std- and `os`-gated, so a no-std reader never meets them; their shape is what two hosts execute today, and the death lane's second entrance is still open work. |
| MSRV | a published fact, not a verdict | `rust-version = "1.100.0"`, with the same channel pinned in `rust-toolchain.toml`; the crate builds on stable and needs no nightly. |

## What a `no-std` build gets

With default features off you keep the byte, layout, storage, transport and
notification-model layers, plus the two seams above. `map`, `notify`, `process`,
`os`, `runtime`, `tokio`, `evidence`, `benchmark`, `plot` and `tracing` each ask for a
host: they add native mapping, doorbells, descriptor exchange, process supervision,
async waiting, or measurement. The word `no-std` in the crate's metadata means
exactly this section, and nothing else.

A bare-metal or microkernel port therefore implements the region seam and the
doorbell seam and supplies its own roster. There is no handover seam where there is
no descriptor inheritance, no peer credential to attest against, and no exit status
to reap, so membership arrives static, and recovery keys on the roster rather than
on an `assume_dead` the port could only assert, never prove. What such a port is
still owed by this crate is the layout row above, not a new abstraction — and the
arrangement a base has to sit in, which is not a layout fact at all, is in
[porting.md](porting.md).

## What this page does not promise

Compatibility windows, deprecation periods, byte-identical Rust ABIs across builds or
targets, recovery of ambiguous general-heap mutation, membership that cannot write,
automatic replay of application work, and performance outside a registered and
matched study condition. The behavioral reasons for those limits are in
[architecture.md](architecture.md); this page exists only to say which surfaces were
checked, against what, and how you would find out.
