use core::{
    mem,
    ops::Deref,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

use crate::numeric::{Alignable, CastInto, Packable};
use crossbeam_utils::Backoff;

type UInt = u32;
type AtomicUInt = AtomicU32;
type AtomicPackedUInt = AtomicU64;

type Offset = UInt;
type Size = UInt;

const ARENA_MAX_CAPACITY: Size = UInt::MAX;
const SENTINEL_OFFSET: Offset = UInt::MAX;
const SENTINEL_SIZE: Size = UInt::MAX;
const SEGMENT_NODE_SIZE: Size = UInt::size_of::<SegmentNode>();
const SEGMENT_NODE_REMOVED: Size = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Insufficient space in the arena
    InsufficientSpace {
        /// The requested size
        requested: UInt,
        /// The remaining size
        available: UInt,
    },
    /// The arena is read-only
    ReadOnly,

    /// Index is out of range
    OutOfBounds {
        /// The offset
        offset: usize,
        /// The current allocated size of the arena
        allocated: usize,
    },
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InsufficientSpace {
                requested,
                available,
            } => write!(
                f,
                "Allocation failed: requested size is {}, but only {} is available",
                requested, available
            ),
            Self::ReadOnly => write!(f, "Arena is read-only"),
            Self::OutOfBounds { offset, allocated } => write!(
                f,
                "Index out of bounds: offset {} is out of range, the current allocated size is {}",
                offset, allocated
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddrSpan {
    pub start_offset: Offset,
    pub size: Size,
}

impl AddrSpan {
    #[inline]
    pub const fn null() -> Self {
        Self {
            start_offset: 0,
            size: 0,
        }
    }

    #[inline]
    pub const fn new(offset: Offset, size: Size) -> Self {
        Self {
            start_offset: offset,
            size,
        }
    }

    #[inline]
    pub const fn end_offset(&self) -> u32 {
        self.start_offset + self.size
    }

    #[inline]
    pub const fn align_of<T>(&self) -> Self {
        Self {
            start_offset: self.start_offset.align_up_of::<T>(),
            size: UInt::size_of::<T>(),
        }
    }

    #[inline]
    pub const fn align_to<T>(&self) -> Self {
        let aligned_offset = self.start_offset.align_up_of::<T>();
        let new_size = self.end_offset() - aligned_offset;

        Self {
            start_offset: aligned_offset,
            size: new_size,
        }
    }

    #[inline]
    const fn shift(&self, delta: Offset) -> Self {
        Self {
            start_offset: self.start_offset.saturating_add(delta),
            size: self.size,
        }
    }

    #[inline]
    const unsafe fn as_ptr(&self, base_ptr: *const u8) -> *const u8 {
        unsafe { base_ptr.add(self.start_offset.cast_into()) }
    }

    #[inline]
    const unsafe fn as_mut_ptr(&self, base_ptr: *mut u8) -> *mut u8 {
        unsafe { base_ptr.add(self.start_offset.cast_into()) }
    }
}

/// The metadata of the structs allocated from ARENA.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Meta {
    base_ptr: *const u8,
    raw: AddrSpan,
    view: AddrSpan,
}

unsafe impl Send for Meta {}
unsafe impl Sync for Meta {}

impl Meta {
    #[inline]
    const fn null(base_ptr: *const u8) -> Self {
        Self {
            base_ptr,
            raw: AddrSpan::null(),
            view: AddrSpan::null(),
        }
    }

    #[inline]
    const fn raw(base_ptr: *const u8, raw_offset: Offset, raw_size: Size) -> Self {
        Self {
            base_ptr,
            raw: AddrSpan {
                start_offset: raw_offset,
                size: raw_size,
            },
            // just set the ptr_offset to the memory_offset, and ptr_size to the memory_size.
            // we will align the ptr_offset and ptr_size when it should be aligned.
            view: AddrSpan {
                start_offset: raw_offset,
                size: raw_size,
            },
        }
    }

    #[inline]
    unsafe fn clear<A: crate::area::MemBlkOps>(&self, arena: &A) {
        unsafe {
            let ptr = arena.get_ptr_mut(self.view.start_offset as usize);
            core::ptr::write_bytes(ptr, 0, self.view.size as usize);
        }
    }

    #[inline]
    fn align_to<T>(&mut self) {
        let aligned = self.raw.align_to::<T>();
        self.view = aligned;
    }

    #[inline]
    fn align_of<T>(&mut self) {
        let aligned = self.raw.align_of::<T>();
        self.view = aligned;
    }
}

#[derive(Debug)]
#[repr(C, align(8))]
pub struct Header {
    /// The sentinel node for the ordered free list.
    pub(super) sentinel: SegmentNode,
    pub(super) allocated: AtomicUInt,
    pub(super) min_segment_size: AtomicUInt,
    pub(super) discarded: AtomicUInt,
}

impl Header {
    #[inline]
    fn new(initial_size: u32, min_segment_size: u32) -> Self {
        Self {
            allocated: AtomicU32::new(initial_size),
            sentinel: SegmentNode::sentinel(),
            min_segment_size: AtomicU32::new(min_segment_size),
            discarded: AtomicU32::new(0),
        }
    }

    #[inline]
    fn load(&self) -> SegmentNodeData {
        self.sentinel.load()
    }

    #[inline]
    fn allocate<T>(&self, total: u32) -> Option<(Size, Size)> {
        let mut allocated = self.allocated();
        loop {
            let aligned_offset = allocated.align_up_of::<T>();
            let want = aligned_offset + UInt::size_of::<T>();
            if want > total {
                break None;
            }

            match self.allocated.compare_exchange_weak(
                allocated,
                want,
                Ordering::SeqCst,
                Ordering::Acquire,
            ) {
                Ok(old) => break Some((want, old)),
                Err(changed) => {
                    allocated = changed;
                    continue;
                }
            }
        }
    }

    #[inline]
    fn allocated(&self) -> Size {
        self.allocated.load(Ordering::Acquire)
    }

    #[inline]
    fn min_segment_size(&self) -> u32 {
        self.min_segment_size.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SegmentNodeData {
    size: Size,
    next: Offset,
}

impl From<u64> for SegmentNodeData {
    fn from(value: u64) -> Self {
        Self::decode(value)
    }
}

impl Into<u64> for SegmentNodeData {
    fn into(self) -> u64 {
        self.encode()
    }
}

impl core::fmt::Debug for SegmentNodeData {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SegmentNode")
            .field("size", &self.size)
            .field("next", &self.next)
            .finish()
    }
}

impl SegmentNodeData {
    #[inline]
    const fn encode(self) -> <UInt as Packable>::Packed {
        UInt::pack(self.size, self.next)
    }
    #[inline]
    const fn decode(value: <UInt as Packable>::Packed) -> Self {
        let (size, next) = UInt::unpack(value);
        Self { size, next }
    }

    #[inline]
    const fn next_is_tail(&self) -> bool {
        self.next == SENTINEL_OFFSET
    }

    #[inline]
    const fn next_is_removed(&self) -> bool {
        self.next == SEGMENT_NODE_REMOVED
    }

    #[inline]
    const fn is_sentinel(&self) -> bool {
        self.size == SENTINEL_SIZE
    }

    #[inline]
    const fn is_removed(&self) -> bool {
        self.size == SEGMENT_NODE_REMOVED
    }

    #[inline]
    const fn is_empty(&self) -> bool {
        self.is_sentinel() && self.next_is_tail()
    }

    #[inline]
    const fn remove(self) -> Self {
        Self {
            size: SEGMENT_NODE_REMOVED,
            next: self.next,
        }
    }

    #[inline]
    const fn insert(self, next: Segment) -> Self {
        Self {
            size: self.size,
            next: next.node_offset,
        }
    }

    #[inline]
    const fn advance(self, to: Self) -> Self {
        Self {
            size: self.size,
            next: to.next,
        }
    }

    #[inline]
    const fn sentinel() -> Self {
        Self {
            size: SENTINEL_OFFSET,
            next: SENTINEL_OFFSET,
        }
    }
}

#[repr(transparent)]
struct SegmentNode {
    /// The first 32 bits are the size of the memory,
    /// the last 32 bits are the offset of the next segment node.
    node: AtomicPackedUInt,
}

impl core::fmt::Debug for SegmentNode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let data = self.load();
        core::fmt::Debug::fmt(&data, f)
    }
}

