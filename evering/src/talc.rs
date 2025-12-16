use core::alloc;
use core::cell::UnsafeCell;
use core::ops::Deref;
use core::{marker::PhantomData, ptr::NonNull};

use spin::Mutex;

use crate::mem::{AddrSpec, MapLayout, Mmap};
use crate::numeric::bit::{bit_check, bit_flip};
use crate::numeric::{
    AlignPtr, Alignable,
    bit::{WORD_ALIGN, WORD_BITS, WORD_SIZE, Word},
};
use crate::{header, mem};

type UInt = usize;
type Size = UInt;
type Offset = UInt;
type AddrSpan = crate::mem::AddrSpan<Offset>;

/// A relocatable pointer represented as an offset from a base pointer.
///
/// # Safety
///
/// - callers must ensure `base_ptr` is the same base used for creation.
#[derive(PartialEq, Eq, PartialOrd)]
#[repr(transparent)]
pub struct Rel<T: ?Sized> {
    pub offset: Offset,
    _marker: PhantomData<T>,
}
type RelPtr = Rel<u8>;

impl<T: ?Sized> core::fmt::Debug for Rel<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Rel<{}> {{ offset: {:?} }}",
            core::any::type_name::<T>(),
            self.offset
        )
    }
}

impl<T: ?Sized> Rel<T> {
    #[inline]
    const fn null() -> Self {
        Self {
            offset: Offset::MAX,
            _marker: PhantomData,
        }
    }

    #[inline]
    const fn is_null(&self) -> bool {
        self.offset == Offset::MAX
    }

    #[inline]
    const fn new(offset: Offset) -> Self {
        Self {
            offset,
            _marker: PhantomData,
        }
    }

    /// Construct Rel from raw pointer relative to `base_ptr`.
    ///
    /// # Safety
    ///
    /// - `ptr` must be within the same allocation as `base_ptr`.
    /// - `ptr >= base_ptr`.
    #[inline]
    const unsafe fn from_raw(ptr: *mut T, base_ptr: *const u8) -> Self {
        Self {
            offset: unsafe { ptr.byte_offset_from_unsigned(base_ptr.cast_mut()) },
            _marker: PhantomData,
        }
    }
}

impl<T> Rel<[T]> {
    #[inline]
    const unsafe fn as_raw(self, len: usize, base_ptr: *const u8) -> *mut [T] {
        core::ptr::slice_from_raw_parts_mut(
            base_ptr.wrapping_add(self.offset).cast::<T>().cast_mut(),
            len,
        )
    }

    #[inline]
    const unsafe fn as_ptr(self, len: usize, base_ptr: *const u8) -> NonNull<[T]> {
        unsafe {
            let ptr = self.as_raw(len, base_ptr);
            NonNull::new_unchecked(ptr)
        }
    }
}

impl<T> Rel<T> {
    /// Return a raw pointer using `base_ptr` by wrapping arithmetic.
    /// # Safety
    ///
    /// - `ptr` must be within the same allocation as `base_ptr`.
    #[inline]
    const unsafe fn as_raw(self, base_ptr: *const u8) -> *mut T {
        base_ptr.wrapping_add(self.offset).cast_mut().cast()
    }

    /// Return a `NonNull<T>` pointer using `base_ptr` by wrapping arithmetic.
    /// # Safety
    /// - `ptr` must be within the same allocation as `base_ptr`.
    #[inline]
    const unsafe fn as_ptr(self, base_ptr: *const u8) -> NonNull<T> {
        unsafe { NonNull::new_unchecked(self.as_raw(base_ptr)) }
    }
}

impl<T: ?Sized> Clone for Rel<T> {
    fn clone(&self) -> Self {
        Self {
            offset: self.offset,
            _marker: self._marker,
        }
    }
}

impl<T: ?Sized> Copy for Rel<T> {}

const _: () = {
    assert!(
        Tag::ALIGN == FreeNode::ALIGN
            && FreeNode::ALIGN == FreeTail::ALIGN
            && FreeTail::ALIGN == WORD_ALIGN,
        "Align of Tag/FreeNode/FreeTail must be same for consistency."
    );
    assert!(
        Tag::SIZE == FreeTail::SIZE,
        "Size of Tag/FreeTail must be same for consistency."
    )
};

#[derive(Clone, Copy)]
#[repr(transparent)]
struct Tag(Word);

impl core::fmt::Debug for Tag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tag")
            .field("is_allocated", &self.is_allocated())
            .field("is_above_free", &self.is_above_free())
            .field("relative base:", &self.to_base_rel())
            .finish()
    }
}

impl Tag {
    const SIZE: Size = core::mem::size_of::<Self>();
    const ALIGN: Offset = core::mem::align_of::<Self>();

    pub const ALLOCATED_FLAG: usize = 1 << 0;
    pub const IS_ABOVE_FREE_FLAG: usize = 1 << 1;
    pub const ALL_FLAG: usize = Self::IS_ABOVE_FREE_FLAG | Self::ALLOCATED_FLAG;
    pub const BASE_MASK: usize = !(Self::IS_ABOVE_FREE_FLAG | Self::ALLOCATED_FLAG);

    #[inline]
    const fn from_acme(acme: *mut u8) -> *mut Self {
        unsafe { acme.sub(Tag::SIZE).cast() }
    }

    unsafe fn from_alloc_base(ptr: *mut u8, size: Size, heap_base: *mut u8) -> *mut Self {
        unsafe {
            let post = ptr.add(size).align_up_of::<Word>();
            let post_rel = Rel::from_raw(post, heap_base);
            // Suppose it's a ptr to `Tag` or directly a `Tag`.
            let tag_or_tag_rel = post.cast::<RelPtr>().read();

            // The low bits of flags of tag doesn't affect the inequality.
            if tag_or_tag_rel > post_rel {
                // If it's a ptr to the real `Tag`
                let tag_ptr = tag_or_tag_rel.as_raw(heap_base);
                tag_ptr.cast()
            } else {
                // Else it's directly a `Tag`
                post.cast()
            }
        }
    }

    #[inline]
    fn chunk(tag: *mut Self, heap_base: *mut u8) -> Chunk {
        let base = Tag::to_base(tag, heap_base);
        let acme = Tag::to_acme(tag);
        unsafe { Chunk::from_endpoint(base, acme) }
    }

    /// Encode and write a Tag value to `tag_ptr`.
    unsafe fn init(tag: *mut Self, chunk_base: *mut u8, is_above_free: bool, heap_base: *mut u8) {
        // let base_value = chunk_base.addr();
        let rel_base = unsafe { Rel::from_raw(chunk_base, heap_base) };
        debug_assert!(
            rel_base.offset & Self::ALL_FLAG == 0,
            "Chunk base must be aligned."
        );

        let flags = if is_above_free {
            Self::ALL_FLAG
        } else {
            Self::ALLOCATED_FLAG
        };

        #[cfg(feature = "tracing")]
        tracing::debug!("[Talc]: tag init offset: {:#x}", rel_base.offset);

        unsafe { *tag = Self(rel_base.offset | flags) };
    }

