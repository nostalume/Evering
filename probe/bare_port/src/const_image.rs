//! The question this file compiles instead of answering in prose: can a target with no libc under it build
//! an initial region image in `const` context, and in which shape? Nothing here reaches inside `evering`:
//! the crate's header type is not published at all, so a port that wants its region already initialized when
//! it boots builds the image itself and compares it against the two numbers the crate does hand over,
//! `evering::layout::HEADER_PREFIX_BYTES` and `HEADER_PREFIX_ALIGN`. What is modelled below is the shipped
//! field order, and shape five is pinned to it: its length and alignment are asserted against those two
//! numbers. Every other claim here is about the order, never about a type this repository hands out.
//!
//! Five shapes, all over concrete `#[repr(C)]` structs of atomics — no generic `const fn`, so none of them
//! asks stable for the generic const surface it does not have:
//!
//! * a **typed static** — the port names its header type, writes `static IMAGE: Header = Header::initial(…)`,
//!   and hands the address of the whole value to `Map::from_static`. Needs only that the initial words be a
//!   non-generic `const fn`.
//! * a **byte image** — the same image as `[u8; N]`, so the port needs no type at all, only the array. Needs
//!   a const reinterpretation of a struct of atomics as bytes, and that is what has to be *compiled* to be
//!   believed.
//!
//! The measured answer, at `rustc 1.100.0` on `riscv32imac-unknown-none-elf`, `aarch64-unknown-none` and
//! this host: the typed static compiles; a byte image compiles **iff every byte belongs to a field**, and
//! which bytes do not is target-dependent — so gaps have to be *computed* from a twin type, never written
//! down from whichever host the author is sitting on. Shapes four and five model the shipped header order,
//! and there naming the padding changes no offset and no size, because those bytes are padding already.
//!
//! The `const _NAME: () = assert!(…)` blocks are the load-bearing half: `cargo check` evaluates them, so a
//! layout that stops tiling, or a magic halfword that stops landing little-endian at its own field, fails
//! the build instead of ageing silently.

#![allow(dead_code)]

use core::sync::atomic::{AtomicU8, AtomicU32, AtomicUsize};

const CAPACITY: usize = 64;

/// The field classes a header is built from, in the shape this question needs: one magic halfword, a schema
/// id and revision, control words that are written atomically (`members`, the member array), and plain `u64`
/// words. Shapes one to four are chosen to reproduce the compiler's padding, not to copy the crate: the word
/// list here is longer than the shipped header's, and `Shipped` below is the one that models its order.
/// That split is not decoration — `riscv32imac` has no `AtomicU64` at all, so a header whose sizes were
/// 64-bit atomics could not be const-built on the target this crate is checked at.
#[repr(C)]
pub struct Header {
    magic: u16,
    schema: AtomicU32,
    revision: u32,
    members: AtomicUsize,
    region: u64,
    offset: u64,
    header_size: u64,
    header_align: u64,
    info_size: u64,
    body_size: u64,
    body_align: u64,
    slots: [AtomicUsize; CAPACITY],
}

impl Header {
    /// The initial words of a fresh region, as a non-generic `const fn` over concrete atomics.
    pub const fn initial(region: u64, body_size: u64, body_align: u64) -> Self {
        Self {
            magic: 0xABCD,
            schema: AtomicU32::new(0x0001_0002),
            revision: 2,
            members: AtomicUsize::new(0),
            region,
            offset: 0,
            header_size: core::mem::size_of::<Self>() as u64,
            header_align: core::mem::align_of::<Self>() as u64,
            info_size: 0,
            body_size,
            body_align,
            slots: [const { AtomicUsize::new(0) }; CAPACITY],
        }
    }
}

/// Shape one: the port's own image is a typed static, and the doorbell-free `Source` hands out its address.
static IMAGE: Header = Header::initial(0x0053_E480, 1 << 16, 4096);