impl core::ops::Deref for SegmentNode {
    type Target = AtomicU64;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.node
    }
}

impl SegmentNode {
    #[inline]
    const fn sentinel() -> Self {
        Self {
            node: AtomicU64::new(SegmentNodeData::sentinel().encode()),
        }
    }

    #[inline]
    fn load_word(&self) -> u64 {
        self.node.load(Ordering::Acquire)
    }

    #[inline]
    fn load(&self) -> SegmentNodeData {
        SegmentNodeData::decode(self.load_word())
    }

    #[inline]
    fn store(&self, data: SegmentNodeData) {
        self.node.store(data.encode(), Ordering::Release);
    }

    #[inline]
    fn compare_exchange_data(
        &self,
        current: SegmentNodeData,
        new: SegmentNodeData,
        success: Ordering,
        failure: Ordering,
    ) -> Result<SegmentNodeData, SegmentNodeData> {
        let current = current.encode();
        let new = new.encode();
        match self.node.compare_exchange(current, new, success, failure) {
            Ok(word) => Ok(SegmentNodeData::from(word)),
            Err(word) => Err(SegmentNodeData::from(word)),
        }
    }

    #[inline]
    fn remove(&self) -> Result<SegmentNodeData, SegmentNodeData> {
        let cur = self.load();
        if cur.is_removed() {
            return Err(cur);
        }

        let removed = cur.remove();
        match self.compare_exchange_data(cur, removed, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(old) => Ok(old),
            Err(new) => Err(new),
        }
    }

