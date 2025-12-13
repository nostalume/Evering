use core::alloc;
use core::{marker::PhantomData, ptr::NonNull};

use crate::numeric::bit::{bit_check, bit_flip};
use crate::numeric::{
    AlignPtr, Alignable,
    bit::{WORD_ALIGN, WORD_BITS, WORD_SIZE, Word},
};

type Size = usize;
type Offset = usize;

/// A relocatable pointer represented as an offset from a base pointer.
///
/// # Safety
///
/// - callers must ensure `base_ptr` is the same base used for creation.
#[derive(Debug, PartialEq, Eq, PartialOrd)]
#[repr(transparent)]
struct Rel<T> {
    pub offset: Offset,
    _marker: PhantomData<T>,
}
type RelPtr = Rel<u8>;

impl<T> Rel<T> {
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
    const unsafe fn from_raw(ptr: *mut T, base_ptr: *const u8) -> Rel<T> {
        Rel {
            offset: unsafe { ptr.byte_offset_from_unsigned(base_ptr.cast_mut()) },
            _marker: PhantomData,
        }
    }

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

impl<T> Clone for Rel<T> {
    fn clone(&self) -> Self {
        Self {
            offset: self.offset,
            _marker: self._marker,
        }
    }
}

impl<T> Copy for Rel<T> {}

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

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
struct Tag(Word);

impl Tag {
    const SIZE: usize = core::mem::size_of::<Self>();
    const ALIGN: usize = core::mem::align_of::<Self>();

    pub const ALLOCATED_FLAG: usize = 1 << 0;
    pub const IS_ABOVE_FREE_FLAG: usize = 1 << 1;
    pub const ALL_FLAG: usize = Self::IS_ABOVE_FREE_FLAG | Self::ALLOCATED_FLAG;
    pub const BASE_MASK: usize = !(Self::IS_ABOVE_FREE_FLAG | Self::ALLOCATED_FLAG);

    #[inline]
    const fn from_acme(acme: *mut u8) -> *mut Self {
        unsafe { acme.sub(Tag::SIZE).cast() }
    }

