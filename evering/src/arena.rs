use core::{
    marker::PhantomData,
    mem::MaybeUninit,
    ops::Deref,
    ptr::NonNull,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

use crossbeam_utils::{Backoff, CachePadded};

use crate::{
    header::{self, Magic},
    mem::{self, AddrSpec, MemBlkLayout, Mmap},
    numeric::{Alignable, CastInto, Packable},
};

pub type UInt = u32;
type PackedUInt = u64;
type AtomicUInt = AtomicU32;
type AtomicPackedUInt = AtomicU64;
type PAtomicUInt = CachePadded<AtomicUInt>;
type PAtomicPackedUInt = CachePadded<AtomicPackedUInt>;

type Offset = UInt;
type Size = UInt;
type AddrSpan = crate::mem::AddrSpan<UInt>;

pub const fn max_bound(n: usize) -> Option<UInt> {
    let n = UInt::try_from(n).ok()?;
    Some(n)
}

pub const fn max_bound_ok(n: usize) -> Result<UInt, Error> {
    max_bound(n).ok_or(Error::OutofBounds { requested: n })
}

pub const fn bound(n: usize, available: UInt) -> Option<UInt> {
    let n = UInt::try_from(n).ok()?;
    if n < available { Some(n) } else { None }
}

pub const ARENA_MAX_CAPACITY: Size = UInt::MAX;
const SENTINEL_OFFSET: Offset = UInt::MAX;
const SENTINEL_SIZE: Size = UInt::MAX;
const SEGMENT_NODE_SIZE: Size = UInt::size_of::<SegmentNode>();
const SEGMENT_NODE_REMOVED: Size = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    ReadOnly,
    OutofBounds {
        requested: usize,
    },
    UnenoughSpace {
        /// The requested size
        requested: Size,
        /// The remaining size
        available: usize,
    },
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ReadOnly => write!(f, "Arena is read-only"),
            Self::UnenoughSpace {
                requested,
                available,
            } => write!(
                f,
                "Allocation failed: requested size is {}, but only {} is available",
                requested, available
            ),
            Self::OutofBounds { requested } => write!(f, "Allocation failed: {}", requested,),
        }
    }
}

/// The metadata of the structs allocated from ARENA.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Meta {
    base_ptr: *const u8,
    raw: AddrSpan,
    view: AddrSpan,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct SpanMeta {
    raw: AddrSpan,
    view: AddrSpan,
}

unsafe impl Send for Meta {}

unsafe impl const mem::Meta for Meta {
    type SpanMeta = SpanMeta;
    #[inline]
    fn null() -> Self {
        Self {
            base_ptr: core::ptr::null(),
            raw: AddrSpan::null(),
            view: AddrSpan::null(),
        }
    }

    #[inline]
    fn is_null(&self) -> bool {
        self.raw.is_null() || self.view.is_null()
    }

    #[inline]
    fn as_uninit<T>(&self) -> NonNull<MaybeUninit<T>> {
        if self.is_null() {
            return NonNull::dangling();
        }
        let ptr = unsafe { self.view.as_ptr(self.base_ptr) };
        // memory allocated while it may be uninitiated.
        unsafe { NonNull::new_unchecked(ptr as *mut _) }
    }

    #[inline]
    fn erase(self) -> Self::SpanMeta {
        SpanMeta {
            raw: self.raw,
            view: self.view,
        }
    }

    #[inline]
    unsafe fn recall(span: Self::SpanMeta, base_ptr: *const u8) -> Self {
        Meta {
            base_ptr,
            raw: span.raw,
            view: span.view,
        }
    }
}

unsafe impl const mem::Span for SpanMeta {
    fn null() -> Self {
        Self {
            raw: AddrSpan::null(),
            view: AddrSpan::null(),
        }
    }

    fn is_null(&self) -> bool {
        self.raw.is_null() || self.view.is_null()
    }
}