    #[inline]
    fn insert(&self, next: Segment) -> Result<(), SegmentNodeData> {
        let cur = self.load();
        if cur.next == next.node_offset {
            // we are already linked to the next node.
            return Ok(());
        }
        let inserted = cur.insert(next);
        match self.compare_exchange_data(cur, inserted, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => Ok(()),
            Err(word) => Err(word),
        }
    }

    #[inline]
    fn loose_advance(&self, next_to: &SegmentNode) {
        let cur = self.load();
        let next_to = next_to.load();
        let advanced = cur.advance(next_to);
        self.store(advanced);
    }

    #[inline]
    fn advance(&self, next_to: &SegmentNode) -> Result<(), SegmentNodeData> {
        let cur = self.load();
        let next_to = next_to.load();
        let advanced = cur.advance(next_to);
        match self.compare_exchange_data(cur, advanced, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => Ok(()),
            Err(word) => Err(word),
        }
    }
}

type SegmentView<'a> = (&'a SegmentNode, SegmentNodeData);
type Prev<'a> = SegmentView<'a>;
type Next<'a> = SegmentView<'a>;
type SegmentViewPair<'a> = (Prev<'a>, Next<'a>);

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct Segment {
    base_ptr: *mut u8,
    node_offset: Offset,
    data_size: Size,
}

impl Segment {
    /// # Safety
    /// - offset must point to a well-aligned and in-bounds SegmentNode
    #[inline]
    const unsafe fn raw(base_ptr: *mut u8, node_offset: Offset, data_size: Size) -> Self {
        Self {
            base_ptr,
            node_offset,
            data_size,
        }
    }

    #[inline]
    const fn node(&self) -> &SegmentNode {
        // Safety: when constructing the Segment, we have checked the ptr_offset is in bounds and well-aligned.
        unsafe {
            let ptr = self.base_ptr.add(self.node_offset.cast_into());
            &*ptr.cast::<SegmentNode>()
        }
    }

    #[inline]
    const fn data_offset(&self) -> Offset {
        self.node_offset + SEGMENT_NODE_SIZE
    }

    #[inline]
    const fn data_ptr(&self) -> *mut u8 {
        unsafe {
            self.base_ptr
                .add((self.node_offset + SEGMENT_NODE_SIZE).cast_into())
        }
    }
}

struct ReqSegment {
    seg: Segment,
    req_size: Size,
}

impl Deref for ReqSegment {
    type Target = Segment;

    fn deref(&self) -> &Self::Target {
        &self.seg
    }
}

trait Strategy: Sized {
    /// Check ordering relation between segment sizes.
    fn order(val: u32, next_node_size: u32) -> bool;
    fn alloc_slow(arena: &Arena<Self>, size: u32) -> Result<Meta, Error>;
}

