# Placing a region that is already in your image

For a port with no mapping call — bare metal, a microkernel-resident component, a
firmware image where the shared extent exists because the linker put it there. The two
seams you implement are `mapping::Source`/`Map` and `notify::Notify`/`Wait`; nothing
else in the crate asks to be replaced. What this page adds to
[the verdicts](stability.md) is the arrangement a base has to be in, which today lives
only inside `tests/portable_contracts/static_source.rs`.

## The three numbers, and which one you cannot compute

| What | Where it comes from | Why it is that shape |
| --- | --- | --- |
| The bound your base sits on | `mapping::REGION_ALIGN`, published: `1 << 21` on unix, `1 << 16` elsewhere | It is a power of two, so reaching it is one `next_multiple_of` and nothing else. A base below the bound is what a joining member is refused for. |
| The slack your extent needs | arithmetic on the published constant: `extent + REGION_ALIGN` | You reserve the slack once and slide into it, instead of asking the image to be aligned to a bound the compiler cannot honor. |
| The alignment your extent declares | a **literal** you write, e.g. `#[repr(C, align(4096))]` | `repr(align)` accepts only a literal integer — a constant is rejected (`E0693`), so no published constant can replace this number, and it is yours to choose. Choose one the toolchain will place: on `x86-64-pc-windows-msvc`, a static aligned past 4 KiB aborts the compiler, measured from 8 KiB up on every toolchain on that host. |

The pattern, then, is: declare the extent at an alignment your linker and toolchain
both honor, reserve `extent + REGION_ALIGN` bytes, and hand `Map::from_static` the first
address inside it that carries no residue. Arranging before placing is the whole trick —
the same thing `os::unix` does when it reserves a window and then places a file in it.

## What the region still needs at run time

Bytes that are only *placed* are not yet a region. Composition writes the root header and
the directory into your extent, so:

- the extent must be **writable**, which is why the test backend uses one mutable extent
  rather than a `static`;
- a read-only extent is refused by the write that composition has to make, not by a check
  afterwards;
- shipping a **pre-initialized** image instead of composing at boot is possible in
  principle and deliberately not offered: a fully initialized region carries a directory
  and pool state as well as a header, its byte layout is [provisional](stability.md), and
  today no port needs it. If you do, say so — the shape you would need is a `const fn`
  over the concrete header type, which compiled at
  `riscv32imac-unknown-none-elf` and `aarch64-unknown-none` on stable in a probe that is
  not part of this tree (the verdict page records that row as one whose check is
  missing), whereas the same
  image as a byte array additionally pins which bytes belong to no field, the
  target-dependent tail, and endianness.

## What you can do with no host at all

Composing a region, seating members, and transporting are not host features:
`Session::create`, `open_as`, `reserve_seat`, `create_channel` and `adopt` are all
reachable with default features off. What the features remove is the `impl
std::error::Error` blocks and the two adapters, not the methods. A port therefore does
not need a third seam to get a working region — it needs the two below and a layout.

## What does not exist here

The handover seam: there is no descriptor inheritance to grant, no peer credential to
attest against, and no exit status to reap, so process admission is not a degraded port —
it is absent. Membership therefore arrives as a static roster you supply.

Recovery does not get a host-free form of that roster. The only entrance that needs no
death evidence from a host is `Session::assume_dead`, which is `unsafe` precisely
because you, not the crate, are the authority for it. A roster says which slots are
occupied; it never says an occupant stopped, so it can narrow which assertion you are
allowed to make and cannot substitute for one. A port that wants recovery without an
assertion has to bring the proof itself — a hypervisor notice, a watchdog token, a
generation it cannot increment — and hand it to `assume_dead`'s caller.
`Observer`-style reading needs no membership either, so a monitor that cannot write
shared truth is the one process-free shape that does exist.

## Checking your own target

```console
cargo check --no-default-features --target riscv32imac-unknown-none-elf
cargo check --no-default-features --target aarch64-unknown-none
```

Substitute your target. A clean compile means the core and both seam signatures name
types your target has; it does not mean anything ran, and the distinction is what
`checked`, `provisional`, and `absent` are for on the previous page.