/// Shape two, run one — the same image as bytes, produced in const context from the typed value.
///
/// **This is the half stable refuses, and it refuses it for a reason a layout spec has to answer, not a
/// reason the compiler can be talked out of.** `core::mem::transmute` is usable in const context here, and
/// the field values come through, but `#[repr(C)]` puts two padding bytes after `magic: u16` before
/// `schema: AtomicU32`, and those bytes are never written by any initializer:
///
/// ```text
/// error[E0080]: constructing invalid value of type [u8; 328]: at [2], encountered uninitialized memory,
///     but expected an integer
/// ```
///
/// So a byte image is not constructible for a layout that has gaps — and the fix is not a cast, it is the
/// gaps being named. `Sealed` below is the same header with its padding written down as fields.
#[cfg(any())] // Disabled after the run that answered the question; the error text above is its record.
const HEADER_BYTES: [u8; core::mem::size_of::<Header>()] =
    const { unsafe { core::mem::transmute(Header::initial(0x0053_E480, 1 << 16, 4096)) } };

/// The same layout with every byte owned by a field, which is the shape a published byte image has to be.
#[repr(C)]
pub struct Sealed {
    magic: u16,
    /// The two bytes `#[repr(C)]` would otherwise leave for no one to write.
    pad: [u8; 2],
    schema: AtomicU32,
    revision: u32,
    members: AtomicUsize,
    region: u64,
    offset: u64,
    header_size: u64,
    header_align: u64,
    info_size: u64,
    body_size: u64,
    body_align: u64,
    slots: [AtomicUsize; CAPACITY],
}

impl Sealed {
    pub const fn initial(region: u64, body_size: u64, body_align: u64) -> Self {
        Self {
            magic: 0xABCD,
            pad: [0; 2],
            schema: AtomicU32::new(0x0001_0002),
            revision: 2,
            members: AtomicUsize::new(0),
            region,
            offset: 0,
            header_size: core::mem::size_of::<Self>() as u64,
            header_align: core::mem::align_of::<Self>() as u64,
            info_size: 0,
            body_size,
            body_align,
            slots: [const { AtomicUsize::new(0) }; CAPACITY],
        }
    }
}

/// Shape two, run two — the sealed layout as a byte image. **Refused too, and at a different offset, which
/// is the finding worth more than the first one:**
///
/// ```text
/// error[E0080]: constructing invalid value of type [u8; 592]: at [12], encountered uninitialized memory,
///     but expected an integer
/// ```
///
/// `pad: [u8; 2]` closed the gap riscv had and opened the one a 64-bit target has: there `members` is an
/// 8-byte atomic that follows a 4-byte `revision`, so the gap moved rather than went away. A byte image
/// cannot be sealed by naming the gaps *this host* happens to have; it needs a field order that leaves no
/// interior gap on any target. That is shape three.
#[cfg(any())] // Disabled after the run that answered it; the two error texts above are its record.
const SEALED_BYTES: [u8; core::mem::size_of::<Sealed>()] =
    const { unsafe { core::mem::transmute(Sealed::initial(0x0053_E480, 1 << 16, 4096)) } };

static SEALED: Sealed = Sealed::initial(0x0053_E480, 1 << 16, 4096);

/// Shape three: the same words, ordered by descending alignment, with a tail field sized `0`.
#[repr(C)]
pub struct Ordered {
    region: u64,
    offset: u64,
    header_size: u64,
    header_align: u64,
    info_size: u64,
    body_size: u64,
    body_align: u64,
    members: AtomicUsize,
    schema: AtomicU32,
    revision: u32,
    magic: u16,
    slots: [AtomicUsize; CAPACITY],
    /// The tail gap, named — but naming the tail is not the same as owning every byte.
    tail: [u8; Self::TAIL],
}