pub struct Arena<S: Strategy> {
    ptr: *mut u8,
    header: Header,
    magic: u16,
    version: u16,
    read_only: bool,
    max_retries: u16,
    // memory info
    cap: u32,
    reserved: usize,
    data_offset: usize,
    strategy: S,
}

struct Pessimistic;
struct Optimistic;

type OArena = Arena<Pessimistic>;
type PArena = Arena<Optimistic>;

impl Strategy for Optimistic {
    #[inline]
    fn order(val: u32, next_node_size: u32) -> bool {
        val >= next_node_size
    }

    fn alloc_slow(arena: &Arena<Self>, size: u32) -> Result<Meta, Error> {
        let cur = &arena.header().sentinel;
        arena.alloc_slow_by(&cur, size)
    }
}

impl Strategy for Pessimistic {
    #[inline]
    fn order(val: u32, next_node_size: u32) -> bool {
        val <= next_node_size
    }

    fn alloc_slow(arena: &Arena<Self>, size: u32) -> Result<Meta, Error> {
        let Some(cur) = arena.find_by(size).ok().map(|(cur, _)| cur) else {
            return Err(Error::InsufficientSpace {
                requested: size,
                available: arena.remaining() as Size,
            });
        };

        arena.alloc_slow_by(&cur, size)
    }
}

impl<S: Strategy> Arena<S> {
    fn header(&self) -> &Header {
        &self.header
    }

    #[inline]
    fn meta(&self, seg: ReqSegment) -> Meta {
        let mut meta = Meta::raw(self.ptr.cast(), seg.node_offset, seg.data_size);
        meta.view.start_offset = seg.data_offset();
        meta.view.size = seg.req_size;
        unsafe {
            meta.clear(self);
        }
        meta
    }

    #[inline]
    unsafe fn raw_segment(&self, offset: Offset, size: Size) -> Segment {
        // Safety: when constructing the Segment, we have checked the ptr_offset is in bounds and well-aligned.
        unsafe { Segment::raw(self.ptr, offset, size) }
    }

    #[inline]
    unsafe fn segment_from_pair(&self, prev: SegmentNodeData, cur: SegmentNodeData) -> Segment {
        unsafe { self.raw_segment(prev.next, cur.size) }
    }

    #[inline]
    fn segment_req(
        &self,
        prev: &SegmentNode,
        size: Size,
    ) -> Result<Option<(ReqSegment, &SegmentNode)>, ()> {
        let prev_data = prev.load();
        if prev_data.is_empty() {
            return Err(());
        }

        if prev_data.next_is_removed() {
            return Ok(None);
        }

        let cur = self.next_segment_node(prev_data);
        let cur_data = cur.load();
        if cur_data.is_removed() {
            return Ok(None);
        }

        if size > cur_data.size {
            return Err(());
        }

        let seg = ReqSegment {
            seg: unsafe { self.segment_from_pair(prev_data, cur_data) },
            req_size: size,
        };

        Ok(Some((seg, cur)))
    }

    #[inline]
    fn split_segment(&self, segment: &ReqSegment) -> Option<(ReqSegment, Segment)> {
        let req_size = segment.req_size;
        let rem_size = segment.data_size - req_size;
        let rem_offset = segment.data_offset() + req_size;
        // check if the remaining is enough to allocate a new segment.
        if !self.is_valid_segment(rem_offset, rem_size) {
            return None;
        }
        let alloc_seg = unsafe { self.raw_segment(segment.node_offset, req_size) };
        let rem_seg = unsafe { self.raw_segment(rem_offset, rem_size) };
        Some((
            ReqSegment {
                seg: alloc_seg,
                req_size,
            },
            rem_seg,
        ))
    }

    #[inline]
    fn merge_segment(&self, first: &Segment, second: &Segment) -> Option<Segment> {
        if first.data_offset() + first.data_size != second.node_offset {
            return None;
        }
        let merged_size = first.data_size + second.data_size;
        unsafe { Some(self.raw_segment(first.node_offset, merged_size)) }
    }

    #[inline]
    fn recycle_segment(&self, segment: Segment) {
        if self.is_valid_segment(segment.node_offset, segment.data_size) {
            self.g_dealloc(segment.data_offset(), segment.data_size);
        }
    }

