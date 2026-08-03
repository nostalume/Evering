use core::alloc;
use core::cell::UnsafeCell;
use core::ops::Deref;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::{marker::PhantomData, ptr::NonNull};

use crate::mem::Build;
use crate::numeric::bit::{bit_check, bit_flip};
use crate::numeric::{
    AlignPtr, Alignable,
    bit::{WORD_ALIGN, WORD_BITS, Word},
};
use crate::schema::{LayoutContext, LayoutInfo, SchemaKey, SharedSchema, schema_id};
use crate::{header, mem};

type UInt = usize;
type Size = UInt;
type Offset = UInt;
type AddrSpan = crate::mem::AddrSpan<Offset>;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TalcInfo {
    geometry: Geometry,
    usable_offset: u64,
    usable_length: u64,
}

impl SharedSchema for TalcInfo {
    const SCHEMA: SchemaKey = SchemaKey::new(schema_id("evering.talc.info"), 2);
}

unsafe impl LayoutInfo for TalcInfo {}

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
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
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
    /// # Safety
    ///
    /// - `ptr` must be within the same allocation as `base_ptr`.
    #[inline]
    const unsafe fn as_raw(self, base_ptr: *const u8) -> *mut T {
        base_ptr.wrapping_add(self.offset).cast_mut().cast()
    }

    /// # Safety
    /// - `ptr` must be within the same allocation as `base_ptr`.
    #[inline]
    const unsafe fn as_ptr(self, base_ptr: *const u8) -> NonNull<T> {
        unsafe { NonNull::new_unchecked(self.as_raw(base_ptr)) }
    }
}