    #[inline]
    const fn to_base_rel(self) -> RelPtr {
        RelPtr::new(self.0 & Self::BASE_MASK)
    }

    #[inline]
    const fn to_base(tag: *mut Self, heap_base: *mut u8) -> *mut u8 {
        unsafe { (*tag).to_base_rel().as_raw(heap_base) }
    }

    #[inline]
    const fn to_acme(tag: *mut Self) -> *mut u8 {
        unsafe { tag.byte_add(Self::SIZE).cast() }
    }

    #[inline]
    const fn is_above_free(self) -> bool {
        self.0 & Self::IS_ABOVE_FREE_FLAG != 0
    }

    #[inline]
    const fn is_allocated(self) -> bool {
        self.0 & Self::ALLOCATED_FLAG != 0
    }

    #[inline]
    const unsafe fn toggle_above_free(tag: *mut Self, should_free: bool) {
        let mut cur = unsafe { tag.read() };
        debug_assert!(cur.is_above_free() != should_free);
        if should_free {
            cur.0 |= Self::IS_ABOVE_FREE_FLAG
        } else {
            cur.0 &= !(Self::IS_ABOVE_FREE_FLAG)
        }
        debug_assert!(cur.is_above_free() == should_free);
        unsafe { tag.write(cur) }
    }

    #[inline]
    unsafe fn set_above_free(tag: *mut Self) {
        unsafe { Self::toggle_above_free(tag, true) };
    }

    pub unsafe fn clear_above_free(tag: *mut Self) {
        unsafe { Self::toggle_above_free(tag, false) };
    }
}

/// Intrusive doubly-linked list node for free chunks.
///
/// # Layout:
///  `[FreeListNode] [size: usize] ... [FreeTail(size)]`
#[derive(Debug)]
#[repr(C)]
pub struct FreeNode {
    /// The ptr to the next free node.
    pub next: Option<Rel<FreeNode>>,
    /// The ptr to the prev free node's `next` field.
    pub prev_next: Rel<Option<Rel<FreeNode>>>,
}

pub type FreeNodeLink = Option<Rel<FreeNode>>;

impl FreeNode {
    const LINK_SIZE: Size = core::mem::size_of::<FreeNodeLink>();
    const SIZE: Size = core::mem::size_of::<Self>();
    const ALIGN: Offset = core::mem::align_of::<Self>();

    #[inline]
    const unsafe fn from_base(base: *mut u8) -> *mut Self {
        base.cast()
    }

    /// Return pointer to the `next` field within the node.
    #[inline]
    const unsafe fn next(node: *mut Self) -> *mut FreeNodeLink {
        unsafe { &raw mut (*node).next }
    }

    #[inline]
    const unsafe fn next_rel(node: *mut Self, heap_base: *mut u8) -> Rel<FreeNodeLink> {
        unsafe {
            let next = Self::next(node);
            Rel::from_raw(next, heap_base)
        }
    }

    /// Return pointer to the `next_prev` field within the node.
    #[inline]
    const unsafe fn as_rel(node: *mut Self, heap_base: *mut u8) -> FreeNodeLink {
        unsafe { Some(Rel::from_raw(node, heap_base)) }
    }

    #[inline]
    unsafe fn insert(
        node: *mut Self,
        next: FreeNodeLink,
        prev_next: *mut FreeNodeLink,
        heap_base: *mut u8,
    ) {
        unsafe {
            debug_assert!(!node.is_null());
            debug_assert!(!prev_next.is_null());

            let prev_next_rel = Rel::from_raw(prev_next, heap_base);
            node.write(Self {
                next,
                prev_next: prev_next_rel,
            });
            *prev_next = FreeNode::as_rel(node, heap_base);
            debug_assert!((*prev_next).is_some());

            if let Some(next) = next {
                (*next.as_raw(heap_base)).prev_next = Self::next_rel(node, heap_base);
            }
        }
    }

    /// Assume `prev_next` point to `next`, resolve the `next` node and insert `this node`.
    #[inline]
    unsafe fn insert_by(node: *mut Self, prev_next: *mut FreeNodeLink, heap_base: *mut u8) {
        #[cfg(feature = "tracing")]
        tracing::debug!(
            "[Talc]: insert_by: prev_next: {:?}, node: {:?}",
            prev_next,
            node
        );
        unsafe {
            Self::insert(node, *prev_next, prev_next, heap_base);
        }
    }

