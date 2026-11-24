# Which byte facts are published

This page owns two questions: which facts about shared bytes someone outside this crate has
to reproduce, and in what shape each one is handed over. Three neighbours own what this page
does not: [stability.md](stability.md) holds every "this was checked" claim,
[porting.md](porting.md) holds what a base implementing the two seams has to arrange, and
[architecture.md](architecture.md) holds the model those bytes serve.

## The test a fact has to pass

A byte-level fact is published here only when all three of these hold.

- Writing bytes a different implementation can read requires knowing it. Using it internally
  is not enough; somebody has to be unable to read the region without it.
- Getting it wrong corrupts quietly at the peer. A violation that trips this crate's own
  assertion is a defect here, not a promise broken out there.
- The crate can actually hand it over. A fact that cannot be handed over is a constraint on
  the design, and the honest response to it is to change the design, not to document it.

Four things stay outside that test deliberately:

- Process-relative values — pointers, callbacks, native handles, task wakers, a local
  reference count. None of them belongs in a shared byte, and seeing one in the shared words
  is a defect rather than a portable representation.
- Construction-time policy — address preference, `Patience`, allocator choice, the shape of
  an OS flag. Admission erases these, so a peer never has to copy them.
- Rust layout itself. Outside the `#[repr(C)]` items named below, no struct's layout is
  claimed compatible across builds or targets.
- Names shared only between two versions of this crate, such as the `layout` vocabulary and
  `SchemaKey`. They stay in the type surface, where the whole consequence of changing one is
  a recompile, so publishing them as data would freeze a thing that never had to be frozen.

## The shapes a fact is handed over in

| Shape | What it is | Use it when |
| --- | --- | --- |
| Named data | a `pub const` offset, width, or tag | the reader must reproduce a number in another process or language, and miscopying it is possible |
| Typed const construction | a `const fn` over concrete types, placed in a `static` | a Rust reader needs a whole initial region image rather than one field of it |
| Public function | a call whose answer depends on this mapping | the fact moves with a base address or a generation, so no constant can hold it |
| Prose | a sentence | it states only what a violation does. Prose never carries the only copy of a value a reader has to reproduce. |

Whether a number is written out or computed follows two rules:

- **A total a peer copies is a promise, so it is written literally.** Deriving a published
  width from the pieces that tile it would let one field change move a wire format with
  nobody approving the move.
- **Where the pieces sit is an inference, so it is derived.** Offsets, widths, and bit masks
  come from the widths of what occupies them, and a `const` assertion ties the derived sum
  back to the promised total. A number that is a function of another number is not written
  twice.

One published item shows both at once: `layout::MEMBER_STATE_BITS` states how many bits a
member's state occupies and is written literally, because that cut is what a peer copies;
`layout::MEMBER_STATE_MASK` and `layout::MEMBER_GENERATION_MAX` are computed from it; and a
`const` assertion pins `layout::MEMBER_RESERVED` to the fifth state with a value still free
beyond it inside those bits, so widening the cut and shrinking the headroom cannot happen in
the same edit unnoticed.

Derivation reads the width a value occupies on the wire, not the type a Rust field happens to
declare: a field stamped as one byte contributes one byte. The reverse mistake — deriving from
the declared type — reproduces the copy it was meant to remove, only hidden. An explicit
little-endian buffer is not a native field that `offset_of!` can reach either, so the byte
order of those two counts is stated on the items themselves rather than encoded in a type.

## What is published today

| Fact | Where it lives | Shape |
| --- | --- | --- |
| A frame's front: the two tags, the width the front is cut to, and the ranges that tile it | `process::HANDOFF_MAGIC`, `process::ARRIVAL_MAGIC`, `process::FRAME_HEADER`, `process::FRAME_TAG`, `process::FRAME_LEN`, `process::FRAME_COUNT` | named data; the two counts' encoding is stated on the range items |
| How many bytes an identity occupies | `PoolId::BYTE_LEN` and `Port::BYTE_LEN` | named data — and the totals only, see the note below |
| Which member states exist, and how one word is cut between a state and a generation | `layout::MEMBER_FREE`, `MEMBER_LIVE`, `MEMBER_DEAD`, `MEMBER_RESERVED`, `MEMBER_RETIRED`, `MEMBER_STATE_BITS`, `MEMBER_STATE_MASK`, `MEMBER_GENERATION_MAX` | named data, where the mask and the limit are derived from the cut and an assertion pins the boundary |
| What a region's alignment and a block class's width demand of an extent | `mapping::REGION_ALIGN` and `BlockRange::required_extent` | named data plus a public function, because the second answer depends on which class was asked for |
| That a retired generation is never handed out again | the recovery rules on [architecture.md](architecture.md) | prose: the consequence of violating it is a handover aimed at an old occupant, and there is no value to copy |
| The bytes a frame's front is assembled from | one `stamp_front`/`read_front` pair inside the crate | not published as a call: a peer reproduces the bytes in the first row, never the function |

One entry is deliberately narrow. A member can pass a `PoolId` or a `Port` across, and the
width each occupies is published, but the order of the segments inside them is not: turning an
id into bytes is crate-internal, and a `Port` reaches its bytes through a Rust call, so a reader
writing in another language holds two widths and no field table to fill them from. By the third
clause of the test above, that makes the internal order a constraint on this crate rather than a
promise to a reader — which is the honest description today, and the reason no row of numbers
here carries it.

An initial region image that a port can build without a host is a typed const construction,
and its current standing is recorded as a row on [stability.md](stability.md) rather than
described here: which route is checked and which is not is a verdict, and verdicts have one
page.

## Target-dependent quantities

Three facts move with the target: which bytes belong to no field, the tail whose length
follows pointer width, and the byte order of an image written by a different toolchain. None
of them is published.

The reason is a scope limit rather than an omission. Peers that disagree on target
width or byte order do not share a region: the record at a region's root carries the
geometry it was created with, and a member whose own geometry differs is refused when
it is admitted rather than served the region with translated field widths. It is that
refusal which leaves the three quantities above without a reader — a region created on
one target is never made readable on another — and whether the refusal is checked is a
row on [stability.md](stability.md), which owns that claim. A fact with no reader is not
a promise, so the gaps between fields have no consumer waiting for a number.

What a port legitimately needs — how much alignment a target enforces, and how much extent a
block class asks for there — stays queryable instead of copyable. Ask `mapping::REGION_ALIGN`
and `BlockRange::required_extent` rather than assuming either, and keep every block class
within the narrower alignment the two hosts support and both agree on. Reversing the scope
limit would mean publishing all three quantities above, and that is a design change, not a
documentation change.

## Changing something on this page

- Moving a published number is a wire-format event: it takes a schema or generation version
  event, not a quiet edit.
- Deriving a number does not downgrade that event. Derivation removes the risk that a copy
  drifts from what it describes; it does not make the value stop moving.
- Adding a typed construction freezes the field order it carries. That is a decision with a
  cost, and it is taken deliberately or not at all.
- This page states a fact's shape; [stability.md](stability.md) states what pins it, and
  [AGENTS.md](AGENTS.md) owns the rule that moves the two together.
- Tags, version numbers, and uploads are authorized separately, each time. Nothing on this
  page is such an authorization.