impl<T: ?Sized> Clone for Rel<T> {
    fn clone(&self) -> Self {
        *self
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
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
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

    #[cfg(feature = "tracing")]
    #[inline]
    fn debug(tag: *mut Self, note: &'static str) {
        tracing::debug!("[Talc]: {} tag: {:?}, {:?}", note, tag, unsafe { *tag })
    }

    #[inline]
    const fn from_acme(acme: *mut u8) -> *mut Self {
        unsafe { acme.sub(Tag::SIZE).cast() }
    }

    unsafe fn from_alloc_base(ptr: *mut u8, size: Size, heap_base: *mut u8) -> *mut Self {
        unsafe {
            let post = ptr.add(size).align_up_of::<Word>();
            let post_rel = Rel::from_raw(post, heap_base);
            let tag_or_tag_rel = post.cast::<RelPtr>().read();
            if tag_or_tag_rel > post_rel {
                let tag_ptr = tag_or_tag_rel.as_raw(heap_base);
                tag_ptr.cast()
            } else {
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

    unsafe fn init(tag: *mut Self, chunk_base: *mut u8, is_above_free: bool, heap_base: *mut u8) {
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
    unsafe fn acme_tag(tag: *mut Tag, chunk_acme: *mut u8, heap_base: *mut u8) {
        if tag.cast() != chunk_acme {
            unsafe {
                let tag_rel = Rel::<Tag>::from_raw(tag, heap_base);
                chunk_acme.cast::<Rel<Tag>>().write(tag_rel);
            }
        }
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

/// Intrusive free-list node stored at a free chunk's base.
#[derive(Debug)]
#[repr(C)]
pub struct FreeNode {
    pub next: Option<Rel<FreeNode>>,
    pub prev_next: Rel<Option<Rel<FreeNode>>>,
}

pub type FreeNodeLink = Option<Rel<FreeNode>>;

impl FreeNode {
    const SIZE: Size = core::mem::size_of::<Self>();
    const ALIGN: Offset = core::mem::align_of::<Self>();

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

/// Free-chunk head paired with a size-bearing [`FreeTail`].
#[derive(Debug)]
#[repr(C)]
struct FreeHead {
    node: FreeNode,
    size_low: usize,
}

impl FreeHead {
    #[inline]
    const unsafe fn from_base(base: *mut u8) -> *mut Self {
        base.cast()
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

    #[inline]
    const fn to_base(head: *mut Self) -> *mut u8 {
        head.cast()
    }

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

/// Boundary tag holding the free chunk's size.
#[derive(Debug)]
#[repr(transparent)]
struct FreeTail {
    size_high: usize,
}

impl FreeTail {
    const SIZE: usize = core::mem::size_of::<Self>();
    const ALIGN: usize = core::mem::align_of::<Self>();

    #[inline]
    const unsafe fn from_acme(acme: *mut u8) -> *mut Self {
        unsafe { acme.sub(FreeTail::SIZE).cast() }
    }

    #[inline]
    const fn init(tail: *mut Self, size_high: Size) {
        unsafe { (*tail).size_high = size_high }
    }

    #[inline]
    const unsafe fn to_head(tail: *mut Self) -> *mut FreeHead {
        unsafe {
            let size = (*tail).size_high;
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
    const MIN_TAG_OFFSET: usize = FreeNode::SIZE;
    const MIN_CHUNK_SIZE: usize = Self::MIN_TAG_OFFSET + Tag::SIZE;

    #[inline]
    const unsafe fn from_endpoint<T, U>(base: *mut T, acme: *mut U) -> Self {
        Chunk {
            base: base.cast(),
            acme: acme.cast(),
        }
    }

    #[inline]
    const unsafe fn head(&self) -> *mut FreeHead {
        unsafe { FreeHead::from_base(self.base) }
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
    const unsafe fn prev_tag(&self) -> *mut Tag {
        Tag::from_acme(self.base)
    }

    #[inline]
    fn size_by_range(&self) -> Size {
        self.acme.addr() - self.base.addr()
    }

    #[inline]
    fn is_valid(self) -> bool {
        Self::is_chunk(self.base, self.acme)
    }

    #[inline]
    fn is_chunk<T, U>(base: *mut T, acme: *mut U) -> bool {
        if acme < base.cast() {
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
            #[cfg(feature = "tracing")]
            tracing::debug!("[Talc]: split suffix {:?}", suffix);
            if suffix.is_valid() {
                self.acme = suffix_base;
                (Some(suffix), tag_ptr.cast())
            } else {
                // Tag pointer doesn't change, resolve to original acme.
                tag_ptr = self.acme.sub(Tag::SIZE);
                (None, tag_ptr.cast())
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeometryError {
    TooSmall,
    TooManyBins,
    Invalid,
}

/// Immutable size-class geometry shared by every participant of one heap.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    bins: u16,
    linear: u16,
    linear_shift: u8,
    exponential_log: u8,
    divisions_log: u8,
    reserved: u8,
}

impl Geometry {
    const MIN_METADATA: usize = 512;
    const MAX_METADATA: usize = 16 << 10;
    const METADATA_RATIO_SHIFT: usize = 10;
    const MIN_LINEAR_SHIFT: usize = 4;
    const MAX_DIVISIONS_LOG: usize = 3;
    const DEFAULT_EXPONENTIAL_LOG: usize = 10;
    // A nullable relative bin head occupies two 64-bit words in the portable
    // format; budget against that upper cost on both native widths.
    const CANONICAL_BIN_BITS: usize = 129;

    /// Builds an explicit geometry covering every allocation up to `extent`.
    pub const fn new(
        extent: usize,
        linear_shift: u8,
        exponential_log: u8,
        divisions_log: u8,
    ) -> Result<Self, GeometryError> {
        if extent < Chunk::MIN_CHUNK_SIZE
            || linear_shift > exponential_log
            || exponential_log as u32 >= usize::BITS
            || exponential_log as u32 > extent.ilog2()
            || divisions_log as usize > Self::MAX_DIVISIONS_LOG
        {
            return Err(GeometryError::Invalid);
        }
        let linear = 1usize << (exponential_log - linear_shift);
        let exponential =
            (extent.ilog2() as usize - exponential_log as usize + 1) * (1usize << divisions_log);
        let bins = linear + exponential;
        if bins > u16::MAX as usize {
            return Err(GeometryError::TooManyBins);
        }
        let geometry = Self {
            bins: bins as u16,
            linear: linear as u16,
            linear_shift,
            exponential_log,
            divisions_log,
            reserved: 0,
        };
        match geometry.validate(extent) {
            Ok(()) => Ok(geometry),
            Err(error) => Err(error),
        }
    }

    pub const fn auto(extent: usize) -> Result<Self, GeometryError> {
        if extent < Chunk::MIN_CHUNK_SIZE {
            return Err(GeometryError::TooSmall);
        }
        let max_log = extent.ilog2() as usize;
        let exponential_log = max_log.min(Self::DEFAULT_EXPONENTIAL_LOG);
        let max_bins = (Self::budget(extent) * 8 / Self::CANONICAL_BIN_BITS)
            .min(WORD_BITS * WORD_BITS)
            .min(u16::MAX as usize);

        let mut linear_shift = Self::MIN_LINEAR_SHIFT.min(exponential_log);
        while linear_shift <= exponential_log {
            let linear = 1usize << (exponential_log - linear_shift);
            let mut divisions_log = Self::MAX_DIVISIONS_LOG;
            loop {
                let divisions = 1usize << divisions_log;
                let exponential = (max_log - exponential_log + 1) * divisions;
                let bins = linear + exponential;
                if bins <= max_bins {
                    let geometry = Self {
                        bins: bins as u16,
                        linear: linear as u16,
                        linear_shift: linear_shift as u8,
                        exponential_log: exponential_log as u8,
                        divisions_log: divisions_log as u8,
                        reserved: 0,
                    };
                    return match geometry.validate(extent) {
                        Ok(()) => Ok(geometry),
                        Err(error) => Err(error),
                    };
                }
                if divisions_log == 0 {
                    break;
                }
                divisions_log -= 1;
            }
            linear_shift += 1;
        }
        Err(GeometryError::TooManyBins)
    }

    pub const fn validate(self, extent: usize) -> Result<(), GeometryError> {
        if extent < Chunk::MIN_CHUNK_SIZE
            || self.reserved != 0
            || self.bins == 0
            || self.linear == 0
            || self.linear_shift as usize > self.exponential_log as usize
            || self.exponential_log as u32 >= usize::BITS
            || self.divisions_log as usize > Self::MAX_DIVISIONS_LOG
            || self.availability_words(WORD_BITS) > WORD_BITS
            || self.metadata_bytes() + Self::MIN_METADATA > extent
        {
            return Err(GeometryError::Invalid);
        }
        let expected_linear =
            1usize << (self.exponential_log as usize - self.linear_shift as usize);
        if self.linear as usize != expected_linear || self.bin_count() < expected_linear {
            return Err(GeometryError::Invalid);
        }
        let max_log = extent.ilog2() as usize;
        if max_log < self.exponential_log as usize {
            return Err(GeometryError::Invalid);
        }
        let exponential = self.bin_count() - self.linear as usize;
        if !exponential.is_multiple_of(self.divisions()) {
            return Err(GeometryError::Invalid);
        }
        let covered_log = self.exponential_log as usize + exponential / self.divisions() - 1;
        if covered_log < max_log || self.bin(extent) >= self.bin_count() {
            return Err(GeometryError::Invalid);
        }
        Ok(())
    }

    pub const fn budget(extent: usize) -> usize {
        let budget = extent >> Self::METADATA_RATIO_SHIFT;
        if budget < Self::MIN_METADATA {
            Self::MIN_METADATA
        } else if budget > Self::MAX_METADATA {
            Self::MAX_METADATA
        } else {
            budget
        }
    }

    #[inline(always)]
    pub const fn bin(self, size: usize) -> usize {
        let size = size.max(1);
        let exponential_start = 1usize << self.exponential_log;
        let index = if size < exponential_start {
            (size - 1) >> self.linear_shift
        } else {
            let octave = size.ilog2() as usize;
            let base = 1usize << octave;
            let division = ((size - base) << self.divisions_log) >> octave;
            self.linear as usize
                + (octave - self.exponential_log as usize) * self.divisions()
                + division
        };
        index.min(self.bin_count() - 1)
    }

    #[inline(always)]
    pub const fn bin_count(self) -> usize {
        self.bins as usize
    }

    #[inline(always)]
    const fn divisions(self) -> usize {
        1usize << self.divisions_log
    }

    pub const fn availability_words(self, word_bits: usize) -> usize {
        self.bin_count().div_ceil(word_bits)
    }

    pub const fn metadata_bytes(self) -> usize {
        self.bin_count() * core::mem::size_of::<FreeNodeLink>()
            + self.availability_words(WORD_BITS) * core::mem::size_of::<Word>()
    }
}

#[repr(C)]
pub struct TalcMeta {
    summary: Word,
    avails: Rel<[Word]>,
    bins: Rel<[FreeNodeLink]>,
}

unsafe impl Send for TalcMeta {}

impl TalcMeta {
    const SIZE: usize = core::mem::size_of::<Self>();

    const MIN_HEAP_SIZE: Size = Chunk::MIN_CHUNK_SIZE + Tag::SIZE;

    #[inline]
    const unsafe fn claim_metadata(&mut self, ptr: *mut u8, geometry: Geometry) -> *mut u8 {
        unsafe {
            let avails = ptr.cast::<Word>();
            let mut i = 0;
            while i < geometry.availability_words(WORD_BITS) {
                avails.add(i).write(0);
                i += 1;
            }
            self.avails = Rel::<[Word]>::from_raw(
                core::ptr::slice_from_raw_parts_mut(avails, geometry.availability_words(WORD_BITS)),
                self.base_ptr(),
            );

            let bins: *mut FreeNodeLink = avails.add(geometry.availability_words(WORD_BITS)).cast();
            i = 0;
            while i < geometry.bin_count() {
                let bin = bins.add(i);
                bin.write(None);
                i += 1;
            }
            let slice = core::ptr::slice_from_raw_parts_mut(bins, geometry.bin_count());
            let metadata = Rel::<[FreeNodeLink]>::from_raw(slice, self.base_ptr());
            self.bins = metadata;

            bins.add(geometry.bin_count()).cast()
        }
    }

    pub unsafe fn claim(&mut self, conf: Config) -> Result<(), ()> {
        let geometry = conf.geometry().map_err(|_| ())?;
        let Config { forward, size, .. } = conf;
        let base = unsafe {
            self.base_ptr()
                .byte_add(Self::SIZE + forward)
                .align_up_of::<Word>()
        };
        let size = size.align_down_of::<Word>();

        #[cfg(feature = "tracing")]
        tracing::debug!("[Talc]: claim base: {:?}, size: {:?}", base, size);
        if !self.bins.is_null() {
            if size <= Self::MIN_HEAP_SIZE {
                return Err(());
            }
            unsafe {
                Tag::init(base.cast(), self.base_ptr(), true, self.base_ptr());
                #[cfg(feature = "tracing")]
                Tag::debug(base.cast(), "claim: head");

                self.insert_free(
                    FreeHead::from_base(base.byte_add(Tag::SIZE)),
                    size,
                    geometry,
                );
                self.scan_errors(geometry);
                Ok(())
            }
        } else {
            unsafe {
                if size < geometry.metadata_bytes() + 2 * Tag::SIZE {
                    return Err(());
                }
                Tag::init(base.cast(), self.base_ptr(), true, self.base_ptr());
                #[cfg(feature = "tracing")]
                Tag::debug(base.cast(), "claim: head");

                let metadata_base = base.byte_add(Tag::SIZE);
                let metadata_acme = self.claim_metadata(metadata_base, geometry);
                let metadata_tag_acme = metadata_acme.byte_add(Tag::SIZE);

                // [(base_ptr)header][(base)tag][metadata(metadata_acme)][tag(metadata_tag_acme)][free]
                let free_size = size - (metadata_tag_acme.offset_from_unsigned(base));
                if Chunk::is_chunk_size(free_size) {
                    self.insert_free(FreeHead::from_base(metadata_tag_acme), free_size, geometry);
                    Tag::init(metadata_acme.cast(), base, true, self.base_ptr());
                    #[cfg(feature = "tracing")]
                    Tag::debug(base.cast(), "claim: metadata end");
                } else {
                    // the whole memory only hold a single chunk.
                    let acme = base.byte_add(size);
                    let tag_ptr = Tag::from_acme(acme);
                    Tag::init(tag_ptr, base, false, self.base_ptr());
                    Tag::acme_tag(tag_ptr, acme, self.base_ptr());
                    #[cfg(feature = "tracing")]
                    Tag::debug(base.cast(), "claim: single chunk end");
                }
                Ok(())
            }
        }
    }
}

impl TalcMeta {
    #[inline]
    const fn null() -> Self {
        TalcMeta {
            summary: 0,
            avails: Rel::null(),
            bins: Rel::null(),
        }
    }

    #[inline]
    const fn base_ptr(&self) -> *mut u8 {
        (&raw const *self).cast_mut().cast()
    }

    #[inline]
    const fn bins(&self, geometry: Geometry) -> NonNull<[FreeNodeLink]> {
        unsafe { self.bins.as_ptr(geometry.bin_count(), self.base_ptr()) }
    }

    #[inline]
    const fn avails(&self, geometry: Geometry) -> NonNull<[Word]> {
        unsafe {
            self.avails
                .as_ptr(geometry.availability_words(WORD_BITS), self.base_ptr())
        }
    }

    #[inline]
    const fn word_bit_idx(idx: usize) -> (usize, usize) {
        (idx / WORD_BITS, idx % WORD_BITS)
    }

    #[inline]
    const unsafe fn toggle_avail(&mut self, idx: usize, should_be: bool, geometry: Geometry) {
        debug_assert!(idx < geometry.bin_count());

        let (word_idx, bit_idx) = Self::word_bit_idx(idx);

        let avails = self.avails(geometry).as_mut_ptr().cast::<Word>();
        let word = unsafe { &mut *avails.add(word_idx) };
        let was_empty = *word == 0;
        debug_assert!(bit_check(*word, bit_idx) != should_be);
        bit_flip(word, bit_idx);
        debug_assert!(bit_check(*word, bit_idx) == should_be);
        let is_empty = *word == 0;
        if was_empty != is_empty {
            if is_empty {
                self.summary &= !(1 << word_idx);
            } else {
                self.summary |= 1 << word_idx;
            }
        }
    }

    #[inline]
    const fn set_avail(&mut self, idx: usize, geometry: Geometry) {
        unsafe { self.toggle_avail(idx, true, geometry) };
    }

    #[inline]
    const fn clear_avail(&mut self, idx: usize, geometry: Geometry) {
        unsafe { self.toggle_avail(idx, false, geometry) };
    }

    #[inline]
    const fn bin_by_idx(&self, idx: usize, geometry: Geometry) -> *mut FreeNodeLink {
        debug_assert!(idx < geometry.bin_count());
        unsafe { self.bins(geometry).as_mut_ptr().add(idx) }
    }

    #[inline]
    const fn bin_by_size(&self, size: usize, geometry: Geometry) -> (*mut FreeNodeLink, usize) {
        let idx = geometry.bin(size);
        (self.bin_by_idx(idx, geometry), idx)
    }

    #[inline(always)]
    const fn next_avail_bin_idx(&self, idx: usize, geometry: Geometry) -> Option<usize> {
        if idx >= geometry.bin_count() {
            return None;
        }
        let word_idx = idx / WORD_BITS;
        let bit_idx = idx % WORD_BITS;
        let avails = self.avails(geometry).as_ptr().cast::<Word>();
        let shift_avails = unsafe { *avails.add(word_idx) } >> bit_idx;
        if shift_avails != 0 {
            return Some(idx + shift_avails.trailing_zeros() as usize);
        }

        let next_word = word_idx + 1;
        if next_word >= geometry.availability_words(WORD_BITS) {
            return None;
        }
        let summary = self.summary & (Word::MAX << next_word);
        if summary == 0 {
            return None;
        }
        let next_word = summary.trailing_zeros() as usize;
        let word = unsafe { *avails.add(next_word) };
        Some(next_word * WORD_BITS + word.trailing_zeros() as usize)
    }

    #[cfg(not(debug_assertions))]
    fn scan_errors(&self, _: Geometry) {}

    #[cfg(debug_assertions)]
    fn scan_errors(&self, geometry: Geometry) {
        for idx in 0..geometry.bin_count() {
            unsafe {
                let iter = FreeNodeIter::new(*self.bin_by_idx(idx, geometry), self.base_ptr());
                for head in iter {
                    let (word_idx, bit_idx) = Self::word_bit_idx(idx);
                    assert!(
                        bit_check(
                            *self.avails(geometry).as_ptr().cast::<Word>().add(word_idx),
                            bit_idx,
                        ),
                        "[Talc]: scan errors: word_idx {}, bit_idx {}",
                        word_idx,
                        bit_idx
                    );

                    let acme = FreeHead::to_acme(head.as_ptr());
                    let tail = FreeHead::to_tail(head.as_ptr());
                    let size_low = head.as_ref().size_low;
                    let size_high = (*tail).size_high;
                    let size_real = acme.byte_offset_from_unsigned(head.as_ptr());
                    assert!(size_low == size_high && size_high == size_real);

                    let prev_tag = Tag::from_acme(head.as_ptr().cast());
                    assert!((*prev_tag).is_above_free());
                    assert!((*prev_tag).is_allocated());
                }
            }
        }
    }
}

impl TalcMeta {
    #[inline]
    const fn insert_free(&mut self, head: *mut FreeHead, size: Size, geometry: Geometry) {
        debug_assert!(Chunk::is_chunk_size(size));

        let (bin_ptr, bin_idx) = self.bin_by_size(size, geometry);
        unsafe {
            if (*bin_ptr).is_none() {
                self.set_avail(bin_idx, geometry);
            }
            FreeHead::init(head, bin_ptr, size, self.base_ptr());
            let tail = FreeHead::to_tail(head);
            FreeTail::init(tail, size);
        }
    }

    #[inline]
    const unsafe fn remove_free(
        &mut self,
        head: *mut FreeHead,
        bin_idx: usize,
        geometry: Geometry,
    ) {
        unsafe {
            let bin = self.bin_by_idx(bin_idx, geometry);
            debug_assert!((*bin).is_some());
            FreeHead::deinit(head, self.base_ptr());

            if (*bin).is_none() {
                self.clear_avail(bin_idx, geometry);
            }
        }
    }

    #[inline]
    const unsafe fn remove_free_by_head(&mut self, head: *mut FreeHead, geometry: Geometry) {
        unsafe {
            let bin_idx = geometry.bin((*head).size_low);
            self.remove_free(head, bin_idx, geometry);
        }
    }

    #[inline]
    const unsafe fn remove_free_by_tail(
        &mut self,
        tail: *mut FreeTail,
        geometry: Geometry,
    ) -> *mut FreeHead {
        unsafe {
            let head = FreeTail::to_head(tail);
            self.remove_free_by_head(head, geometry);
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
        size: Size,
        align: Offset,
        geometry: Geometry,
    ) -> Option<(Chunk, *mut u8, *mut u8)> {
        let req_size = Chunk::chunk_size(size);
        let need_align = align > WORD_ALIGN;

        let mut bin_idx = self.next_avail_bin_idx(geometry.bin(req_size), geometry)?;
        #[cfg(feature = "tracing")]
        tracing::debug!("[Talc]: acquire chunk: next avail idx: {}", bin_idx);
        loop {
            unsafe {
                let cur_rel = *self.bin_by_idx(bin_idx, geometry);
                let iter = FreeNodeIter::new(cur_rel, self.base_ptr());
                for head in iter {
                    let chunk_size = head.as_ref().size_low;
                    let base = FreeHead::to_base(head.as_ptr());
                    let acme = FreeHead::to_acme(head.as_ptr());
                    if chunk_size >= req_size && !need_align {
                        let alloc_acme = base.add(size).align_up_of::<Word>();
                        self.remove_free(head.as_ptr(), bin_idx, geometry);
                        return Some((Chunk::from_endpoint(base, acme), base, alloc_acme));
                    }
                    let alloc_base = base.align_up(align);
                    if alloc_base.add(req_size) <= acme {
                        let alloc_acme = alloc_base.add(size).align_up_of::<Word>();
                        self.remove_free(head.as_ptr(), bin_idx, geometry);
                        return Some((Chunk::from_endpoint(base, acme), alloc_base, alloc_acme));
                    }
                }
            }
            bin_idx = self.next_avail_bin_idx(bin_idx + 1, geometry)?;
        }
    }
    pub unsafe fn allocate(
        &mut self,
        layout: alloc::Layout,
        geometry: Geometry,
    ) -> Result<NonNull<u8>, ()> {
        if layout.size() == 0 {
            return Ok(NonNull::dangling());
        }

        self.scan_errors(geometry);
        unsafe {
            let (mut free, alloc_base, alloc_acme) = self
                .acquire_chunk(layout.size(), layout.align(), geometry)
                .ok_or(())?;

            #[cfg(feature = "tracing")]
            tracing::debug!("[Talc]: acquire chunk: {:?}", free);

            if let Some(prefix) = free.split_prefix(alloc_base) {
                #[cfg(feature = "tracing")]
                tracing::debug!("[Talc]: insert prefix: {:?}", prefix);
                self.insert_free(prefix.head(), prefix.size_by_range(), geometry);
            } else {
                Tag::clear_above_free(free.prev_tag());
            }

            let (suffix, tag_ptr) = free.split_suffix(alloc_acme);
            if let Some(suffix) = suffix {
                #[cfg(feature = "tracing")]
                tracing::debug!("[Talc]: insert suffix: {:?}", suffix);
                self.insert_free(suffix.head(), suffix.size_by_range(), geometry);
                Tag::init(tag_ptr, free.base, true, self.base_ptr());
            } else {
                Tag::init(tag_ptr, free.base, false, self.base_ptr());
            }

            Tag::acme_tag(tag_ptr, alloc_acme, self.base_ptr());

            Ok(NonNull::new_unchecked(alloc_base))
        }
    }

    /// Free previously allocated/reallocated memory.
    ///
    /// # Safety
    /// `ptr` must have been previously allocated given `layout`.
    pub unsafe fn deallocate(&mut self, ptr: NonNull<u8>, size: Size, geometry: Geometry) {
        if size == 0 {
            return;
        }

        self.scan_errors(geometry);
        unsafe {
            let tag = Tag::from_alloc_base(ptr.as_ptr(), size, self.base_ptr());
            let mut chunk = Tag::chunk(tag, self.base_ptr());

            #[cfg(feature = "tracing")]
            tracing::debug!("[Talc]: deallocate with tag: {:?}, chunk: {:?}", tag, chunk);

            debug_assert!((*tag).is_allocated());
            debug_assert!(Chunk::is_valid(chunk));

            let prev_tag = chunk.prev_tag();
            #[cfg(feature = "tracing")]
            tracing::debug!(
                "[Talc]: deallocate with prev tag: {:?}, read: {:#b}",
                prev_tag,
                prev_tag.cast::<Word>().read()
            );
            if !(*prev_tag).is_allocated() {
                let prev_tail = chunk.prev_tail();
                let prev_head = self.remove_free_by_tail(prev_tail, geometry);

                chunk.base = prev_head.cast();
            } else {
                Tag::set_above_free(prev_tag);
            }

            if (*tag).is_above_free() {
                let next_head = chunk.next_head();
                let next_size = (*next_head).size_low;
                self.remove_free_by_head(next_head, geometry);

                chunk.acme = chunk.acme.byte_add(next_size);
            }

            self.insert_free(chunk.head(), chunk.size_by_range(), geometry);
        }
    }
}

#[repr(C)]
pub struct TalckMeta {
    mutation: AtomicUsize,
    talc: UnsafeCell<TalcMeta>,
}

impl SharedSchema for TalckMeta {
    const SCHEMA: SchemaKey = SchemaKey::new(schema_id("evering.talc"), 2);
}

unsafe impl Sync for TalckMeta {}

impl TalckMeta {
    #[inline]
    const fn null() -> Self {
        Self {
            mutation: AtomicUsize::new(MUTATION_CLEAN),
            talc: UnsafeCell::new(TalcMeta::null()),
        }
    }

    #[inline]
    unsafe fn claim(&mut self, conf: Config) -> Result<(), ()> {
        unsafe { self.talc.get_mut().claim(conf) }
    }
}

impl TalckMeta {
    #[inline]
    const fn talc_ref(&self) -> &TalcMeta {
        unsafe { self.talc.as_ref_unchecked() }
    }

    #[inline]
    const fn talc_ptr(&self) -> *mut TalcMeta {
        self.talc.get()
    }
}

pub type Header = header::RcHeader<TalckMeta>;
pub type MapHeader = mem::Mapped<Header>;

pub struct Talc<H: const Deref<Target = Header>> {
    pub header: H,
    owner: u8,
    geometry: Geometry,
}

unsafe impl<H: const Deref<Target = Header> + Send> Send for Talc<H> {}
unsafe impl<H: const Deref<Target = Header> + Sync> Sync for Talc<H> {}

pub type RefTalc<'a> = Talc<&'a Header>;
pub type MapTalc = Talc<MapHeader>;

#[cfg(test)]
pub(crate) const ALLOC_CLAIMED: usize = 1;
#[cfg(test)]
pub(crate) const ALLOC_MUTATED: usize = 2;
#[cfg(test)]
pub(crate) const ALLOC_CLEAN: usize = 3;
#[cfg(test)]
pub(crate) const FREE_CLAIMED: usize = 4;
#[cfg(test)]
pub(crate) const FREE_MUTATED: usize = 5;
#[cfg(test)]
pub(crate) const FREE_CLEAN: usize = 6;
#[cfg(test)]
static CRASH_AFTER: AtomicUsize = AtomicUsize::new(0);

#[cfg(all(test, unix, feature = "map"))]
pub(crate) fn crash_after_for_test(point: usize) {
    CRASH_AFTER.store(point, Ordering::Relaxed);
}

#[cfg(test)]
fn crash_after(point: usize) {
    if CRASH_AFTER.load(Ordering::Relaxed) == point {
        std::process::exit(120 + point as i32);
    }
}

impl<H: const Deref<Target = Header>> Talc<H> {
    #[inline]
    pub(crate) fn base_ptr(&self) -> *const u8 {
        self.header.talc_ref().base_ptr()
    }

    pub fn allocate(&self, layout: alloc::Layout) -> Result<Meta, MutationError> {
        let mutation = self.begin_mutation()?;
        #[cfg(test)]
        crash_after(ALLOC_CLAIMED);
        let result = unsafe { self.allocate_during(&mutation, layout) };
        #[cfg(test)]
        crash_after(ALLOC_MUTATED);
        mutation.commit();
        #[cfg(test)]
        crash_after(ALLOC_CLEAN);
        result
    }

    #[cfg(test)]
    pub(crate) fn deallocate(
        &self,
        ptr: NonNull<u8>,
        layout: alloc::Layout,
    ) -> Result<(), MutationError> {
        let mutation = self.begin_mutation()?;
        #[cfg(test)]
        crash_after(FREE_CLAIMED);
        unsafe { self.deallocate_during(&mutation, ptr, layout) };
        #[cfg(test)]
        crash_after(FREE_MUTATED);
        mutation.commit();
        #[cfg(test)]
        crash_after(FREE_CLEAN);
        Ok(())
    }

    pub(crate) unsafe fn release_value<T: ?Sized>(
        &self,
        pointer: *mut T,
        meta: Meta,
        layout: alloc::Layout,
    ) -> Result<(), Meta> {
        let mutation = match Mutation::claim(&self.header.mutation, self.owner) {
            Ok(mutation) => mutation,
            Err(_) => return Err(meta),
        };
        #[cfg(test)]
        crash_after(FREE_CLAIMED);
        unsafe {
            core::ptr::drop_in_place(pointer);
            (&mut *self.header.talc_ptr()).deallocate(
                meta.as_nonnull(self.base_ptr()),
                layout.size(),
                self.geometry,
            );
        }
        #[cfg(test)]
        crash_after(FREE_MUTATED);
        mutation.commit();
        #[cfg(test)]
        crash_after(FREE_CLEAN);
        Ok(())
    }

    pub(crate) fn begin_mutation(&self) -> Result<Mutation<'_>, MutationError> {
        Mutation::claim(&self.header.mutation, self.owner)
    }

    pub(crate) unsafe fn allocate_during(
        &self,
        mutation: &Mutation<'_>,
        layout: alloc::Layout,
    ) -> Result<Meta, MutationError> {
        debug_assert!(mutation.owns(&self.header.mutation, self.owner));
        unsafe {
            (&mut *self.header.talc_ptr())
                .allocate(layout, self.geometry)
                .map(|ptr| Meta::from_ptr(ptr.as_ptr(), self.base_ptr(), layout.size()))
                .map_err(|_| MutationError::Exhausted)
        }
    }

    pub(crate) unsafe fn deallocate_during(
        &self,
        mutation: &Mutation<'_>,
        ptr: NonNull<u8>,
        layout: alloc::Layout,
    ) {
        debug_assert!(mutation.owns(&self.header.mutation, self.owner));
        unsafe {
            (&mut *self.header.talc_ptr()).deallocate(ptr, layout.size(), self.geometry);
        }
    }

    pub(crate) fn pointer(&self, meta: &Meta) -> NonNull<u8> {
        unsafe { meta.as_nonnull(self.base_ptr()) }
    }

    pub(crate) fn meta_for(&self, pointer: NonNull<u8>, size: usize) -> Option<Meta> {
        let info = self.header().layout_info();
        let offset = pointer
            .as_ptr()
            .addr()
            .checked_sub(self.base_ptr().addr())?;
        let lower = core::mem::size_of::<TalcMeta>().checked_add(info.usable_offset as usize)?;
        let bound = lower.checked_add(info.usable_length as usize)?;
        let end = offset.checked_add(size)?;
        (offset >= lower && end <= bound)
            .then(|| unsafe { Meta::from_ptr(pointer.as_ptr(), self.base_ptr(), size) })
    }
}

fn poison_mutation(state: &AtomicUsize, owner: u8) -> bool {
    state
        .compare_exchange(
            owner_word(owner),
            MUTATION_POISONED,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
}

impl<'a> Mutation<'a> {
    fn claim(state: &'a AtomicUsize, owner: u8) -> Result<Self, MutationError> {
        match state.compare_exchange(
            MUTATION_CLEAN,
            owner_word(owner),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(Self {
                state,
                owner,
                committed: false,
            }),
            Err(MUTATION_POISONED) => Err(MutationError::Poisoned),
            Err(owner) => Err(MutationError::Busy((owner - 1) as u8)),
        }
    }
}

const MUTATION_CLEAN: usize = 0;
const MUTATION_POISONED: usize = usize::MAX;

const fn owner_word(slot: u8) -> usize {
    slot as usize + 1
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationError {
    Busy(u8),
    Poisoned,
    Exhausted,
    LayoutOverflow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MutationState {
    Clean,
    Owned(u8),
    Poisoned,
}

pub(crate) struct Mutation<'a> {
    state: &'a AtomicUsize,
    owner: u8,
    committed: bool,
}

impl Mutation<'_> {
    fn owns(&self, state: &AtomicUsize, owner: u8) -> bool {
        core::ptr::eq(self.state, state) && self.owner == owner
    }

    pub(crate) fn commit(mut self) {
        self.state.store(MUTATION_CLEAN, Ordering::Release);
        self.committed = true;
    }
}

impl Drop for Mutation<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.state.store(MUTATION_POISONED, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod mutation_tests {
    use super::{
        MUTATION_CLEAN, MUTATION_POISONED, Mutation, MutationError, owner_word, poison_mutation,
    };
    use core::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn live_contention_is_one_bounded_attempt_and_reports_owner() {
        let state = AtomicUsize::new(MUTATION_CLEAN);
        let held = Mutation::claim(&state, 2).unwrap();
        assert!(matches!(
            Mutation::claim(&state, 1),
            Err(MutationError::Busy(2))
        ));
        held.commit();
        assert_eq!(state.load(Ordering::Acquire), MUTATION_CLEAN);
    }

    #[test]
    fn only_the_exact_dead_owner_can_make_mutation_permanently_poisoned() {
        let state = AtomicUsize::new(owner_word(3));
        assert!(!poison_mutation(&state, 2));
        assert!(poison_mutation(&state, 3));
        assert_eq!(state.load(Ordering::Acquire), MUTATION_POISONED);
        assert!(matches!(
            Mutation::claim(&state, 0),
            Err(MutationError::Poisoned)
        ));
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::{Chunk, Geometry, GeometryError, TalcMeta, WORD_BITS, Word};

    #[test]
    fn automatic_geometry_covers_varied_heap_extents_monotonically() {
        for extent in [4 << 10, 64 << 10, 1 << 20, 64 << 20, 1 << 30] {
            let geometry = Geometry::auto(extent).expect("supported heap extent");
            geometry.validate(extent).expect("self-valid geometry");

            let mut prior = 0;
            let mut size = Chunk::MIN_CHUNK_SIZE;
            while size <= extent {
                let bin = geometry.bin(size);
                assert!(bin >= prior);
                assert!(bin < geometry.bin_count());
                prior = bin;
                size = size.saturating_add((size >> 5).max(1));
            }
            assert!(geometry.metadata_bytes() <= Geometry::budget(extent));
        }
    }

    #[test]
    fn logical_geometry_is_independent_of_native_availability_word_width() {
        let geometry = Geometry::auto(16 << 20).unwrap();
        assert_eq!(
            geometry.availability_words(32),
            geometry.bin_count().div_ceil(32)
        );
        assert_eq!(
            geometry.availability_words(64),
            geometry.bin_count().div_ceil(64)
        );
        assert!(geometry.bin(1 << 10) < geometry.bin(2 << 10));
        assert!(geometry.bin(2 << 10) < geometry.bin(4 << 10));
    }

    #[test]
    fn explicit_geometry_may_cover_a_smaller_final_extent() {
        const GEOMETRY: Geometry = match Geometry::new(64 << 20, 5, 10, 2) {
            Ok(geometry) => geometry,
            Err(_) => panic!("valid explicit geometry"),
        };

        GEOMETRY.validate(32 << 20).unwrap();
        assert_eq!(
            Geometry::new(1 << 20, 11, 10, 2),
            Err(GeometryError::Invalid)
        );
    }

    #[test]
    fn summary_finds_and_clears_availability_across_words() {
        #[repr(C)]
        struct Storage {
            meta: TalcMeta,
            words: [Word; 512],
        }

        let geometry = Geometry::new(16 << 20, 5, 10, 3).unwrap();
        assert!(geometry.availability_words(WORD_BITS) > 1);
        assert!(geometry.metadata_bytes() <= core::mem::size_of::<[Word; 512]>());

        let mut storage = Storage {
            meta: TalcMeta::null(),
            words: [0; 512],
        };
        let metadata = storage.words.as_mut_ptr().cast();
        unsafe {
            storage.meta.claim_metadata(metadata, geometry);
        }
        let distant = WORD_BITS + 7;
        storage.meta.set_avail(distant, geometry);
        assert_eq!(storage.meta.next_avail_bin_idx(0, geometry), Some(distant));
        storage.meta.clear_avail(distant, geometry);
        assert_eq!(storage.meta.next_avail_bin_idx(0, geometry), None);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    forward: Offset,
    size: Size,
    geometry: Option<Geometry>,
}

impl Config {
    #[inline]
    pub const fn new(size: Size) -> Self {
        Self {
            forward: 0,
            size,
            geometry: None,
        }
    }

    pub const fn with_geometry(self, geometry: Geometry) -> Self {
        Self {
            geometry: Some(geometry),
            ..self
        }
    }

    const fn geometry(self) -> Result<Geometry, GeometryError> {
        match self.geometry {
            Some(geometry) => match geometry.validate(self.size) {
                Ok(()) => Ok(geometry),
                Err(error) => Err(error),
            },
            None => Geometry::auto(self.size),
        }
    }

    /// Constrains `[forward][size]` to not exceed the given `[bound]`, adjusting for the forward offset.
    ///
    /// Example: If `self.size = 100`, `self.forward = 10`, and `bound = 50`,
    /// the new `size` is `min(100, 50) - 10 = 40`.
    #[inline]
    pub const fn with_bound(self, bound: Size) -> Self {
        assert!(
            bound > self.forward,
            "[Talc]: [forward][size] where must [forward] < [bound]"
        );
        Self {
            size: self.size.min(bound) - self.forward,
            ..self
        }
    }
}

unsafe impl header::Layout for TalckMeta {
    type Config = Config;
    type Info = TalcInfo;

    const MAGIC: header::Magic = 0x1234;

    fn info(conf: &Self::Config, _: LayoutContext) -> Self::Info {
        TalcInfo {
            geometry: conf.geometry().unwrap_or(Geometry {
                bins: 1,
                linear: 1,
                linear_shift: 0,
                exponential_log: 0,
                divisions_log: 0,
                reserved: 0,
            }),
            usable_offset: conf.forward as u64,
            usable_length: conf.size as u64,
        }
    }

    unsafe fn init(destination: *mut Self, conf: Self::Config) -> header::Status {
        unsafe {
            destination.write(Self::null());
            match (&mut *destination).claim(conf) {
                Ok(_) => header::Status::Initialized,
                Err(_) => header::Status::Corrupted,
            }
        }
    }

    fn attach(&self, _conf: &Self::Config) -> header::Status {
        header::Status::Initialized
    }
}

impl<H: const Deref<Target = Header>> Talc<H> {
    #[inline]
    pub const fn header(&self) -> &Header {
        &self.header
    }
}

impl Clone for RefTalc<'_> {
    fn clone(&self) -> Self {
        Self {
            header: self.header,
            owner: self.owner,
            geometry: self.geometry,
        }
    }
}

impl MapTalc {
    #[inline]
    pub fn from_handle(handle: MapHeader) -> Self {
        let owner = handle.peer().slot();
        let geometry = handle.layout_info().geometry;
        Self {
            header: handle,
            owner,
            geometry,
        }
    }

    pub(crate) fn region_id(&self) -> crate::schema::RegionId {
        self.header.region_id()
    }

    #[inline]
    pub(crate) fn from_layout(area: Build, conf: Config) -> Result<Self, mem::Error> {
        let mut area = area;
        let reserve = area.reserve::<Header>()?;
        let conf = conf.with_bound(reserve.remaining_after());
        #[cfg(feature = "tracing")]
        tracing::debug!("[Talc]: with conf: {:?}", conf);

        let handle = reserve.commit(conf)?;
        Ok(Self::from_handle(handle))
    }

    #[inline]
    pub fn as_ref(&self) -> RefTalc<'_> {
        RefTalc {
            header: &self.header,
            owner: self.owner,
            geometry: self.geometry,
        }
    }

    pub(crate) fn mutation_state(&self) -> MutationState {
        match self.header.mutation.load(Ordering::Acquire) {
            MUTATION_CLEAN => MutationState::Clean,
            MUTATION_POISONED => MutationState::Poisoned,
            owner => MutationState::Owned((owner - 1) as u8),
        }
    }

    pub(crate) fn poison_owner(&self, owner: u8) -> bool {
        poison_mutation(&self.header.mutation, owner)
    }

    pub(crate) fn clear_dead_owner(&self, owner: u8) -> bool {
        match self.header.mutation.compare_exchange(
            owner_word(owner),
            MUTATION_CLEAN,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) | Err(MUTATION_CLEAN) => true,
            Err(_) => false,
        }
    }

    pub(crate) fn finish_recovery(&self, dead: u8) -> bool {
        match self.mutation_state() {
            MutationState::Clean | MutationState::Poisoned => {}
            MutationState::Owned(owner) if owner == dead => {
                if !self.poison_owner(dead) && self.mutation_state() != MutationState::Poisoned {
                    return false;
                }
            }
            MutationState::Owned(_) => return false,
        }
        self.header.recover_member(dead);
        !self.header.has_member(dead)
    }

    #[cfg(test)]
    pub(crate) fn abandon_mutation_for_test(&self, owner: u8) {
        assert_eq!(
            self.header.mutation.compare_exchange(
                MUTATION_CLEAN,
                owner_word(owner),
                Ordering::AcqRel,
                Ordering::Acquire,
            ),
            Ok(MUTATION_CLEAN)
        );
    }
}

impl TryFrom<Build> for MapTalc {
    type Error = mem::Error;

    fn try_from(area: Build) -> Result<Self, Self::Error> {
        use crate::mem::MemOps;
        let size = area.size();
        Self::from_layout(area, Config::new(size))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Meta {
    view: AddrSpan,
}

unsafe impl Send for Meta {}

impl SharedSchema for Meta {
    const SCHEMA: SchemaKey = SchemaKey::new(schema_id("evering.talc.meta"), 1);
}

unsafe impl crate::msg::Repr for Meta {
    const SCHEMA: SchemaKey = <Self as SharedSchema>::SCHEMA;
}

impl Meta {
    pub(crate) const fn null() -> Self {
        Self {
            view: AddrSpan::null(),
        }
    }

    pub(crate) fn layout(&self) -> alloc::Layout {
        unsafe { alloc::Layout::from_size_align_unchecked(self.view.size, 1) }
    }

    #[inline]
    pub(crate) const fn is_null(&self) -> bool {
        self.view.is_null()
    }

    #[inline]
    const unsafe fn from_ptr(ptr: *const u8, base_ptr: *const u8, size: Size) -> Self {
        let offset = unsafe { ptr.byte_offset_from_unsigned(base_ptr) };
        Self {
            view: AddrSpan::new(offset, size),
        }
    }

    #[inline]
    const unsafe fn as_nonnull(&self, base_ptr: *const u8) -> NonNull<u8> {
        if self.is_null() {
            return NonNull::dangling();
        }
        unsafe { self.view.as_nonnull(base_ptr) }
    }
}