    #[inline]
    fn next_segment_node(&self, data: SegmentNodeData) -> &SegmentNode {
        self.segment_node(data.next)
    }

    #[inline]
    fn segment_node(&self, offset: Offset) -> &SegmentNode {
        // Safety: the offset is in bounds and well aligned.
        unsafe {
            let ptr = self.ptr.add(offset as usize);
            &*ptr.cast()
        }
    }

    #[inline]
    fn validate_segment(&self, offset: Offset, size: Size) -> Option<(u32, u32, u32)> {
        if offset == 0 || size == 0 {
            return None;
        }

        let aligned_offset = offset.align_up_of::<SegmentNode>();
        let overhead = offset.align_offset_of::<SegmentNode>() + SEGMENT_NODE_SIZE;
        let available = size.checked_sub(overhead)?;
        if available < self.header().min_segment_size() {
            return None;
        }

        Some((aligned_offset, overhead, available))
    }

    #[inline]
    fn is_valid_segment(&self, offset: Offset, size: Size) -> bool {
        self.validate_segment(offset, size).is_some()
    }

    #[inline]
    fn new_segment(&self, offset: Offset, size: Size) -> Option<Segment> {
        let (aligned_offset, _, available) = self.validate_segment(offset, size)?;
        unsafe { Some(self.raw_segment(aligned_offset, available)) }
    }

    #[inline]
    fn increase_discarded(&self, size: Size) {
        #[cfg(feature = "tracing")]
        tracing::debug!("discard {size} bytes");

        self.header().discarded.fetch_add(size, Ordering::Release);
    }

    // find prev and next node that satisfies the given size check.
    fn find_by(&self, size: Size) -> Result<SegmentView, SegmentView> {
        let header = self.header();
        let mut cur = &header.sentinel;
        let mut cur_data = cur.load();
        let backoff = Backoff::new();

        loop {
            // the list is empty
            if cur_data.is_empty() {
                return Err((cur, cur_data));
            }

            if cur_data.is_removed() {
                if cur_data.next_is_tail() {
                    return Err((cur, cur_data));
                }
                cur = self.next_segment_node(cur_data);
                cur_data = cur.load();
                continue;
            }

            // the next is the tail, then we should insert the value after the current node.
            if cur_data.next_is_tail() {
                return Err((cur, cur_data));
            }

            let next = self.next_segment_node(cur_data);
            let next_data = next.load();
            if next_data.is_removed() {
                backoff.snooze();
                continue;
            }

            if S::order(size, next_data.size) {
                let re_cur = cur.load();
                if re_cur.is_removed() {
                    backoff.snooze();
                    cur = &header.sentinel;
                    cur_data = cur.load();
                    continue;
                }
                return Ok((cur, cur_data));
            }

            cur = next;
            cur_data = next_data;
        }
    }

    /// Returns the free list position to insert the value.
    /// - `None` means that we should insert to the head.
    /// - `Some(offset)` means that we should insert after the offset. offset -> new -> next
    // fn find_pos(&self, size: Size) -> Result<SegmentView, SegmentView> {
    //     match self.find_by(size) {
    //         Ok((cur_view, _)) => Ok(cur_view),
    //         Err(cur_view) => Err(cur_view),
    //     }
    // }

    fn discard_freelist_in(&self) -> u32 {
        let header = self.header();
        let backoff = Backoff::new();

        let mut discarded = 0;
        loop {
            let setinel_data = header.load();

            // free list is empty
            if setinel_data.is_empty() {
                return discarded;
            }

            if setinel_data.next_is_removed() {
                // the head node is marked as removed, wait other thread to make progress.
                backoff.snooze();
                continue;
            }

            let head = self.next_segment_node(setinel_data);

            let Ok(old_head_data) = head.remove() else {
                backoff.snooze();
                continue;
            };

            match header.sentinel.advance(head) {
                Ok(_) => {
                    self.increase_discarded(old_head_data.size);
                    discarded += old_head_data.size;
                    continue;
                }
                Err(cur) => {
                    if cur.is_removed() {
                        // The current head is removed from the list, wait other thread to make progress.
                        backoff.snooze();
                    } else {
                        backoff.spin();
                    }
                }
            }
        }
    }