impl Ordered {
    /// `0` is the claim, not an assumption. On a 64-bit target the seven `u64`s and the 8-byte `members`
    /// close on 64, the two four-byte words on 72 and the halfword on 74 — and 74 is not a multiple of the
    /// 8-byte alignment `slots` brings with it there. That is where shape three fails.
    const TAIL: usize = 0;

    pub const fn initial(region: u64, body_size: u64, body_align: u64) -> Self {
        Self {
            region,
            offset: 0,
            header_size: core::mem::size_of::<Self>() as u64,
            header_align: core::mem::align_of::<Self>() as u64,
            info_size: 0,
            body_size,
            body_align,
            members: AtomicUsize::new(0),
            schema: AtomicU32::new(0x0001_0002),
            revision: 2,
            magic: 0xABCD,
            slots: [const { AtomicUsize::new(0) }; CAPACITY],
            tail: [],
        }
    }
}

/// The rule was supposed to be two lines: widest alignment first, then a tail that makes the size a whole
/// multiple of the alignment, so that no field ever arrives at an offset its alignment does not divide.
///
/// **It is wrong, and wrong on both targets, which is the finding this shape exists to report:**
///
/// ```text
/// error[E0080]: constructing invalid value of type [u8; 328]: at [70], encountered uninitialized memory …  // riscv32imac
/// error[E0080]: constructing invalid value of type [u8; 592]: at [74], encountered uninitialized memory …  // aarch64
/// ```
///
/// `TAIL = 0` is the mistake, and so is the placement that lets it be one. Descending alignment is not
/// enough once a field's alignment *varies by target*: `slots: [AtomicUsize; CAPACITY]` is 4-aligned on
/// riscv32 and 8-aligned on aarch64, and here it follows the halfword `magic`, which ends the running
/// offset at 70 or 74 — neither of which divides the array's alignment. A halfword before a variable-width
/// array therefore leaves a gap on every target, and a struct with a named tail cannot see it, because the
/// tail field is after everything and the gap is in the middle.
#[cfg(any())] // Disabled after the run that answered it; the two error texts above are its record.
const ORDERED_BYTES: [u8; core::mem::size_of::<Ordered>()] =
    const { unsafe { core::mem::transmute(Ordered::initial(0x0053_E480, 1 << 16, 4096)) } };

static ORDERED: Ordered = Ordered::initial(0x0053_E480, 1 << 16, 4096);

/// Shape four, part one: the words ordered so the only gap the compiler can build is the tail.
///
/// The fix for shape three is a placement rule rather than a padding rule: the variable-width atomics —
/// `members` and `slots` — travel with the 8-byte geometry block, whose running offset is a multiple of 8
/// and so of 4 too, and the fixed-width words and the halfword come after the array, where no wider field
/// follows them to demand realignment. Everything before `magic` then closes on a target-independent
/// multiple of whatever comes next.
///
/// This type is deliberately *not* the published shape: it ends at the halfword and so has tail padding it
/// cannot name. It exists so the size of that padding can be measured rather than guessed.
#[repr(C)]
pub struct Packed {
    region: u64,
    offset: u64,
    header_size: u64,
    header_align: u64,
    info_size: u64,
    body_size: u64,
    body_align: u64,
    members: AtomicUsize,
    slots: [AtomicUsize; CAPACITY],
    schema: AtomicU32,
    revision: u32,
    magic: u16,
}

/// Shape four, part two: the same bytes plus the tail, whose length is computed from `Packed`.
///
/// `size_of` already rounds `Packed` up to its alignment, and `magic` is the last field, so the difference
/// is exactly the padding no field owns — `2` on riscv32imac, `6` on aarch64. Writing those as a field is
/// what makes the image constructible; `TAIL = 0` in shape three is what made it not.
#[repr(C)]
pub struct Image {
    region: u64,
    offset: u64,
    header_size: u64,
    header_align: u64,
    info_size: u64,
    body_size: u64,
    body_align: u64,
    members: AtomicUsize,
    slots: [AtomicUsize; CAPACITY],
    schema: AtomicU32,
    revision: u32,
    magic: u16,
    /// Every byte the compiler padded, written by hand so that it belongs to something.
    tail: [u8; TAIL],
}