impl SpanMeta {
    #[inline]
    const fn raw(raw_offset: Offset, raw_size: Size) -> Self {
        Self {
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
    pub const unsafe fn resolve(self, base_ptr: *const u8) -> Meta {
        Meta {
            base_ptr,
            raw: self.raw,
            view: self.view,
        }
    }
}

impl Meta {
    #[inline]
    pub const fn forget(self) -> SpanMeta {
        SpanMeta {
            raw: self.raw,
            view: self.view,
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
    const fn from_req_seg(base_ptr: *const u8, req_seg: ReqSegment) -> Self {
        Self {
            base_ptr,
            raw: AddrSpan {
                start_offset: req_seg.seg.node_offset,
                size: req_seg.seg.data_size,
            },
            view: AddrSpan {
                start_offset: req_seg.seg.data_offset(),
                size: req_seg.req_size,
            },
        }
    }

    #[inline]
    unsafe fn clear(&self) {
        const NULL: u8 = 0;
        unsafe {
            let ptr = self.view.as_ptr(self.base_ptr).cast_mut();
            core::ptr::write_bytes(ptr, NULL, self.view.size.cast_into());
        }
    }

    #[inline]
    fn align_of<T>(self) -> Self {
        let mut meta = self;
        let aligned = self.raw.align_of::<T>();
        meta.view = aligned;
        meta
    }

    #[inline]
    fn align_to(self, align: UInt) -> Self {
        let mut meta = self;
        let aligned = self.raw.align_to(align);
        meta.view = aligned;
        meta
    }

    #[inline]
    fn align_to_of<T>(self) -> Self {
        let mut meta = self;
        let aligned = self.raw.align_to_of::<T>();
        meta.view = aligned;
        meta
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MetaConfig {
    min_segment_size: Size,
    allocated: Size,
}

impl MetaConfig {
    pub const fn default<S: Strategy>() -> Self {
        use core::mem;
        Self {
            min_segment_size: ArenaMeta::<S>::MIN_SEGMENT_SIZE,
            allocated: mem::size_of::<Header<S>>() as Size,
        }
    }

    pub const fn with_min_segment_size(self, min_segment_size: Size) -> Self {
        Self {
            min_segment_size,
            ..self
        }
    }

    pub const fn with_allocated(self, allocated: Size) -> Self {
        Self { allocated, ..self }
    }
}

#[repr(C)]
pub struct ArenaMeta<S: Strategy> {
    sentinel: SegmentNode,
    // hot path
    allocated: PAtomicUInt,
    discarded: PAtomicUInt,
    min_segment_size: AtomicUInt,
    strategy: PhantomData<S>,
}

pub type Header<S> = header::Header<ArenaMeta<S>>;
pub type MemHeader<S, A, M> = mem::MemRef<Header<S>, A, M>;

impl<S: Strategy> core::fmt::Debug for ArenaMeta<S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let allocated = self.allocated.load(Ordering::Relaxed);
        let discarded = self.discarded.load(Ordering::Relaxed);
        f.debug_struct("Arena Header")
            .field("allocated", &allocated)
            .field("discarded", &discarded)
            .finish()
    }
}

impl<S: Strategy> header::Layout for ArenaMeta<S> {
    const MAGIC: Magic = 0xABCD;
    type Config = MetaConfig;

    #[inline]
    fn init(&mut self, conf: Self::Config) -> header::Status {
        let data = Self::from_config(conf);
        let ptr = self as *mut Self;
        unsafe { ptr.write(data) };
        header::Status::Initialized
    }

    #[inline]
    fn attach(&self) -> header::Status {
        header::Status::Initialized
    }

    #[inline]
    fn finalize(&self) -> bool {
        true
    }
}

impl<S: Strategy> ArenaMeta<S> {
    pub const MIN_SEGMENT_SIZE: Size = 20;

    #[inline]
    const fn from_config(conf: MetaConfig) -> Self {
        let MetaConfig {
            min_segment_size,
            allocated,
        } = conf;
        Self::new(min_segment_size, allocated)
    }

    #[inline]
    const fn new(min_segment_size: Size, allocated: Size) -> Self {
        Self {
            sentinel: SegmentNode::sentinel(),
            allocated: CachePadded::new(AtomicUInt::new(allocated)),
            discarded: CachePadded::new(AtomicUInt::new(0)),
            min_segment_size: AtomicUInt::new(min_segment_size),
            strategy: PhantomData,
        }
    }

    #[inline]
    fn load(&self) -> SegmentNodeData {
        self.sentinel.load()
    }

    #[inline]
    fn alloc_fast<H: const Deref<Target = Header<S>>>(
        &self,
        a: &Arena<H, S>,
        size: UInt,
        align: UInt,
    ) -> Option<Meta> {
        let mut allocated = self.allocated();
        loop {
            let want = allocated.align_up(align) + size;
            if want > a.size {
                break None;
            }

            match self.allocated.compare_exchange_weak(
                allocated,
                want,
                Ordering::SeqCst,
                Ordering::Acquire,
            ) {
                Ok(allocated) => {
                    break {
                        let raw_size = want - allocated;

                        #[cfg(feature = "tracing")]
                        tracing::debug!(
                            "allocate {} bytes at offset {} from arena",
                            raw_size,
                            allocated
                        );
                        let meta = Meta::raw(a.base_ptr(), allocated, raw_size).align_to(align);
                        // unsafe { meta.clear(a) }
                        Some(meta)
                    };
                }
                Err(changed) => {
                    allocated = changed;
                    continue;
                }
            }
        }
    }

    #[inline]
    // remove the last node: `offset+size` or return false
    fn dealloc_last(&self, offset: Offset, size: Size) -> bool {
        if self
            .allocated
            .compare_exchange(offset + size, offset, Ordering::SeqCst, Ordering::Relaxed)
            .is_ok()
        {
            return true;
        }

        false
    }

    #[inline]
    pub fn allocated(&self) -> Size {
        self.allocated.load(Ordering::Acquire)
    }

    #[inline]
    pub fn discarded(&self) -> Size {
        self.discarded.load(Ordering::Acquire)
    }

    #[inline]
    fn incre_discarded(&self, size: Size) {
        #[cfg(feature = "tracing")]
        tracing::debug!("discard {size} bytes");
        self.discarded.fetch_add(size, Ordering::Release);
    }

    #[inline]
    pub fn min_segment_size(&self) -> Size {
        self.min_segment_size.load(Ordering::Acquire)
    }

    #[inline]
    fn with_min_segment_size(&self, size: Size) {
        self.min_segment_size.store(size, Ordering::Release);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SegmentNodeData {
    size: Size,
    next: Offset,
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
    fn load_word(&self) -> PackedUInt {
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
            Ok(word) => Ok(SegmentNodeData::decode(word)),
            Err(word) => Err(SegmentNodeData::decode(word)),
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
        // prev(self) -> next(new segment)
        next.init_node(self);
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

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct Segment {
    base_ptr: *const u8,
    node_offset: Offset,
    data_size: Size,
}

impl Segment {
    /// # Safety
    /// - offset must point to a well-aligned and in-bounds SegmentNode
    #[inline]
    const unsafe fn raw(base_ptr: *const u8, node_offset: Offset, data_size: Size) -> Self {
        Self {
            base_ptr,
            node_offset,
            data_size,
        }
    }

    #[inline]
    const fn new(
        base_ptr: *const u8,
        offset: Offset,
        size: Size,
        min_data_size: Size,
    ) -> Option<Self> {
        let (aligned_offset, _, available) = Self::validate(offset, size, min_data_size)?;
        unsafe { Some(Self::raw(base_ptr, aligned_offset, available)) }
    }

    #[inline]
    const fn data_offset(&self) -> Offset {
        self.node_offset + SEGMENT_NODE_SIZE
    }

    #[inline]
    const fn end_offset(&self) -> Offset {
        self.data_offset() + self.data_size
    }

    #[inline]
    const fn size(&self) -> Size {
        self.data_size + SEGMENT_NODE_SIZE
    }

    #[inline]
    const fn data_ptr(&self) -> *const u8 {
        unsafe {
            self.base_ptr
                .add((self.node_offset + SEGMENT_NODE_SIZE).cast_into())
        }
    }

    #[inline]
    const fn validate(
        offset: Offset,
        size: Size,
        min_data_size: Size,
    ) -> Option<(Offset, Size, Size)> {
        let aligned_offset = offset.align_up_of::<SegmentNode>();
        let overhead = offset.align_offset_of::<SegmentNode>() + SEGMENT_NODE_SIZE;
        let available = size.checked_sub(overhead)?;
        if available < min_data_size {
            return None;
        }

        Some((aligned_offset, overhead, available))
    }

    #[inline]
    const fn is_valid(offset: Offset, size: Size, min_data_size: Size) -> bool {
        if offset == 0 || size == 0 {
            return false;
        }

        Self::validate(offset, size, min_data_size).is_some()
    }

    #[inline]
    fn init_node(&self, prev: &SegmentNode) {
        // Safety: when constructing the Segment, we have checked the ptr_offset is in bounds and well-aligned.
        unsafe {
            let ptr = self.base_ptr.add(self.node_offset.cast_into());
            let node = &*ptr.cast::<SegmentNode>();
            let prev_data = prev.load();
            let node_data = SegmentNodeData {
                size: self.data_size,
                next: prev_data.next,
            };
            node.store(node_data);
        }
    }
}

struct ReqSegment {
    seg: Segment,
    req_size: Size,
}

impl const Deref for ReqSegment {
    type Target = Segment;

    fn deref(&self) -> &Self::Target {
        &self.seg
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    read_only: bool,
    max_retries: u32,
}

impl Config {
    const MAX_RETRIES: u32 = 5;

    pub const fn default() -> Self {
        Self {
            read_only: true,
            max_retries: Self::MAX_RETRIES,
        }
    }

    pub const fn with_read_only(self, read_only: bool) -> Self {
        Self { read_only, ..self }
    }

    pub const fn with_max_retries(self, retries: u32) -> Self {
        Self {
            max_retries: retries,
            ..self
        }
    }
}

#[derive(Debug)]
pub struct Arena<H: const Deref<Target = Header<S>>, S: Strategy> {
    header: H,
    size: Size,
    read_only: bool,
    max_retries: u32,
}

pub type ArenaRef<'a, S> = Arena<&'a Header<S>, S>;
pub type MemArena<S, A, M> = Arena<MemHeader<S, A, M>, S>;

pub struct Pessimistic;
pub struct Optimistic;

pub trait Strategy: Sized {
    /// Check ordering relation between segment sizes.
    fn order(val: Size, next_node_size: Size) -> bool;
    fn alloc_slow<H: const Deref<Target = Header<Self>>>(
        arena: &Arena<H, Self>,
        size: Size,
        align: Offset,
    ) -> Result<Meta, Error>;
}

impl Strategy for Optimistic {
    #[inline]
    fn order(val: Size, next_node_size: Size) -> bool {
        val >= next_node_size
    }

    fn alloc_slow<H: const Deref<Target = Header<Optimistic>>>(
        arena: &Arena<H, Self>,
        size: Size,
        align: Offset,
    ) -> Result<Meta, Error> {
        let cur = &arena.header().sentinel;
        arena.alloc_slow_by(cur, size, align)
    }
}

impl Strategy for Pessimistic {
    #[inline]
    fn order(val: Size, next_node_size: Size) -> bool {
        val <= next_node_size
    }

    fn alloc_slow<H: const Deref<Target = Header<Pessimistic>>>(
        arena: &Arena<H, Self>,
        size: Size,
        align: Offset,
    ) -> Result<Meta, Error> {
        use mem::MemAllocInfo;
        let Some(cur) = arena.find_by(size).ok().map(|(cur, _)| cur) else {
            return Err(Error::UnenoughSpace {
                requested: size,
                available: arena.remained(),
            });
        };

        arena.alloc_slow_by(cur, size, align)
    }
}

unsafe impl<H: const Deref<Target = Header<S>>, S: Strategy> Send for Arena<H, S> {}
unsafe impl<H: const Deref<Target = Header<S>>, S: Strategy> Sync for Arena<H, S> {}

impl<H: const Deref<Target = Header<S>> + Clone, S: Strategy> Clone for Arena<H, S> {
    fn clone(&self) -> Self {
        Self {
            header: self.header.clone(),
            size: self.size,
            read_only: self.read_only,
            max_retries: self.max_retries,
        }
    }
}

unsafe impl<H: const Deref<Target = Header<S>>, S: Strategy> mem::MemAlloc for Arena<H, S> {
    type Meta = Meta;
    type Error = Error;

    #[inline]
    fn base_ptr(&self) -> *const u8 {
        self.base_ptr()
    }

    #[inline]
    fn malloc_by(&self, layout: core::alloc::Layout) -> Result<Meta, Error> {
        let size = layout.size();
        let size = max_bound_ok(size)?;
        let align = layout.align();
        let align = max_bound_ok(align)?;

        self.alloc(size, align)
    }
}

unsafe impl<H: const Deref<Target = Header<S>>, S: Strategy> mem::MemDealloc for Arena<H, S> {
    fn demalloc(&self, meta: Meta) -> bool {
        self.dealloc(meta)
    }
}

impl<H: const Deref<Target = Header<S>>, S: Strategy> mem::MemAllocator for Arena<H, S> {}

impl<H: const Deref<Target = Header<S>>, S: Strategy> mem::MemAllocInfo for Arena<H, S> {
    fn allocated(&self) -> usize {
        self.header().allocated().cast_into()
    }

    fn remained(&self) -> usize {
        self.size
            .saturating_sub(self.header().allocated())
            .cast_into()
    }

    fn discarded(&self) -> usize {
        self.header().discarded().cast_into()
    }
}

impl<H: const Deref<Target = Header<S>>, S: Strategy> Arena<H, S> {
    #[inline]
    pub const fn header(&self) -> &Header<S> {
        &self.header
    }

    #[inline]
    pub const fn from_conf(header: H, size: Size, conf: Config) -> Self {
        let Config {
            max_retries,
            read_only,
        } = conf;

        Self {
            header,
            size,
            read_only,
            max_retries,
        }
    }
}

impl<S: Strategy, A: AddrSpec, M: Mmap<A>> MemArena<S, A, M> {
    #[inline]
    pub fn as_ref(&self) -> ArenaRef<'_, S> {
        ArenaRef {
            header: self.header(),
            size: self.size,
            max_retries: self.max_retries,
            read_only: self.read_only,
        }
    }

    #[inline]
    pub fn from_layout(area: MemBlkLayout<A, M>, conf: Config) -> Result<Self, mem::Error<A, M>> {
        use mem::MemBlkOps;
        let mut area = area;
        let offset = area.offset();
        let mconf = MetaConfig::default::<S>();
        let ptr = area.push::<Header<S>>(mconf)?;
        let (area, _) = area.finish();

        let size = area.size() - offset;
        let size = max_bound(size).ok_or(mem::Error::OutofSize {
            requested: size,
            bound: ARENA_MAX_CAPACITY.cast_into(),
        })?;

        let header = unsafe { MemHeader::from_raw(area.into(), ptr) };
        Ok(Self::from_conf(header, size, conf))
    }
}

impl<H: const Deref<Target = Header<S>>, S: Strategy> Arena<H, S> {
    #[inline]
    const fn base_ptr(&self) -> *const u8 {
        (self.header() as *const Header<S>).cast()
    }

    #[inline]
    fn meta(&self, seg: ReqSegment) -> Meta {
        Meta::from_req_seg(self.base_ptr(), seg)
        // unsafe { meta.clear(self) }
    }

    #[inline]
    fn new_segment(&self, offset: Offset, size: Size) -> Option<Segment> {
        if offset == 0 || size == 0 {
            return None;
        }
        Segment::new(
            self.base_ptr(),
            offset,
            size,
            self.header().min_segment_size(),
        )
    }

    #[inline]
    unsafe fn raw_segment(&self, offset: Offset, data_size: Size) -> Segment {
        // Safety: when constructing the Segment, we have checked the ptr_offset is in bounds and well-aligned.
        unsafe { Segment::raw(self.base_ptr(), offset, data_size) }
    }

    #[inline]
    unsafe fn segment_from_pair(&self, prev: SegmentNodeData, cur: SegmentNodeData) -> Segment {
        unsafe { self.raw_segment(prev.next, cur.size) }
    }

    #[inline]
    fn split_segment(&self, segment: &ReqSegment) -> Option<(ReqSegment, Segment)> {
        let req_size = segment.req_size;
        let rem_size = segment.data_size - req_size;
        let rem_offset = segment.data_offset() + req_size;
        let rem_segment = self.new_segment(rem_offset, rem_size)?;
        let alloc_segment = unsafe { self.raw_segment(segment.node_offset, req_size) };
        Some((
            ReqSegment {
                seg: alloc_segment,
                req_size,
            },
            rem_segment,
        ))
    }

    #[inline]
    fn merge_segment(&self, prev: &Segment, next: &Segment) -> Option<Segment> {
        if prev.end_offset() != next.node_offset {
            return None;
        }
        let merged_data_size = prev.data_size + next.size();
        unsafe { Some(self.raw_segment(prev.node_offset, merged_data_size)) }
    }

    #[inline]
    fn recycle_segment(&self, segment: Segment) {
        unsafe { self.dealloc_by(segment.data_offset(), segment.data_size) };
    }

    #[inline]
    fn next_segment_node(&self, data: SegmentNodeData) -> &SegmentNode {
        self.segment_node(data.next)
    }

    #[inline]
    fn segment_node(&self, offset: Offset) -> &SegmentNode {
        // Safety: the offset is in bounds and well aligned.
        unsafe {
            let ptr = self.base_ptr().add(offset.cast_into());
            &*ptr.cast()
        }
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

    // find prev and next node that satisfies the given size check.
    fn find_by(&self, size: Size) -> Result<SegmentView<'_>, SegmentView<'_>> {
        let backoff = Backoff::new();

        let header = self.header();
        let mut cur = &header.sentinel;
        let mut cur_data = cur.load();

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

    fn discard(&self) -> Result<Size, Error> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }

        Ok(self.discard_in())
    }

    fn discard_in(&self) -> Size {
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
                    header.incre_discarded(old_head_data.size);
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

    pub fn dealloc(&self, meta: Meta) -> bool {
        unsafe { self.dealloc_by(meta.raw.start_offset, meta.raw.size) }
    }

    unsafe fn dealloc_by(&self, offset: UInt, size: UInt) -> bool {
        let header = self.header();
        if header.dealloc_last(offset, size) {
            return true;
        }

        // enough space to initiate segment?
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

            // found original node, then we need to refind the position.
            if segment.node_offset == cur_data.next {
                backoff.snooze();
                continue;
            }

            match cur.insert(segment) {
                Ok(_) => {
                    #[cfg(feature = "tracing")]
                    tracing::debug!(
                        "create segment node ({} bytes) at {}, next segment {}",
                        cur_data.size,
                        segment.node_offset,
                        cur_data.next,
                    );

                    header.incre_discarded(SEGMENT_NODE_SIZE);
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
    fn alloc_slow_by(&self, cur: &SegmentNode, size: Size, align: Offset) -> Result<Meta, Error> {
        use mem::MemAllocInfo;

        let backoff = Backoff::new();

        let want = size + align - 1;
        loop {
            let (cur_seg, next) = match self.segment_req(cur, want) {
                Err(_) => {
                    return Err(Error::UnenoughSpace {
                        requested: size,
                        available: self.remained(),
                    });
                }
                Ok(None) => {
                    // the head node is marked as removed, wait other thread to make progress.
                    backoff.snooze();
                    continue;
                }
                Ok(Some(res)) => res,
            };

            if next.remove().is_err() {
                backoff.snooze();
                continue;
            };

            match cur.advance(next) {
                Ok(_) => {
                    #[cfg(feature = "tracing")]
                    tracing::debug!(
                        "allocate {} bytes at offset {} from segment",
                        size,
                        cur_seg.node_offset
                    );
                    // `want = size + align - 1 -> offset.align_up(align)`
                    if let Some((alloc_seg, rem_seg)) = self.split_segment(&cur_seg) {
                        self.recycle_segment(rem_seg);
                        return Ok(self.meta(alloc_seg).align_to(align));
                    }
                    return Ok(self.meta(cur_seg).align_to(align));
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

    pub fn alloc(&self, size: UInt, align: UInt) -> Result<Meta, Error> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        if size == 0 {
            use mem::Meta;
            return Ok(Meta::null());
        }

        let header = self.header();
        if let Some(meta) = header.alloc_fast(self, size, align) {
            Ok(meta)
        } else {
            for i in 0..self.max_retries {
                match S::alloc_slow(self, size, align) {
                    Ok(m) => return Ok(m),
                    Err(e) if i + 1 == self.max_retries => return Err(e),
                    Err(_) => { /* retry */ }
                }
            }
            unreachable!()
        }
    }
}