    #[inline]
    const unsafe fn remove(node: *mut Self, heap_base: *mut u8) {
        unsafe {
            debug_assert!(!node.is_null());
            let Self { next, prev_next } = node.read();
            let prev_next_ptr = prev_next.as_raw(heap_base);
            debug_assert!(!prev_next_ptr.is_null());
            *prev_next_ptr = next;

            if let Some(next) = next {
                (*next.as_raw(heap_base)).prev_next = prev_next;
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct FreeNodeIter {
    cur: FreeNodeLink,
    heap_base: *mut u8,
}

impl FreeNodeIter {
    const fn new(cur: FreeNodeLink, heap_base: *mut u8) -> Self {
        Self { cur, heap_base }
    }
}

impl Iterator for FreeNodeIter {
    type Item = NonNull<FreeHead>;

    fn next(&mut self) -> Option<Self::Item> {
        let cur = unsafe { Rel::<FreeNode>::as_ptr(self.cur?, self.heap_base) };
        self.cur = unsafe { (*cur.as_ptr()).next };
        Some(cur.cast())
    }
}

/// Header structure at the base of a free chunk.
///
/// # Layout:
///  `[FreeListNode] [size: usize] ... [FreeTail(size)]`
///
///  Where `[FreeListNode] [size: usize] = [FreeHead]`
#[derive(Debug)]
#[repr(C)]
struct FreeHead {
    node: FreeNode,
    size_low: usize,
}

impl FreeHead {
    const NODE_OFFSET: usize = 0;
    const SIZE_LOW_OFFSET: usize = FreeNode::SIZE;

    /// Creates a pointer to the FreeHead struct from the raw chunk base pointer.
    #[inline]
    const unsafe fn from_base(base: *mut u8) -> *mut Self {
        base.cast()
    }

    /// Calculates the chunk base pointer from a NonNull pointer to the LlistNode.
    #[inline]
    const unsafe fn from_node(node: NonNull<FreeNode>) -> *mut Self {
        // The `FreeNode` is at offset 0 within `FreeHead`, and `FreeHead` is at the chunk base.
        // Therefore, the node pointer is the same as the FreeHead pointer (and the chunk base).
        node.as_ptr().cast()
    }

    #[inline]
    const fn node(head: *mut Self) -> *mut FreeNode {
        unsafe { &raw mut (*head).node }
    }

    #[inline]
    unsafe fn init(
        head: *mut Self,
        prev_next: *mut FreeNodeLink,
        size_low: Size,
        heap_base: *mut u8,
    ) {
        let node = Self::node(head);
        unsafe {
            FreeNode::insert_by(node, prev_next, heap_base);
            (*head).size_low = size_low
        }
    }

    #[inline]
    const unsafe fn deinit(head: *mut Self, heap_base: *mut u8) {
        let node = Self::node(head);
        unsafe {
            FreeNode::remove(node, heap_base);
        }
    }

    /// Gets the raw base pointer of the chunk from a pointer to the FreeHead.
    #[inline]
    const fn to_base(head: *mut Self) -> *mut u8 {
        head.cast()
    }

    /// Calculates the previous acme pointer (the address at the exclusive *end* of the chunk)
    #[inline]
    const fn to_prev_acme(head: *mut Self) -> *mut u8 {
        head.cast()
    }

    /// Calculates the previous tag (the address at the exclusive *end* of the chunk)
    #[inline]
    const fn to_prev_tag(head: *mut Self) -> *mut Tag {
        Tag::from_acme(Self::to_prev_acme(head))
    }

    /// Calculates the acme pointer (the address at the exclusive *end* of the chunk)
    /// by reading the `size_low` field.
    #[inline]
    const unsafe fn to_acme(head: *mut Self) -> *mut u8 {
        unsafe {
            let size = (*head).size_low;
            head.byte_add(size).cast()
        }
    }

    #[inline]
    const unsafe fn to_tail(head: *mut Self) -> *mut FreeTail {
        unsafe {
            let acme = FreeHead::to_acme(head);
            FreeTail::from_acme(acme)
        }
    }
}

/// Tail structure at the base of a free chunk.
///
/// # Layout:
///  `[FreeListNode] [size: usize] ... [FreeTail(size)]`
#[derive(Debug)]
#[repr(transparent)]
struct FreeTail {
    // Stores the size of the entire chunk
    size_high: usize,
}

impl FreeTail {
    const SIZE: usize = core::mem::size_of::<Self>();
    const ALIGN: usize = core::mem::align_of::<Self>();

    /// Creates a pointer to the `FreeTail` from the chunk acme ptr.
    #[inline]
    const unsafe fn from_acme(acme: *mut u8) -> *mut Self {
        unsafe { acme.sub(FreeTail::SIZE).cast() }
    }

    #[inline]
    const fn init(tail: *mut Self, size_high: Size) {
        unsafe { (*tail).size_high = size_high }
    }

    #[inline]
    const unsafe fn as_tag(tail: *mut Self) -> *mut Tag {
        tail.cast()
    }

    #[inline]
    const unsafe fn to_base(acme: *mut u8) -> *mut u8 {
        unsafe {
            let tail = FreeTail::from_acme(acme);
            let size = (*tail).size_high;
            tail.byte_sub(size).cast()
        }
    }

    /// Calculates the chunk base pointer from the chunk acme pointer
    /// by reading the `size_high` field.
    #[inline]
    unsafe fn to_head(tail: *mut Self) -> *mut FreeHead {
        unsafe {
            let size = (*tail).size_high;
            #[cfg(feature = "tracing")]
            tracing::debug!("[Talc]: to head, size: {}", size);
            tail.byte_sub(size - FreeTail::SIZE).cast()
        }
    }
}

/// `Chunk` is a memory range satisfy the chunk restriction of Talc.
#[derive(Debug, Clone, Copy)]
struct Chunk {
    pub base: *mut u8,
    pub acme: *mut u8,
}

impl Chunk {
    /// The minimal offset of a tag from the base ptr, which is the node size.
    const MIN_TAG_OFFSET: usize = FreeNode::SIZE;
    /// The minimal size of a chunk from the base ptr, which is the node size plus a tag size.
    const MIN_CHUNK_SIZE: usize = Self::MIN_TAG_OFFSET + Tag::SIZE;

    #[inline]
    const unsafe fn from_endpoint<T, U>(base: *mut T, acme: *mut U) -> Self {
        Chunk {
            base: base.cast(),
            acme: acme.cast(),
        }
    }

    #[inline]
    const fn from_head(base: NonNull<FreeHead>) -> Self {
        let acme = unsafe { FreeHead::to_acme(base.as_ptr()) };
        unsafe { Self::from_endpoint(base.as_ptr(), acme) }
    }

    #[inline]
    const unsafe fn head(&self) -> *mut FreeHead {
        unsafe { FreeHead::from_base(self.base) }
    }

    #[inline]
    const unsafe fn tail(&self) -> *mut FreeTail {
        unsafe { FreeTail::from_acme(self.acme) }
    }

    #[inline]
    const unsafe fn next_head(&self) -> *mut FreeHead {
        unsafe { FreeHead::from_base(self.acme) }
    }

    #[inline]
    const unsafe fn prev_tail(&self) -> *mut FreeTail {
        unsafe { FreeTail::from_acme(self.base) }
    }

    #[inline]
    const unsafe fn tag(&self) -> *mut Tag {
        Tag::from_acme(self.acme)
    }

    #[inline]
    const unsafe fn prev_tag(&self) -> *mut Tag {
        Tag::from_acme(self.base)
    }

    #[inline]
    fn size_by_range(&self) -> Size {
        self.acme.addr() - self.base.addr()
    }

    #[inline]
    unsafe fn size_by_head(&self) -> Size {
        let head = unsafe { self.head() };
        unsafe { (*head).size_low }
    }

    #[inline]
    /// Returns whether the range is greater than `MIN_CHUNK_SIZE`.
    fn is_valid(self) -> bool {
        Self::is_chunk(self.base, self.acme)
    }

    #[inline]
    /// Returns whether the range is greater than `MIN_CHUNK_SIZE`.
    fn is_chunk<T, U>(base: *mut T, acme: *mut U) -> bool {
        if !(acme >= base.cast()) {
            return false;
        }
        debug_assert!(acme >= base.cast(), "!(acme {:p} >= base {:p})", acme, base);
        Self::is_chunk_size(unsafe { acme.byte_offset_from_unsigned(base) })
    }

    #[inline]
    const fn is_chunk_size(size: usize) -> bool {
        size >= Chunk::MIN_CHUNK_SIZE
    }

    #[inline]
    const fn chunk_size(size: usize) -> usize {
        if size <= FreeNode::SIZE {
            Chunk::MIN_CHUNK_SIZE
        } else {
            (size + Tag::SIZE).align_up_of::<Word>()
        }
    }

    /// Split to a prefix chunk if possible and modify the base to the new prefix acme.
    #[inline]
    fn split_prefix(&mut self, alloc_base: *mut u8) -> Option<Self> {
        // Prefix Chunk should be prefix_acme <= alloc_base && [base, prefix_acme(new_base)] >= MIN_CHUNK_SIZE
        let prefix_acme = alloc_base.min(unsafe { self.acme.sub(Self::MIN_CHUNK_SIZE) });
        let prefix = Chunk {
            base: self.base,
            acme: prefix_acme,
        };
        if prefix.is_valid() {
            self.base = prefix_acme;
            Some(prefix)
        } else {
            None
        }
    }

    /// Split to a prefix chunk if possible and modify the acme to the new suffix base.
    #[inline]
    fn split_suffix(&mut self, alloc_acme: *mut u8) -> (Option<Self>, *mut Tag) {
        // Suffix Chunk should be suffix_base >= alloc_acme && [suffix_base(new_acme), acme] >= MIN_CHUNK_SIZE && [free_base, suffix_base(new_acme)] >= MIN_CHUNK_SIZE
        unsafe {
            // While we extract the new/old Tag pointer.
            let mut tag_ptr = self.base.add(Self::MIN_TAG_OFFSET).max(alloc_acme);
            let suffix_base = tag_ptr.add(Tag::SIZE);
            let suffix = Chunk {
                base: suffix_base,
                acme: self.acme,
            };
            tracing::debug!("[Talc]: split suffix {:?}", suffix);
            if suffix.is_valid() {
                self.acme = suffix_base;
                #[cfg(feature = "tracing")]
                tracing::debug!("[Talc]: split suffix success, tag_ptr {:?}", tag_ptr);
                (Some(suffix), tag_ptr.cast())
            } else {
                // Tag pointer doesn't change, resolve to original acme.
                tag_ptr = self.acme.sub(Tag::SIZE);
                #[cfg(feature = "tracing")]
                tracing::debug!("[Talc]: split suffix failed, tag_ptr {:?}", tag_ptr);
                (None, tag_ptr.cast())
            }
        }
    }
}

pub trait BinMarker: crate::seal::Sealed {}

impl<const LOG_DIVS: usize, const LINEAR_STAGES: usize, const BYTES_PER_LINEAR: usize>
    crate::seal::Sealed for BinConfig<LINEAR_STAGES, BYTES_PER_LINEAR, LOG_DIVS>
{
}
impl<const LOG_DIVS: usize, const LINEAR_STAGES: usize, const BYTES_PER_LINEAR: usize> BinMarker
    for BinConfig<LINEAR_STAGES, BYTES_PER_LINEAR, LOG_DIVS>
{
}

/// The static configuration for the allocator binning strategy.
///
/// # Parameters
/// * `LINEAR_STAGES`: How many linear stages to use before switching to exponential.
///    - e.g., `2` means (Word Stride) -> (Double Word Stride) -> Exponential.
/// * `BYTES_PER_LINEAR`: The number of bytes enforced per linear stage.
///    - This acts as the "restrictive gap value". If set to 16, each linear stage covers
///      `16 * Stride` bytes where `Stride = 2^(LINEAR_STAGES)`.
/// * `LOG_DIVS`: The log2 of the number of divisions per power-of-two(`2^n`) in the exponential range.
///    - e.g., `2` means 4 divisions. `3` means 8 divisions.
pub struct BinConfig<
    const LINEAR_STAGES: usize,
    const BYTES_PER_LINEAR: usize,
    const LOG_DIVS: usize,
>;

/// A simplified struct to hold pre-calculated stage data.
#[derive(Copy, Clone, Debug)]
pub struct LinearStage {
    pub size_limit: usize,       // The size limit (exclusive) for this stage
    pub start_bucket_idx: usize, // The bucket index offset where this stage starts
    pub stride: usize,           // The step size (gap) for this stage
}

impl<const LOG_DIVS: usize, const LINEAR_STAGES: usize, const BYTES_PER_LINEAR: usize>
    BinConfig<LINEAR_STAGES, BYTES_PER_LINEAR, LOG_DIVS>
{
    pub const BIN_COUNTS: Size = LINEAR_STAGES * WORD_BITS;
    pub const BIN_ARRAY_SIZE: Size = Self::BIN_COUNTS * FreeNode::LINK_SIZE;

    pub const MIN_CHUNK_SIZE: usize = Chunk::MIN_CHUNK_SIZE;

    pub const UNIT_GAP: Size = WORD_SIZE;
    // (BYTES_PER_LINEAR * stride)/(UNIT_GAP * stride)
    pub const BUCKET_IDX_PER_STAGES: usize = BYTES_PER_LINEAR / Self::UNIT_GAP;
    // 1. Derived Exponential Constants
    //
    pub const DIVS_PER_POW2: usize = 1 << LOG_DIVS;
    // The bitshift required to isolate the division bits.
    // We rely on LOG_DIVS directly, saving the .ilog2() calculation.
    pub const DIV_BITS: usize = LOG_DIVS;

    // 2. Derived Linear Stage Logic
    //
    // We pre-calculate the "End Limit" and "Base Bucket Index" for every linear stage.
    // This creates a compile-time lookup table.
    pub const STAGE_DATA: [LinearStage; LINEAR_STAGES] = Self::init_stages();

    /// The upper bound size where linear binning ends and exponential logic begins.
    /// Any size >= this value uses the exponential logic.
    pub const EXPONENTIAL_START_SIZE: Size = Self::STAGE_DATA[LINEAR_STAGES - 1].size_limit;

    /// The bucket index where the exponential section begins.
    pub const EXPONENTIAL_START_BUCKET: usize =
        Self::STAGE_DATA[LINEAR_STAGES - 1].start_bucket_idx + Self::BUCKET_IDX_PER_STAGES;

    /// The 'Magnitude' offset for the exponential logic.
    /// This effectively aligns the log2 calculation so that the first exponential
    /// bin starts seamlessly after the last linear bin.
    pub const MIN_EXP_BITS_LESS_ONE: usize = Self::EXPONENTIAL_START_SIZE.ilog2() as usize;

    /// Const function to generate the lookup table for linear stages.
    const fn init_stages() -> [LinearStage; LINEAR_STAGES] {
        let mut stages = [LinearStage {
            size_limit: 0,
            stride: 0,
            start_bucket_idx: 0,
        }; LINEAR_STAGES];

        let mut i = 0;
        let mut current_stride = 1;
        let mut current_limit = Self::MIN_CHUNK_SIZE;
        let mut current_bucket = 0;

        while i < LINEAR_STAGES {
            // The limit for this stage is the previous limit + (Slots * Stride)
            let next_limit = current_limit + (BYTES_PER_LINEAR * current_stride);

            stages[i] = LinearStage {
                size_limit: next_limit,
                start_bucket_idx: current_bucket,
                stride: current_stride,
            };

            // Prepare for next iteration
            current_limit = next_limit;
            current_bucket += Self::BUCKET_IDX_PER_STAGES;
            current_stride *= 2; // Stride doubles: 1 -> 2 -> 4 ...
            i += 1;
        }
        stages
    }

    /// Maps a size to a bin index using the provided compile-time configuration.
    #[inline]
    const fn bin_idx(size: usize) -> usize {
        debug_assert!(size >= Self::MIN_CHUNK_SIZE);

        // 1. Linear Stage Check
        // Loop unrolling is likely to happen here by the compiler because
        // LINEAR_STAGES is a small constant (e.g., 2 or 3).
        let mut i = 0;
        while i < LINEAR_STAGES {
            let stage = &Self::STAGE_DATA[i];

            // If size fits in this linear stage
            if size < stage.size_limit {
                // Logic:
                // We need to find how far `size` is from the START of this stage.
                // Start of stage 0 = MIN_CHUNK_SIZE
                // Start of stage N = Limit of stage N-1

                let stage_start = if i == 0 {
                    Self::MIN_CHUNK_SIZE
                } else {
                    Self::STAGE_DATA[i - 1].size_limit
                };

                // Formula: Offset / Stride + Base_Bucket_Index
                return (size - stage_start) / (stage.stride * Self::UNIT_GAP)
                    + stage.start_bucket_idx;
            }
            i += 1;
        }

        // 2. Exponential Stage (Pseudo-Logarithmic)
        // If we reach here, size >= Conf::EXPONENTIAL_START_SIZE

        // A. Find the coarse magnitude (Power of 2 range)
        // ilog2 returns floor(log2(size)).
        let bits_less_one = size.ilog2() as usize;

        // Normalizing the magnitude relative to where exponential bins start.
        let magnitude = bits_less_one - Self::MIN_EXP_BITS_LESS_ONE;

        // B. Find the fine division (Fractional part)
        // We want the 'LOG_DIVS' bits immediately following the MSB.
        // Shift right to bring those bits to the bottom.
        // Example: 1_XX_00... >> shift becomes 1XX
        let shift_amount = bits_less_one - Self::DIV_BITS;
        let shifted = size >> shift_amount;

        // Mask out the leading '1' to get just the index (0..DIVS-1).
        // effectively: shifted - (1 << LOG_DIVS)
        let division = shifted - Self::DIVS_PER_POW2;

        // C. Calculate Final Index
        // Index = (Magnitude * Divs_Per_Mag) + Division_Index + Start_Bucket
        let bucket_offset = (magnitude * Self::DIVS_PER_POW2) + division;

        (bucket_offset + Self::EXPONENTIAL_START_BUCKET).min(Self::BIN_COUNTS - 1)
    }
}

/// # Talc Allocator
#[repr(C)]
pub struct TalcMeta<const LS: usize, const BPL: usize, const LD: usize> {
    /// The bits array of available node.
    ///
    /// Each bits of a word suggests existence or not.
    avails: [Word; LS],
    /// The pointer to the array of nodes in bits array.
    bins: Rel<[FreeNodeLink]>,
    _config: PhantomData<BinConfig<LS, BPL, LD>>,
}

unsafe impl<const LS: usize, const BPL: usize, const LD: usize> Send for TalcMeta<LS, BPL, LD> {}

#[repr(C)]
pub struct TalckMeta<const LS: usize, const BPL: usize, const LD: usize> {
    lock: Mutex<()>,
    talc: UnsafeCell<TalcMeta<LS, BPL, LD>>,
}

unsafe impl<const LS: usize, const BPL: usize, const LD: usize> Sync for TalckMeta<LS, BPL, LD> {}

pub type Header<const LS: usize, const BPL: usize, const LD: usize> =
    header::Header<TalckMeta<LS, BPL, LD>>;
pub type MemTalck<const LS: usize, const BPL: usize, const LD: usize, S, M> =
    mem::MapHandle<Header<LS, BPL, LD>, S, M>;

/// For backpressure of range `[4b,...,4mb]`, suggesting a covering for `<= 14kb` fine granularity
///
/// - `4 B → 16 KiB`  ← very hot
/// - `32–64 KiB`    ← hot
/// - `256 KiB+`     ← cold but expensive.
// pub type Normal = BinConfig<3, 256, 2>;
pub type NHeader = Header<3, 256, 2>;
pub type NMemTalck<S, M> = mem::MapHandle<NHeader, S, M>;

pub struct Talc<
    H: const Deref<Target = Header<LS, BPL, LD>>,
    const LS: usize,
    const BPL: usize,
    const LD: usize,
> {
    header: H,
    bins: NonNull<[FreeNodeLink]>,
}
unsafe impl<
    H: const Deref<Target = Header<LS, BPL, LD>> + Send,
    const LS: usize,
    const BPL: usize,
    const LD: usize,
> Send for Talc<H, LS, BPL, LD>
{
}
unsafe impl<
    H: const Deref<Target = Header<LS, BPL, LD>> + Send,
    const LS: usize,
    const BPL: usize,
    const LD: usize,
> Sync for Talc<H, LS, BPL, LD>
{
}

pub type RefTalc<'a, const LS: usize, const BPL: usize, const LD: usize> =
    Talc<&'a Header<LS, BPL, LD>, LS, BPL, LD>;
pub type MapTalc<const LS: usize, const BPL: usize, const LD: usize, S, M> =
    Talc<MemTalck<LS, BPL, LD, S, M>, LS, BPL, LD>;

impl<const LS: usize, const BPL: usize, const LD: usize> TalcMeta<LS, BPL, LD> {
    const BIN_COUNTS: usize = BinConfig::<LS, BPL, LD>::BIN_COUNTS;
    const BIN_ARRAY_SIZE: Size = BinConfig::<LS, BPL, LD>::BIN_ARRAY_SIZE;
    const MIN_HEAP_SIZE: Size = Chunk::MIN_CHUNK_SIZE + Tag::SIZE;

    #[inline]
    const fn null() -> Self {
        TalcMeta {
            avails: [0usize; LS],
            bins: Rel::null(),
            _config: PhantomData,
        }
    }

    #[inline]
    const fn bins(&self) -> NonNull<[FreeNodeLink]> {
        unsafe { self.bins.as_ptr(Self::BIN_COUNTS, self.base_ptr()) }
    }

    #[inline]
    unsafe fn metadata(&mut self, ptr: *mut FreeNodeLink) -> (NonNull<[FreeNodeLink]>, *mut u8) {
        // let metadata_base = base.add(Tag::SIZE).cast::<FreeNodeLink>();
        unsafe {
            let mut i = 0;
            while i < Self::BIN_COUNTS {
                let bin = ptr.add(i);
                bin.write(None);
                i += 1;
            }
            let slice = core::ptr::slice_from_raw_parts_mut(ptr, Self::BIN_COUNTS);
            let metadata = Rel::<[FreeNodeLink]>::from_raw(slice, self.base_ptr());
            let acme = ptr.add(Self::BIN_COUNTS);
            self.bins = metadata;
            (NonNull::new_unchecked(slice), acme.cast())
        }
    }

    pub unsafe fn claim(
        &mut self,
        base: *mut u8,
        size: Size,
    ) -> Result<NonNull<[FreeNodeLink]>, ()> {
        let base = base.align_up_of::<Word>();
        let size = unsafe {
            (base.add(size))
                .align_down_of::<Word>()
                .byte_offset_from_unsigned(base)
        };
        #[cfg(feature = "tracing")]
        tracing::debug!("[Talc]: claim base: {:?}, size: {:?}", base, size);
        // if !self.bins.is_null() {
        //     if size <= Self::MIN_HEAP_SIZE {
        //         return Err(());
        //     }
        //     unsafe {
        //         // init anchor tag.
        //         let bins = self.bins.as_ptr(Self::BIN_COUNTS, self.base_ptr());
        //         Tag::init(base.cast(), base_ptr(), true, self.base_ptr());
        //         let chunk_base = base.wrapping_add(Tag::SIZE);
        //         self.insert_free(bins, FreeHead::from_base(chunk_base), size);
        //         self.scan_errors(bins);
        //         return Ok(bins);
        //     };
        // } else {
        unsafe {
            if size < Tag::SIZE + Self::BIN_ARRAY_SIZE + Tag::SIZE {
                return Err(());
            }
            Tag::init(base.cast(), base.cast(), true, self.base_ptr());
            #[cfg(feature = "tracing")]
            tracing::debug!("[Talc]: claim mock tag: {:?}", base);
            let metadata_base = base.byte_add(Tag::SIZE);
            let (bins, metadata_acme) = self.metadata(metadata_base.cast());
            let metadata_chunk_acme = metadata_acme.byte_add(Tag::SIZE);

            let free_size = size - (metadata_chunk_acme.offset_from_unsigned(base));
            if Chunk::is_chunk_size(free_size) {
                self.insert_free(bins, FreeHead::from_base(metadata_chunk_acme), free_size);
                Tag::init(metadata_acme.cast(), base, true, self.base_ptr());
                tracing::debug!(
                    "[Talc]: claim metadata end tag: {:?}, {:#b}",
                    metadata_acme,
                    metadata_acme.read()
                );
            } else {
                // the whole memory only hold a single chunk.
                let acme = base.byte_add(size);
                let tag_ptr = Tag::from_acme(acme);
                if tag_ptr != metadata_acme.cast() {
                    let tag_rel = Rel::<Tag>::from_raw(tag_ptr, self.base_ptr());
                    metadata_acme.cast::<Rel<Tag>>().write(tag_rel);
                }
                Tag::init(tag_ptr, base, false, self.base_ptr());
                tracing::debug!("[Talc]: claim metadata end tag in chunk: {:?}", base);
            }
            Ok(bins)
            // }
        }
    }

    #[inline]
    const fn base_ptr(&self) -> *mut u8 {
        (&raw const *self).cast_mut().cast()
    }

    #[inline]
    const fn word_bit_idx(idx: usize) -> (usize, usize) {
        (idx / WORD_BITS, idx % WORD_BITS)
    }

    #[inline]
    const unsafe fn toggle_avail(&mut self, idx: usize, should_be: bool) {
        debug_assert!(idx < Self::BIN_COUNTS);

        let (word_idx, bit_idx) = Self::word_bit_idx(idx);
        debug_assert!(word_idx < LS);

        let word = &mut self.avails[word_idx];
        debug_assert!(bit_check(*word, bit_idx) != should_be);
        bit_flip(word, bit_idx);
        debug_assert!(bit_check(*word, bit_idx) == should_be);
    }

    #[inline]
    const fn set_avail(&mut self, idx: usize) {
        unsafe { self.toggle_avail(idx, true) };
    }

    #[inline]
    const fn clear_avail(&mut self, idx: usize) {
        unsafe { self.toggle_avail(idx, false) };
    }

    #[inline(always)]
    const fn next_avail_bin_idx(&self, idx: usize) -> Option<usize> {
        let word_idx = idx / WORD_BITS;
        if word_idx >= LS {
            return None;
        }

        let bit_idx = idx % WORD_BITS;
        // shift to get the bits array of the given idx.
        let shift_avails = self.avails[word_idx] >> bit_idx;
        if shift_avails != 0 {
            // `trailing_zeros` gets the zero counts from the first non-zero bit 1.
            // thus we calculate the distance from the original idx to the first non-zero bit.
            return Some(idx + shift_avails.trailing_zeros() as usize);
        }

        // if the word of given idx is empty, found the next repeatly.
        let mut i = word_idx + 1;
        while i < LS {
            let word = self.avails[i];
            if word != 0 {
                return Some(i * WORD_BITS + word.trailing_zeros() as usize);
            }
            i += 1;
        }

        None
    }

    #[inline(always)]
    const fn bin_idx(size: usize) -> usize {
        BinConfig::<LS, BPL, LD>::bin_idx(size)
    }

    // Context resolution
    #[inline]
    const fn bin_by_idx(idx: usize, bins: NonNull<[FreeNodeLink]>) -> *mut FreeNodeLink {
        debug_assert!(idx < Self::BIN_COUNTS);
        unsafe { bins.as_mut_ptr().add(idx) }
    }

    #[inline]
    fn bin_by_size(size: usize, bins: NonNull<[FreeNodeLink]>) -> (*mut FreeNodeLink, usize) {
        let idx = Self::bin_idx(size);
        (Self::bin_by_idx(idx, bins), idx)
    }

    #[cfg(not(debug_assertions))]
    fn scan_errors(&self) {}

    #[cfg(debug_assertions)]
    fn scan_errors(&self, bins: NonNull<[FreeNodeLink]>) {
        // #[cfg(any(test, feature = "tracing"))]
        // let mut vec = std::vec::Vec::new();

        for idx in 0..Self::BIN_COUNTS {
            unsafe {
                let iter = FreeNodeIter::new(*Self::bin_by_idx(idx, bins), self.base_ptr());
                for head in iter {
                    let (word_idx, bit_idx) = Self::word_bit_idx(idx);
                    #[cfg(feature = "tracing")]
                    tracing::debug!(
                        "[Talc]: scan errors, word_idx {}, bit_idx {}",
                        word_idx,
                        bit_idx
                    );
                    assert!(bit_check(self.avails[word_idx], bit_idx));

                    let acme = FreeHead::to_acme(head.as_ptr());
                    let tail = FreeHead::to_tail(head.as_ptr());
                    let size_low = head.as_ref().size_low;
                    let size_high = (*tail).size_high;
                    let size_real = acme.byte_offset_from_unsigned(head.as_ptr());
                    assert!(size_low == size_high && size_high == size_real);

                    let prev_tag = Tag::from_acme(head.as_ptr().cast());
                    assert!((*prev_tag).is_above_free());
                    // a free chunk should already merged below free chunk.
                    // so any below chunk should be allocated.
                    assert!((*prev_tag).is_allocated());
                }
            }
        }
    }
}

impl<const LS: usize, const BPL: usize, const LD: usize> TalcMeta<LS, BPL, LD> {
    #[inline]
    fn insert_free(&mut self, bins: NonNull<[FreeNodeLink]>, head: *mut FreeHead, size: Size) {
        debug_assert!(Chunk::is_chunk_size(size));

        let (bin_ptr, bin_idx) = Self::bin_by_size(size, bins);
        #[cfg(feature = "tracing")]
        tracing::debug!("[Talc]: insert, head: {:?}", head);

        unsafe {
            if (*bin_ptr).is_none() {
                #[cfg(feature = "tracing")]
                tracing::debug!("[Talc]: insert, set avail: {}", bin_idx);
                self.set_avail(bin_idx);
            }
            FreeHead::init(head, bin_ptr, size, self.base_ptr());
            let tail = FreeHead::to_tail(head);
            FreeTail::init(tail, size);
        }
    }

    #[inline]
    unsafe fn remove_free(
        &mut self,
        bins: NonNull<[FreeNodeLink]>,
        head: *mut FreeHead,
        bin_idx: usize,
    ) {
        unsafe {
            let bin = Self::bin_by_idx(bin_idx, bins);
            debug_assert!((*bin).is_some());
            FreeHead::deinit(head, self.base_ptr());

            if (*bin).is_none() {
                #[cfg(feature = "tracing")]
                tracing::debug!("[Talc]: remove free clear avail idx: {}", bin_idx);
                self.clear_avail(bin_idx);
            }
        }
    }

    #[inline]
    unsafe fn remove_free_by_head(&mut self, bins: NonNull<[FreeNodeLink]>, head: *mut FreeHead) {
        unsafe {
            let bin_idx = Self::bin_idx((*head).size_low);
            self.remove_free(bins, head, bin_idx);
        }
    }

    #[inline]
    unsafe fn remove_free_by_tail(
        &mut self,
        bins: NonNull<[FreeNodeLink]>,
        tail: *mut FreeTail,
    ) -> *mut FreeHead {
        unsafe {
            let head = FreeTail::to_head(tail);
            self.remove_free_by_head(bins, head);
            head
        }
    }
    /// Acquire a free chunk by given `size` and `align`.
    ///
    /// - `chunk_size >= req_size = size + Tag::SIZE`
    /// - `chunk_base <= alloc_base <= alloc_base + req_size <= chunk_acme`
    #[inline]
    unsafe fn acquire_chunk(
        &mut self,
        bins: NonNull<[FreeNodeLink]>,
        size: Size,
        align: Offset,
    ) -> Option<(Chunk, *mut u8, *mut u8)> {
        let req_size = Chunk::chunk_size(size);
        let need_align = align > WORD_ALIGN;

        let mut bin_idx = self.next_avail_bin_idx(Self::bin_idx(req_size))?;
        #[cfg(feature = "tracing")]
        tracing::debug!("[Talc]: acquire chunk next avail idx: {}", bin_idx);
        loop {
            unsafe {
                let cur_rel = *Self::bin_by_idx(bin_idx, bins);
                let iter = FreeNodeIter::new(cur_rel, self.base_ptr());
                for head in iter {
                    let chunk_size = head.as_ref().size_low;
                    let base = FreeHead::to_base(head.as_ptr());
                    let acme = FreeHead::to_acme(head.as_ptr());
                    // no need align
                    if chunk_size >= req_size && !need_align {
                        let alloc_acme = base.add(size).align_up_of::<Word>();
                        self.remove_free(bins, head.as_ptr(), bin_idx);
                        return Some((Chunk::from_endpoint(base, acme), base, alloc_acme));
                    }
                    // need align
                    let alloc_base = base.align_up(align);
                    if alloc_base.add(req_size) <= acme {
                        let alloc_acme = alloc_base.add(size).align_up_of::<Word>();
                        #[cfg(feature = "tracing")]
                        tracing::debug!(
                            "[Talc]: acquire chunk, alloc_base: {:?}, alloc_acme: {:?}, chunk_size: {:?}, size: {:?}, head: {:?}",
                            alloc_base,
                            alloc_acme,
                            chunk_size,
                            size,
                            head
                        );
                        self.remove_free(bins, head.as_ptr(), bin_idx);
                        return Some((Chunk::from_endpoint(base, acme), alloc_base, alloc_acme));
                    }
                }
            }
            bin_idx = self.next_avail_bin_idx(bin_idx + 1)?;
        }
    }

    pub unsafe fn allocate(
        &mut self,
        bins: NonNull<[FreeNodeLink]>,
        layout: alloc::Layout,
    ) -> Result<NonNull<u8>, ()> {
        if layout.size() == 0 {
            return Ok(NonNull::dangling());
        }

        self.scan_errors(bins);
        unsafe {
            let (mut free, alloc_base, alloc_acme) = self
                .acquire_chunk(bins, layout.size(), layout.align())
                .ok_or(())?;

            if let Some(prefix) = free.split_prefix(alloc_base) {
                #[cfg(feature = "tracing")]
                tracing::debug!("[Talc]: insert prefix: {:?}", prefix);
                self.insert_free(bins, prefix.head(), prefix.size_by_range());
            } else {
                Tag::clear_above_free(free.prev_tag());
            }

            let (suffix, tag_ptr) = free.split_suffix(alloc_acme);
            if let Some(suffix) = suffix {
                #[cfg(feature = "tracing")]
                tracing::debug!("[Talc]: insert suffix: {:?}", suffix);
                self.insert_free(bins, suffix.head(), suffix.size_by_range());
                Tag::init(tag_ptr, free.base, true, self.base_ptr());
            } else {
                Tag::init(tag_ptr, free.base, false, self.base_ptr());
            }

            if tag_ptr.cast() != alloc_acme {
                let tag_rel = Rel::from_raw(tag_ptr, self.base_ptr());
                alloc_acme.cast::<Rel<Tag>>().write(tag_rel);
            }

            Ok(NonNull::new_unchecked(alloc_base))
        }
    }

    /// Free previously allocated/reallocated memory.
    ///
    /// # Safety
    /// `ptr` must have been previously allocated given `layout`.
    pub unsafe fn deallocate(
        &mut self,
        bins: NonNull<[FreeNodeLink]>,
        ptr: NonNull<u8>,
        size: Size,
    ) {
        if size == 0 {
            return;
        }

        // #[cfg(feature = "counters")]
        // self.counters.account_dealloc(layout.size());

        self.scan_errors(bins);
        unsafe {
            let tag = Tag::from_alloc_base(ptr.as_ptr(), size, self.base_ptr());
            let mut chunk = Tag::chunk(tag, self.base_ptr());
            #[cfg(feature = "tracing")]
            tracing::debug!("[Talc]: deallocate tag: {:?}, chunk: {:?}", tag, chunk);

            debug_assert!((*tag).is_allocated());
            debug_assert!(Chunk::is_valid(chunk));

            let prev_tag = chunk.prev_tag();
            #[cfg(feature = "tracing")]
            tracing::debug!(
                "[Talc]: deallocate prev tag: {:?}, read: {:#b}",
                prev_tag,
                prev_tag.cast::<usize>().read()
            );
            // try recombine below if below is free.
            if !(*prev_tag).is_allocated() {
                let prev_tail = chunk.prev_tail();
                let prev_head = self.remove_free_by_tail(bins, prev_tail);

                chunk.base = prev_head.cast();
            } else {
                Tag::set_above_free(prev_tag);
            }

            // try recombine above.
            if (*tag).is_above_free() {
                let next_head = chunk.next_head();
                let next_size = (*next_head).size_low;
                self.remove_free_by_head(bins, next_head);

                chunk.acme = chunk.acme.byte_add(next_size);
            }

            // free the recombined chunk back.
            self.insert_free(bins, chunk.head(), chunk.size_by_range());
        }
    }
}

impl<const LS: usize, const BPL: usize, const LD: usize> TalckMeta<LS, BPL, LD> {
    const SIZE: Size = core::mem::size_of::<Self>();
    const ALIGN: Offset = core::mem::align_of::<Self>();
    const fn null() -> Self {
        Self {
            lock: spin::Mutex::new(()),
            talc: UnsafeCell::new(TalcMeta::<LS, BPL, LD>::null()),
        }
    }
}

impl<
    H: const Deref<Target = Header<LS, BPL, LD>>,
    const LS: usize,
    const BPL: usize,
    const LD: usize,
> Talc<H, LS, BPL, LD>
{
    const BIN_COUNTS: usize = BinConfig::<LS, BPL, LD>::BIN_COUNTS;
    const BIN_ARRAY_SIZE: Size = BinConfig::<LS, BPL, LD>::BIN_ARRAY_SIZE;
    const MIN_HEAP_SIZE: Size = Chunk::MIN_CHUNK_SIZE + Tag::SIZE;

    pub fn allocate(&self, layout: alloc::Layout) -> Result<Meta, ()> {
        let _lock = self.header.lock.lock();
        unsafe {
            let base_ptr = self.header.talc.as_ref_unchecked().base_ptr();
            self.header
                .talc
                .as_mut_unchecked()
                .allocate(self.bins, layout)
                .map(|ptr| {
                    let size = layout.size();
                    Meta::from_ptr(base_ptr, ptr.as_ptr(), size)
                })
        }
    }

    pub fn deallocate(&self, meta: Meta) {
        let _lock = self.header.lock.lock();
        if meta.is_null() {
            return;
        }
        unsafe {
            self.header.talc.as_mut_unchecked().deallocate(
                self.bins,
                meta.as_nonnull(),
                meta.view.size,
            );
        }
    }
}

unsafe impl<
    H: const Deref<Target = Header<LS, BPL, LD>>,
    const LS: usize,
    const BPL: usize,
    const LD: usize,
> mem::MemAlloc for Talc<H, LS, BPL, LD>
{
    type Meta = Meta;

    type Error = ();

    #[inline]
    fn base_ptr(&self) -> *const u8 {
        unsafe { self.header.talc.as_ref_unchecked().base_ptr() }
    }

    #[inline]
    fn malloc_by(&self, layout: std::alloc::Layout) -> Result<Self::Meta, Self::Error> {
        self.allocate(layout)
    }
}

unsafe impl<
    H: const Deref<Target = Header<LS, BPL, LD>>,
    const LS: usize,
    const BPL: usize,
    const LD: usize,
> mem::MemDealloc for Talc<H, LS, BPL, LD>
{
    fn demalloc(&self, meta: Self::Meta) -> bool {
        self.deallocate(meta);
        true
    }
}

impl<
    H: const Deref<Target = Header<LS, BPL, LD>>,
    const LS: usize,
    const BPL: usize,
    const LD: usize,
> mem::MemAllocator for Talc<H, LS, BPL, LD>
{
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Meta {
    base_ptr: *const u8,
    view: AddrSpan,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct SpanMeta {
    view: AddrSpan,
}

unsafe impl Send for Meta {}

unsafe impl const mem::Meta for Meta {
    type SpanMeta = SpanMeta;

    #[inline]
    fn null() -> Self {
        Self {
            base_ptr: core::ptr::null(),
            view: AddrSpan::null(),
        }
    }

    #[inline]
    fn is_null(&self) -> bool {
        self.view.is_null()
    }

    #[inline]
    fn as_uninit<T>(&self) -> NonNull<std::mem::MaybeUninit<T>> {
        if self.is_null() {
            return NonNull::dangling();
        }
        let ptr = unsafe { self.view.as_ptr(self.base_ptr) };
        // memory allocated while it may be uninitiated.
        unsafe { NonNull::new_unchecked(ptr.cast_mut().cast()) }
    }

    #[inline]
    fn erase(self) -> Self::SpanMeta {
        SpanMeta { view: self.view }
    }

    #[inline]
    unsafe fn recall(span: Self::SpanMeta, base_ptr: *const u8) -> Self {
        Meta {
            base_ptr,
            view: span.view,
        }
    }
}

unsafe impl const mem::Span for SpanMeta {
    #[inline]
    fn null() -> Self {
        Self {
            view: AddrSpan::null(),
        }
    }

    #[inline]
    fn is_null(&self) -> bool {
        self.view.is_null()
    }
}

impl Meta {
    #[inline]
    const fn null() -> Self {
        Self {
            base_ptr: core::ptr::null(),
            view: AddrSpan::null(),
        }
    }
    #[inline]
    const fn is_null(&self) -> bool {
        self.view.is_null()
    }
    #[inline]
    fn from_raw(base_ptr: *const u8, offset: Offset, size: Size) -> Self {
        Self {
            base_ptr,
            view: AddrSpan::new(offset, size),
        }
    }
    #[inline]
    unsafe fn from_ptr(base_ptr: *const u8, ptr: *const u8, size: Size) -> Self {
        let offset = unsafe { ptr.byte_offset_from_unsigned(base_ptr) };
        Self {
            base_ptr,
            view: AddrSpan::new(offset, size),
        }
    }
    #[inline]
    const unsafe fn as_nonnull(&self) -> NonNull<u8> {
        if self.is_null() {
            return NonNull::dangling();
        }
        unsafe { self.view.as_nonnull(self.base_ptr) }
    }
}

impl<const LS: usize, const BPL: usize, const LD: usize> header::Layout for TalckMeta<LS, BPL, LD> {
    type Config = (Offset, Size);

    const MAGIC: header::Magic = 0x1234;

    fn init(&mut self, conf: Self::Config) -> header::Status {
        let ptr = &raw mut *self;
        unsafe { ptr.write(Self::null()) };
        let inits_ptr = unsafe {
            ptr.add(Self::SIZE + conf.0)
                .cast::<u8>()
                .align_up_of::<Self>()
        };
        unsafe {
            let _ = self.talc.as_mut_unchecked().claim(inits_ptr.cast(), conf.1);
        }
        header::Status::Initialized
    }

    fn attach(&self) -> header::Status {
        header::Status::Initialized
    }
}

impl<
    H: const Deref<Target = Header<LS, BPL, LD>>,
    const LS: usize,
    const BPL: usize,
    const LD: usize,
> Talc<H, LS, BPL, LD>
{
    #[inline]
    pub const fn header(&self) -> &Header<LS, BPL, LD> {
        &self.header
    }

    #[inline]
    pub const fn new(header: H, bins: NonNull<[FreeNodeLink]>) -> Self {
        Self { header, bins }
    }
}

impl<S: AddrSpec, M: Mmap<S>, const LS: usize, const BPL: usize, const LD: usize>
    MapTalc<LS, BPL, LD, S, M>
{
    #[inline]
    pub fn from_layout(
        area: MapLayout<S, M>,
        mut conf: (Offset, Size),
    ) -> Result<Self, mem::Error<S, M>> {
        use mem::MemOps;
        let mut area = area;

        let reserve = area.reserve::<Header<LS, BPL, LD>>()?;
        let size = area.size() - reserve.next();
        conf.1 = size;
        #[cfg(feature = "tracing")]
        tracing::debug!("[Talc]: with conf: {:?}", conf);
        let handle = area.commit(reserve, conf)?;
        let bins = unsafe { handle.talc.as_ref_unchecked().bins() };

        Ok(Self::new(handle, bins))
    }
}

impl<S: AddrSpec, M: Mmap<S>, const LS: usize, const BPL: usize, const LD: usize>
    TryFrom<MapLayout<S, M>> for MapTalc<LS, BPL, LD, S, M>
{
    type Error = mem::Error<S, M>;

    fn try_from(value: MapLayout<S, M>) -> Result<Self, Self::Error> {
        Self::from_layout(value, (0, 0))
    }
}