const TAIL: usize = core::mem::size_of::<Packed>() - core::mem::offset_of!(Packed, magic) - 2;

impl Image {
    pub const fn initial(region: u64, body_size: u64, body_align: u64) -> Self {
        Self {
            region,
            offset: 0,
            header_size: core::mem::size_of::<Self>() as u64,
            header_align: core::mem::align_of::<Self>() as u64,
            info_size: 0,
            body_size,
            body_align,
            members: AtomicUsize::new(0),
            slots: [const { AtomicUsize::new(0) }; CAPACITY],
            schema: AtomicU32::new(0x0001_0002),
            revision: 2,
            magic: 0xABCD,
            tail: [0; TAIL],
        }
    }
}

/// Shape four as bytes: the byte-image question, now with the gap placement fixed.
const IMAGE_BYTES: [u8; core::mem::size_of::<Image>()] =
    const { unsafe { core::mem::transmute(Image::initial(0x0053_E480, 1 << 16, 4096)) } };

static IMAGE_TYPED: Image = Image::initial(0x0053_E480, 1 << 16, 4096);

/// What a port would have to trust about the layout, stated so it holds on either target: that reordering
/// changed nothing about the size, that sealing changed nothing about the size, that the image is exactly as
/// long as the type, that the computed tail is exactly the padding, and that the magic halfword landed
/// little-endian at its field — at a computed offset, since the two targets disagree on which that is.
const _PACKED_SAME_SIZE: () =
    assert!(core::mem::size_of::<Packed>() == core::mem::size_of::<Header>());
const _SEALED_SAME_SIZE: () =
    assert!(core::mem::size_of::<Header>() == core::mem::size_of::<Sealed>());
const _IMAGE_COVERS: () = assert!(IMAGE_BYTES.len() == core::mem::size_of::<Image>());
const _TAIL_IS_PADDING: () =
    assert!(core::mem::size_of::<Image>() == core::mem::size_of::<Packed>() && TAIL > 0);
const _SAME_ALIGN: () = assert!(
    core::mem::align_of::<Header>() == core::mem::align_of::<Image>()
        && core::mem::align_of::<Image>() >= 8
);
const _IMAGE_GRAMMAR: () = assert!(
    IMAGE_BYTES[core::mem::offset_of!(Image, magic)] == 0xCD
        && IMAGE_BYTES[core::mem::offset_of!(Image, magic) + 1] == 0xAB
);

/// The address-shaped half of the seam: a `*mut u8` for `Map::from_static`, taken from a typed static.
fn base() -> *mut u8 {
    // `Map::from_static` takes `*mut u8` for words that are only ever written atomically, so a plain
    // `static` of atomics is the honest shape: no `static mut`, and casting the constness away is not
    // inventing mutability, because every write through it goes by a type that already is one.
    core::ptr::from_ref(&IMAGE).cast_mut().cast::<u8>()
}

/// The same, from the byte-image shape: the port needs no type from `evering` at all.
fn image_base() -> *mut u8 {
    core::ptr::from_ref(&IMAGE_TYPED).cast_mut().cast::<u8>()
}

/// …and from the published image itself, which is the shape that would need no reinitialisation at all.
fn seed_region(region: *mut u8) {
    // Copying `IMAGE_BYTES` over a mapped region is the whole of a const-constructible bootstrap, and the
    // reason the byte shape is worth the layout work: the port then writes no field, only bytes.
    unsafe { core::ptr::copy_nonoverlapping(IMAGE_BYTES.as_ptr(), region, IMAGE_BYTES.len()) };
}