    unsafe fn from_alloc_base(ptr: *mut u8, size: usize, heap_base: *mut u8) -> *mut Self {
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

    /// Encode and write a Tag value to `tag_ptr`.
    const unsafe fn init(
        tag: *mut Self,
        chunk_base: *mut u8,
        is_above_free: bool,
        heap_base: *mut u8,
    ) {
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

        unsafe { *tag = Self(rel_base.offset | flags) };
    }

    #[inline]
    const fn chunk_base(self, heap_base: *mut u8) -> *mut u8 {
        unsafe { RelPtr::new(self.0 & Self::BASE_MASK).as_raw(heap_base) }
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
        debug_assert!(!cur.is_above_free() == should_free);
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
struct FreeNode {
    /// The ptr to the next free node.
    pub next: Option<Rel<FreeNode>>,
    /// The ptr to the prev free node's `next` field.
    pub prev_next: Rel<Option<Rel<FreeNode>>>,
}

type FreeNodeLink = Option<Rel<FreeNode>>;

impl FreeNode {
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
    const unsafe fn insert(
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
    const unsafe fn insert_by(node: *mut Self, prev_next: *mut FreeNodeLink, heap_base: *mut u8) {
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
        Self {
            cur,
            heap_base: heap_base,
        }
    }
}

impl Iterator for FreeNodeIter {
    type Item = NonNull<FreeNode>;

    fn next(&mut self) -> Option<Self::Item> {
        let cur = unsafe { Rel::as_ptr(self.cur?, self.heap_base) };
        self.cur = unsafe { (*cur.as_ptr()).next };
        Some(cur)
    }
}

/// Header structure at the base of a free chunk.
///
/// # Layout:
///  `[FreeListNode] [size: usize] ... [FreeTail(size)]`
///
///  Where `[FreeListNode] [size: usize] = [FreeHead]`
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
    const unsafe fn init(
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

    // #[inline]
    // unsafe fn init(head: *mut Self, size: usize) {
    //     unsafe { (*head).size_low = size }
    // }

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
    const unsafe fn to_head(tail: *mut Self) -> *mut FreeHead {
        unsafe {
            let size = (*tail).size_high;
            tail.byte_sub(size - FreeTail::SIZE).cast()
        }
    }
}

pub trait BinMarker: crate::seal::Sealed {}

impl<const LOG_DIVS: u32, const LINEAR_STAGES: usize, const BYTES_PER_LINEAR: usize>
    crate::seal::Sealed for BinConfig<LINEAR_STAGES, BYTES_PER_LINEAR, LOG_DIVS>
{
}
impl<const LOG_DIVS: u32, const LINEAR_STAGES: usize, const BYTES_PER_LINEAR: usize> BinMarker
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
pub struct BinConfig<const LINEAR_STAGES: usize, const BYTES_PER_LINEAR: usize, const LOG_DIVS: u32>;
pub type Normal = BinConfig<2, 256, 2>;

/// A simplified struct to hold pre-calculated stage data.
#[derive(Copy, Clone, Debug)]
pub struct LinearStage {
    pub limit: usize,       // The size limit (exclusive) for this stage
    pub stride: usize,      // The step size (gap) for this stage
    pub base_bucket: usize, // The bucket index offset where this stage starts
}

impl<const LOG_DIVS: u32, const LINEAR_STAGES: usize, const BYTES_PER_LINEAR: usize>
    BinConfig<LINEAR_STAGES, BYTES_PER_LINEAR, LOG_DIVS>
{
    pub const MIN_CHUNK_SIZE: usize = Talc::<Self, LINEAR_STAGES>::MIN_CHUNK_SIZE;
    // 1. Derived Exponential Constants
    //
    pub const DIVS_PER_POW2: usize = 1 << LOG_DIVS;
    // The bitshift required to isolate the division bits.
    // We rely on LOG_DIVS directly, saving the .ilog2() calculation.
    pub const DIV_BITS: usize = LOG_DIVS as usize;

    // 2. Derived Linear Stage Logic
    //
    // We pre-calculate the "End Limit" and "Base Bucket Index" for every linear stage.
    // This creates a compile-time lookup table.
    pub const STAGE_DATA: [LinearStage; LINEAR_STAGES] = Self::calculate_stages();

    /// The upper bound size where linear binning ends and exponential logic begins.
    /// Any size >= this value uses the exponential logic.
    pub const EXPONENTIAL_START_SIZE: usize = Self::STAGE_DATA[LINEAR_STAGES - 1].limit;

    /// The bucket index where the exponential section begins.
    pub const EXPONENTIAL_START_BUCKET: usize =
        Self::STAGE_DATA[LINEAR_STAGES - 1].base_bucket + BYTES_PER_LINEAR;

    /// The 'Magnitude' offset for the exponential logic.
    /// This effectively aligns the log2 calculation so that the first exponential
    /// bin starts seamlessly after the last linear bin.
    pub const MIN_EXP_BITS_LESS_ONE: usize = Self::EXPONENTIAL_START_SIZE.ilog2() as usize;

    /// Const function to generate the lookup table for linear stages.
    const fn calculate_stages() -> [LinearStage; LINEAR_STAGES] {
        let mut stages = [LinearStage {
            limit: 0,
            stride: 0,
            base_bucket: 0,
        }; LINEAR_STAGES];

        let mut i = 0;
        let mut current_stride = 1;
        let mut current_limit = Self::MIN_CHUNK_SIZE;
        let mut current_bucket = 0;

        while i < LINEAR_STAGES {
            // The limit for this stage is the previous limit + (Slots * Stride)
            let next_limit = current_limit + (BYTES_PER_LINEAR * current_stride);

            stages[i] = LinearStage {
                limit: next_limit,
                stride: current_stride,
                base_bucket: current_bucket,
            };

            // Prepare for next iteration
            current_limit = next_limit;
            current_bucket += BYTES_PER_LINEAR;
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
            if size < stage.limit {
                // Logic:
                // We need to find how far `size` is from the START of this stage.
                // Start of stage 0 = MIN_CHUNK_SIZE
                // Start of stage N = Limit of stage N-1

                let stage_start = if i == 0 {
                    Self::MIN_CHUNK_SIZE
                } else {
                    Self::STAGE_DATA[i - 1].limit
                };

                // Formula: Offset / Stride + Base_Bucket_Index
                return (size - stage_start) / (stage.stride * WORD_SIZE) + stage.base_bucket;
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

        bucket_offset + Self::EXPONENTIAL_START_BUCKET
    }
}

pub struct Talc<C: BinMarker, const N: usize> {
    avails: [usize; N],
    bins: NonNull<[FreeNodeLink]>,
    _config: PhantomData<C>,
}

impl<C: BinMarker, const N: usize> Talc<C, N> {
    /// The minimal offset of a tag from the base ptr, which is the node size.
    const MIN_TAG_OFFSET: usize = FreeNode::SIZE;
    /// The minimal size of a chunk from the base ptr, which is the node size plus a tag size.
    const MIN_CHUNK_SIZE: usize = Self::MIN_TAG_OFFSET + Tag::SIZE;

    pub const BIN_COUNTS: usize = N * WORD_BITS;

    const fn new(bins: NonNull<[FreeNodeLink]>) -> Self {
        Talc {
            avails: [0usize; N],
            bins,
            _config: PhantomData,
        }
    }

    /// Returns whether the range is greater than `MIN_CHUNK_SIZE`.
    fn is_chunk_size<T, U>(base: *mut T, acme: *mut U) -> bool {
        debug_assert!(acme >= base.cast(), "!(acme {:p} >= base {:p})", acme, base);
        unsafe { acme.byte_offset_from_unsigned(base) >= Self::MIN_CHUNK_SIZE }
    }

    #[inline]
    const fn to_chunk_size(size: usize) -> usize {
        if size <= FreeNode::SIZE {
            Self::MIN_CHUNK_SIZE
        } else {
            (size + Tag::SIZE).align_up_of::<Word>()
        }
    }

    #[inline]
    const fn base_ptr(&self) -> *mut u8 {
        (&raw const *self).cast_mut().cast()
    }

    #[inline]
    const unsafe fn toggle_avail(&mut self, idx: usize, should_be: bool) {
        debug_assert!(idx < Self::BIN_COUNTS);

        let word_idx = idx / WORD_BITS;
        let bit_idx = idx % WORD_BITS;
        debug_assert!(word_idx < N);

        let ptr = self.avails.as_mut_slice();
        let word = &mut ptr[word_idx];
        debug_assert!(!bit_check(*word, bit_idx) == should_be);
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
    const fn next_avail_bin(&self, bin_idx: usize) -> Option<usize> {
        let word_idx = bin_idx / WORD_BITS;
        if word_idx >= N {
            return None;
        }

        let bit_idx = bin_idx % WORD_BITS;
        // shift to get the bits of the bin.
        let shift_avails = self.avails[word_idx] >> bit_idx;
        if shift_avails != 0 {
            return Some(bin_idx + shift_avails.trailing_zeros() as usize);
        }

        let mut i = word_idx + 1;
        while i < N {
            let word = self.avails[i];
            if word != 0 {
                return Some(i * WORD_BITS + word.trailing_zeros() as usize);
            }
            i += 1;
        }

        None
    }
}

impl<const LS: usize, const SPL: usize, const LD: u32> Talc<BinConfig<LS, SPL, LD>, LS> {
    #[inline(always)]
    const fn bin_idx(size: usize) -> usize {
        BinConfig::<LS, SPL, LD>::bin_idx(size)
    }

    #[inline]
    const fn bin_by_idx(&self, idx: usize) -> *mut FreeNodeLink {
        unsafe { self.bins.as_mut_ptr().add(idx) }
    }

    #[inline]
    const fn bin_by_size(&self, size: usize) -> *mut FreeNodeLink {
        self.bin_by_idx(Self::bin_idx(size))
    }

    #[inline]
    fn insert_free(&mut self, head: *mut FreeHead, acme: *mut u8) {
        debug_assert!(Self::is_chunk_size(head, acme));

        let size = acme.addr() - head.addr();
        let bin_idx = BinConfig::<LS, SPL, LD>::bin_idx(size);
        let bin_ptr = self.bin_by_idx(bin_idx);

        unsafe {
            if (*bin_ptr).is_none() {
                self.set_avail(bin_idx);
            }
            FreeHead::init(head, bin_ptr, size, self.base_ptr());
            let tail = FreeTail::from_acme(acme);
            FreeTail::init(tail, size);
        }
    }

    #[inline]
    const unsafe fn remove_free_by(&mut self, head: *mut FreeHead, bin_idx: usize) {
        unsafe {
            let bin = *self.bin_by_idx(bin_idx);
            debug_assert!(bin.is_some());
            FreeHead::deinit(head, self.base_ptr());

            if bin.is_none() {
                self.clear_avail(bin_idx);
            }
        }
    }

    #[inline]
    const unsafe fn remove_free(&mut self, head: *mut FreeHead) {
        unsafe {
            let bin_idx = Self::bin_idx((*head).size_low);
            self.remove_free_by(head, bin_idx);
        }
    }

    #[inline]
    unsafe fn acquire_chunk(
        &mut self,
        layout: alloc::Layout,
    ) -> Option<(*mut FreeHead, *mut u8, *mut u8)> {
        let req_size = Self::to_chunk_size(layout.size());
        let need_align = layout.align() > WORD_ALIGN;

        let mut bin = self.next_avail_bin(Self::bin_idx(req_size))?;
        loop {
            unsafe {
                let cur_rel = *self.bin_by_idx(bin);
                let iter = FreeNodeIter::new(cur_rel, self.base_ptr());
                for node in iter {
                    let head = FreeHead::from_node(node);
                    let size = (*head).size_low;
                    if size >= req_size {
                        if !need_align {
                            self.remove_free_by(head, bin);
                            return Some((head, FreeHead::to_acme(head), FreeHead::to_base(head)));
                        }
                    } else {
                        let base = FreeHead::to_base(head);
                        let acme = FreeHead::to_acme(head);
                        let align_base = base.align_up(layout.align());
                        if align_base.add(req_size) <= acme {
                            self.remove_free_by(head, bin);
                            return Some((head, acme, align_base));
                        }
                    }
                }
            }
            bin = self.next_avail_bin(bin + 1)?;
        }
    }

    pub unsafe fn alloc(&mut self, layout: alloc::Layout) -> Result<NonNull<u8>, ()> {
        debug_assert!(layout.size() != 0);

        unsafe {
            let (mut free_base, free_acme, alloc_base) = loop {
                match self.acquire_chunk(layout) {
                    Some(res) => break res,
                    None => return Err(()),
                }
            };

            let chunk_base_ceil = alloc_base.min(free_acme.sub(Self::MIN_CHUNK_SIZE));
            if Self::is_chunk_size(free_base, chunk_base_ceil) {
                self.insert_free(free_base, chunk_base_ceil);
                free_base = chunk_base_ceil;
            } else {
                let tag = Tag::from_acme(free_base);
                Tag::clear_above_free(tag);
            }

            let post_alloc_ptr = alloc_base.add(layout.size()).align_up_of::<Word>();
            let mut tag_ptr = free_base.add(Self::MIN_TAG_OFFSET).max(post_alloc_ptr);
            let min_alloc_chunk_size = tag_ptr.add(Tag::SIZE);
            if Self::is_chunk_size(min_alloc_chunk_size, free_acme) {
                self.insert_free(min_alloc_chunk_size, free_acme);
                Tag::init(tag_ptr.cast(), free_base, true, self.base_ptr());
            } else {
                tag_ptr = free_acme.sub(Tag::SIZE);
                Tag::init(tag_ptr.cast(), free_base, false, self.base_ptr());
            }

            if tag_ptr != post_alloc_ptr {
                post_alloc_ptr.cast::<*mut u8>().write(tag_ptr);
            }

            Ok(NonNull::new_unchecked(alloc_base))
        }
    }

    /// Free previously allocated/reallocated memory.
    /// # Safety
    /// `ptr` must have been previously allocated given `layout`.
    pub unsafe fn free(&mut self, ptr: NonNull<u8>, layout: alloc::Layout) {
        // self.scan_for_errors();
        // #[cfg(feature = "counters")]
        // self.counters.account_dealloc(layout.size());

        unsafe {
            let tag = Tag::from_alloc_base(ptr.as_ptr(), layout.size(), self.base_ptr());
            let mut chunk_base = (*tag).chunk_base(self.base_ptr());
            let mut chunk_acme = tag.add(Tag::SIZE);

            debug_assert!((*tag).is_allocated());
            debug_assert!(Self::is_chunk_size(chunk_base, chunk_acme));

            // let tail = Tag::from_acme(chunk_base);
            // if Tag::is_allocated(self)
            let prev_tag = Tag::from_acme(chunk_base);
            // try recombine below
            if !(*prev_tag).is_allocated() {
                let prev_tail = FreeTail::from_acme(chunk_base);
                let prev_head = FreeTail::to_head(prev_tail);
                self.remove_free(prev_head);

                chunk_base = prev_head;
            } else {
                Tag::set_above_free(prev_tag);
            }

            // try recombine above
            if (*tag).is_above_free() {
                let next_head = FreeHead::from_base(chunk_acme);
                let next_size = (*next_head).size_low;
                self.remove_free_by(next_head, Self::bin_idx(next_size));

                chunk_acme = chunk_acme.add(next_size);
            }

            // add the full recombined free chunk back into the books
            self.insert_free(chunk_base, chunk_acme);
        }
    }
}

// We define the implementation block generic over the configuration parameters.