    fn g_dealloc(&self, offset: u32, size: u32) -> bool {
        // check if we have enough space to allocate a new segment in this segment.
        let Some(segment) = self.new_segment(offset, size) else {
            return false;
        };

        let backoff = Backoff::new();

        loop {
            let (cur, cur_data) = match self.find_by(segment.data_size) {
                Ok(res) => res,
                Err(res) => res,
            };

            if cur_data.is_removed() {
                backoff.snooze();
                continue;
            }

            // we found ourselves, then we need to refind the position.
            if segment.node_offset == cur_data.next {
                backoff.snooze();
                continue;
            }

            // segment -> next
            let segment_node = segment.node();
            segment_node.loose_advance(cur);

            match cur.insert(segment) {
                Ok(_) => {
                    #[cfg(feature = "tracing")]
                    tracing::debug!(
                        "create segment node ({} bytes) at {}, next segment {}",
                        cur_data.size,
                        segment.node_offset,
                        cur_data.next,
                    );

                    self.increase_discarded(SEGMENT_NODE_SIZE);
                    return true;
                }
                Err(cur) => {
                    if cur.is_removed() {
                        // wait other thread to make progress.
                        backoff.snooze();
                    } else {
                        backoff.spin();
                    }
                }
            }
        }
    }
    fn alloc_slow_by(&self, cur: &SegmentNode, size: u32) -> Result<Meta, Error> {
        let header = self.header();
        let backoff = Backoff::new();

        loop {
            let (segment, head) = match self.segment_req(cur, size) {
                Err(_) => {
                    return Err(Error::InsufficientSpace {
                        requested: size,
                        available: self.remaining() as Size,
                    });
                }
                Ok(None) => {
                    // the head node is marked as removed, wait other thread to make progress.
                    backoff.snooze();
                    continue;
                }
                Ok(Some(res)) => res,
            };

            if head.remove().is_err() {
                backoff.snooze();
                continue;
            };

            match header.sentinel.advance(head) {
                Ok(_) => {
                    #[cfg(feature = "tracing")]
                    tracing::debug!(
                        "allocate {} bytes at offset {} from segment",
                        size,
                        segment.node_offset
                    );
                    if let Some((alloc_seg, rem_seg)) = self.split_segment(&segment) {
                        self.recycle_segment(rem_seg);
                        return Ok(self.meta(alloc_seg));
                    }
                    return Ok(self.meta(segment));
                }
                Err(cur) => {
                    if cur.is_removed() {
                        // The current head is removed from the list, wait other thread to make progress.
                        backoff.snooze();
                    } else {
                        backoff.spin();
                    }
                    continue;
                }
            }
        }
    }
    fn g_alloc<T>(&self, size: u32) -> Result<Option<Meta>, Error> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        if size == 0 {
            return Ok(None);
        }

        let header = self.header();
        if let Some((want, old)) = header.allocate::<T>(self.cap) {
            #[cfg(feature = "tracing")]
            tracing::debug!("allocate {} bytes at offset {} from arena", size, old);

            let mut meta = Meta::raw(self.ptr.cast(), old, want - old);
            meta.align_to::<T>();
            unsafe {
                meta.clear(self);
            }
            return Ok(Some(meta));
        } else {
            for i in 0..self.max_retries {
                let want = UInt::align_of::<T>() + UInt::size_of::<T>() - 1;

                match S::alloc_slow(self, want) {
                    Ok(m) => return Ok(Some(m)),
                    Err(e) if i + 1 == self.max_retries => return Err(e),
                    Err(e) => { /* retry */ }
                }
            }
            unreachable!()
            // if no retries configured or all retries exhausted, return insufficient space
            // return Err(Error::InsufficientSpace {
            //     requested: size,
            //     available: self.remaining() as Size,
            // });
        }
    }
    fn alloc_bytes_in(&self, size: u32) -> Result<Option<Meta>, Error> {
        self.g_alloc::<()>(size)
    }

    fn alloc_aligned_bytes_in<T>(&self, extra: u32) -> Result<Option<Meta>, Error> {
        let size = UInt::size_of::<T>() + extra;
        self.g_alloc::<T>(size)
    }

    fn alloc_in<T>(&self) -> Result<Option<Meta>, Error> {
        let size = UInt::size_of::<T>();
        self.g_alloc::<T>(size)
    }
}