/// The crate's record block, rebuilt field for field: byte arrays only, align 1, size 35, every multi-byte
/// field written with `to_le_bytes()`.
///
/// It is worth rebuilding because it is already the answer to two of the three questions the byte image
/// raises: it has no interior gap (nothing in it is wider-aligned than one byte) and it does not care about
/// endianness (each field is bytes, laid out little-endian by the writer). What the first four shapes
/// hand-waved, `evering` has designed away *inside the record*.
///
/// The rebuild is a copy, and `header::Record` is `pub(crate)` with no published length, so this is where a
/// check can go stale without failing: the assertion below catches a size or an alignment change, not a
/// reordering that leaves both alone.
#[repr(C)]
pub struct Record {
    status: AtomicU8,
    format: u8,
    profile: [u8; 3],
    magic: [u8; 2],
    revision: [u8; 4],
    schema: [u8; 8],
    extent: [u8; 8],
    extent_align: [u8; 8],
}

const _RECORD_AS_SHIPPED: () =
    assert!(core::mem::align_of::<Record>() == 1 && core::mem::size_of::<Record>() == 35);

/// Shape five: the header as `evering` orders it — record, the one atomic, the identity
/// words, the writer's info schema, then the layout body. Modelled with the same field classes: `RegionId` is
/// two `u64` (the public `evering::layout::RegionId`), `SchemaId` one, and the body is an array of
/// `AtomicUsize`. The header's own `*_size`/`*_align` words are gone from the crate, so they are gone here too.
///
/// No gap field appears here, because this type's job is to be the thing measured: the places the compiler
/// reserves padding are what a published byte image has to name.
#[repr(C)]
pub struct Shipped {
    record: Record,
    members: AtomicUsize,
    region: RegionId,
    offset: u64,
    info_schema: u64,
    info_revision: u32,
    inner: [AtomicUsize; CAPACITY],
}

/// The identity half of `RegionId`, modelled at its shipped width.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RegionId {
    high: u64,
    low: u64,
}

/// Shape five's first measurement: everything through `info_revision`, which is where the second gap is.
///
/// `SEAM` and `WORD_GAP` are read off types that keep the padding, so they are the compiler's numbers rather
/// than this file's arithmetic: after a 35-byte record the `AtomicUsize` wants 4 on riscv32 and 8 on aarch64,
/// and the run after it ends on a `u32` inside an 8-aligned struct.
#[repr(C)]
pub struct Recorded {
    record: Record,
    members: AtomicUsize,
    region: RegionId,
    offset: u64,
    info_schema: u64,
    info_revision: u32,
}

/// The one check that keeps shape five from ageing the way its `Record` copy was allowed to: the prefix is
/// measured against the crate rather than against this file. `HEADER_PREFIX_BYTES` is the shipped order with
/// both trailing fields zero-sized, so it is exactly `Recorded`'s length on every target the probe is
/// compiled at.
const _RECORDED_IS_THE_CRATE: () = assert!(
    core::mem::size_of::<Recorded>() == evering::layout::HEADER_PREFIX_BYTES
        && core::mem::align_of::<Recorded>() == evering::layout::HEADER_PREFIX_ALIGN
);

const SEAM: usize = core::mem::offset_of!(Shipped, members) - core::mem::size_of::<Record>();
const WORD_GAP: usize =
    core::mem::size_of::<Recorded>() - core::mem::offset_of!(Recorded, info_revision) - 4;

/// Shape five's second measurement: everything through the body, which is where the tail is.
#[repr(C)]
pub struct Assembled {
    record: Record,
    seam: [u8; SEAM],
    members: AtomicUsize,
    region: RegionId,
    offset: u64,
    info_schema: u64,
    info_revision: u32,
    word_gap: [u8; WORD_GAP],
    inner: [AtomicUsize; CAPACITY],
}

const ROOT_TAIL: usize = core::mem::size_of::<Assembled>()
    - core::mem::offset_of!(Assembled, inner)
    - core::mem::size_of::<[AtomicUsize; CAPACITY]>();

/// Shape five, the publishable one: the shipped order with every reserved byte made a field.
///
/// The claim this shape exists to test is the cheap one: naming the gaps should change **no offset and no
/// size**, because each of them is padding the compiler already put there. If `Shipped` and `Published`
/// agree in size and the byte image compiles, then publishing an initial root image costs a const
/// constructor and three padding fields — not a layout event.
///
/// The identity words come from the crate rather than from this file: `schema_id` and `AbiProfile::NATIVE`
/// are published, so an image built here carries the root's real schema and the target's real profile,
/// which is what makes the claim below — that a booting port needs no reinitialisation — checkable instead
/// of aspirational. The body length is the one part a port must supply, since `Root`'s capacity is not
/// published.
#[repr(C)]
pub struct Published {
    record: Record,
    /// The bytes between the record and the header's one atomic: 1 on riscv32imac, 5 on aarch64.
    seam: [u8; SEAM],
    members: AtomicUsize,
    region: RegionId,
    offset: u64,
    info_schema: u64,
    info_revision: u32,
    /// The bytes between the 4-byte revision word and whatever 8-aligned field follows it.
    word_gap: [u8; WORD_GAP],
    inner: [AtomicUsize; CAPACITY],
    /// The bytes the struct's own alignment rounds it up by, which on these two targets is none.
    tail: [u8; ROOT_TAIL],
}

impl Published {
    pub const fn initial(
        region_high: u64,
        region_low: u64,
        extent: u64,
        extent_align: u64,
    ) -> Self {
        let root = evering::layout::schema_id("evering.root");
        let unit = evering::layout::schema_id("evering.unit");
        let profile = evering::layout::AbiProfile::NATIVE;
        Self {
            record: Record {
                status: AtomicU8::new(2),
                format: 1,
                profile: [
                    profile.word_bytes,
                    profile.atomic_bytes,
                    profile.little_endian as u8,
                ],
                magic: 0xABCDu16.to_le_bytes(),
                revision: 2u32.to_le_bytes(),
                schema: root.0.to_le_bytes(),
                extent: extent.to_le_bytes(),
                extent_align: extent_align.to_le_bytes(),
            },
            seam: [0; SEAM],
            members: AtomicUsize::new(0),
            region: RegionId {
                high: region_high,
                low: region_low,
            },
            offset: 0,
            info_schema: unit.0,
            info_revision: 1,
            word_gap: [0; WORD_GAP],
            inner: [const { AtomicUsize::new(0) }; CAPACITY],
            tail: [0; ROOT_TAIL],
        }
    }
}

/// Shape five as bytes: does the crate's own header order admit a const image once its padding is named?
const PUBLISHED_BYTES: [u8; core::mem::size_of::<Published>()] =
    const { unsafe { core::mem::transmute(Published::initial(0, 0x0053_E480, 1 << 16, 4096)) } };

static PUBLISHED: Published = Published::initial(0, 0x0053_E480, 1 << 16, 4096);

/// The two facts that make shape five worth shipping: naming the gaps moved nothing, and the image is the
/// type — plus the record's own grammar, read at computed offsets so it holds on either target.
const _NAMING_CHANGED_NO_SIZE: () =
    assert!(core::mem::size_of::<Published>() == core::mem::size_of::<Shipped>());
const _GAPS_ARE_SMALL: () = assert!(SEAM < 8 && WORD_GAP < 8 && ROOT_TAIL < 8);
const _IMAGE_IS_THE_TYPE: () = assert!(PUBLISHED_BYTES.len() == core::mem::size_of::<Published>());
const _REIMAGE_GRAMMAR: () = assert!(
    PUBLISHED_BYTES[core::mem::offset_of!(Record, magic)] == 0xCD
        && PUBLISHED_BYTES[core::mem::offset_of!(Record, magic) + 1] == 0xAB
        && PUBLISHED_BYTES[core::mem::offset_of!(Record, schema)]
            == (evering::layout::schema_id("evering.root").0 as u8)
);
