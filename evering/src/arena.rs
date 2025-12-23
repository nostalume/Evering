use core::{
    alloc,
    marker::PhantomData,
    ops::Deref,
    ptr::NonNull,
    sync::atomic::{AtomicU64, Ordering},
};

use crossbeam_utils::{Backoff, CachePadded};

use crate::{
    header::{self, Magic},
    mem::{self, AddrSpec, MapLayout, Mmap},
    numeric::{self, Alignable, CastInto, Measurable, Packable},
};

type UInt = u32;
type AtomicUInt = numeric::Atomic<UInt>;

type PackedUInt = numeric::Pack<UInt>;
type AtomicPackedUInt = numeric::AtomicPack<UInt>;

type PAtomicUInt = CachePadded<AtomicUInt>;
type PAtomicPackedUInt = CachePadded<AtomicPackedUInt>;

type Offset = UInt;
type Size = UInt;
type AddrSpan = crate::mem::AddrSpan<UInt>;

#[inline]
pub const fn bound(n: usize, bound: UInt) -> UInt {
    n.min(bound.cast_into()) as UInt
}

#[inline]
pub const fn cap_bound(n: usize) -> UInt {
    bound(n, ARENA_MAX_CAPACITY)
}

#[inline]
pub const fn bound_ok(n: usize, bound: UInt) -> Result<UInt, Error> {
    if n > bound as usize {
        Err(Error::OutofBounds { requested: n })
    } else {
        Ok(n as UInt)
    }
}

#[inline]
pub const fn cap_bound_ok(n: usize) -> Result<UInt, Error> {
    bound_ok(n, ARENA_MAX_CAPACITY)
}

const ARENA_MAX_CAPACITY: Size = UInt::MAX;
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
    raw: AddrSpan,
    view: AddrSpan,
}

unsafe impl Send for Meta {}

impl mem::Meta for Meta {
    #[inline]
    fn null() -> Self {
        Self {
            raw: AddrSpan::null(),
            view: AddrSpan::null(),
        }
    }

    #[inline]
    fn is_null(&self) -> bool {
        self.raw.is_null() || self.view.is_null()
    }

    #[inline]
    unsafe fn recall(&self, base_ptr: *const u8) -> NonNull<u8> {
        unsafe { self.view.as_nonnull(base_ptr) }
    }

    #[inline]
    fn layout_bytes(&self) -> alloc::Layout {
        unsafe {
            alloc::Layout::from_size_align_unchecked(
                self.raw.size.cast_into(),
                core::mem::align_of::<u8>(),
            )
        }
    }
}

impl Meta {
    #[inline]
    const fn from_raw(raw_offset: Offset, raw_size: Size) -> Self {
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
    const fn from_req_seg(req_seg: ReqSegment) -> Self {
        Self {
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
    fn align_to(self, align: UInt) -> Self {
        let mut meta = self;
        let aligned = self.raw.align_to(align);
        meta.view = aligned;
        meta
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MetaConfig {
    min_data_size: Size,
    forward: Size,
}

impl MetaConfig {
    pub const fn default<S: Strategy>() -> Self {
        Self {
            min_data_size: ArenaMeta::<S>::MIN_DATA_SIZE,
            forward: 0,
        }
    }

    pub const fn with_min_data_size(self, min_data_size: Size) -> Self {
        Self {
            min_data_size,
            ..self
        }
    }

    pub const fn with_forward(self, foward: Size) -> Self {
        Self {
            forward: foward,
            ..self
        }
    }
}

#[repr(C)]
pub struct ArenaMeta<S: Strategy> {
    sentinel: SegmentNode,
    allocated: PAtomicUInt,
    discarded: PAtomicUInt,
    min_data_size: AtomicUInt,
    strategy: PhantomData<S>,
}

pub type Header<S> = header::Header<ArenaMeta<S>>;
pub type MapHeader<S, A, M> = mem::MapHandle<Header<S>, A, M>;

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
        let ptr = self as *mut Self;
        let data = Self::from_config(conf);
        unsafe { ptr.write(data) };
        header::Status::Initialized
    }

    #[inline]
    fn attach(&self) -> header::Status {
        header::Status::Initialized
    }
}

impl<S: Strategy> ArenaMeta<S> {
    pub const MIN_DATA_SIZE: Size = 20;

    #[inline]
    const fn from_config(conf: MetaConfig) -> Self {
        let MetaConfig {
            min_data_size,
            forward,
        } = conf;

        let allocated = UInt::size_of::<Header<S>>() + forward;
        Self::new(min_data_size, allocated)
    }

    #[inline]
    const fn new(min_segment_size: Size, allocated: Size) -> Self {
        Self {
            sentinel: SegmentNode::sentinel(),
            allocated: CachePadded::new(AtomicUInt::new(allocated)),
            discarded: CachePadded::new(AtomicUInt::new(0)),
            min_data_size: AtomicUInt::new(min_segment_size),
            strategy: PhantomData,
        }
    }

    #[inline]
    fn load(&self) -> SegmentNodeData {
        self.sentinel.load()
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
        tracing::debug!("[Arena]: discard {size} bytes");
        self.discarded.fetch_add(size, Ordering::Release);
    }

    #[inline]
    pub fn min_data_size(&self) -> Size {
        self.min_data_size.load(Ordering::Acquire)
    }

    #[inline]
    fn with_min_data_size(&self, size: Size) {
        self.min_data_size.store(size, Ordering::Release);
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

    /// Mark `self` as `Removed` by notating `size = SEGMENT_NODE_REMOVED`
    #[inline]
    const fn remove(self) -> Self {
        Self {
            size: SEGMENT_NODE_REMOVED,
            next: self.next,
        }
    }

    /// Insert a new node next to `self` by notate `self.next = new_segment.node_offset`
    #[inline]
    const fn insert(self, next: Segment) -> Self {
        Self {
            size: self.size,
            next: next.node_offset,
        }
    }

    /// Skip a node to its next node for `self` by notate `self.next = this.next`
    #[inline]
    const fn advance(self, this: Self) -> Self {
        Self {
            size: self.size,
            next: this.next,
        }
    }

    /// Merge a continguous node.
    #[inline]
    const unsafe fn merge(self, this: Self, size: Offset) -> Self {
        Self {
            size,
            next: this.next,
        }
    }

    /// Mark as sentinel node.
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
        let data = SegmentNodeData::decode(self.node.load(Ordering::Relaxed));
        core::fmt::Debug::fmt(&data, f)
    }
}

impl const core::ops::Deref for SegmentNode {
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
    unsafe fn offset(&self, base_ptr: *const u8) -> Offset {
        ((self as *const Self).addr() - base_ptr.addr()) as Offset
    }

    #[inline]
    fn compare_exchange_data(
        &self,
        current: SegmentNodeData,
        new: SegmentNodeData,
        success: Ordering,
        failure: Ordering,
    ) -> Result<SegmentNodeData, SegmentNodeData> {
        match self
            .node
            .compare_exchange(current.encode(), new.encode(), success, failure)
        {
            Ok(word) => Ok(SegmentNodeData::decode(word)),
            Err(word) => Err(SegmentNodeData::decode(word)),
        }
    }

    /// Remove `self` by notate as `Removed`.
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

    /// Insert a new `SegmentNode` next to `self` by `self.next = next_segment.node_offset`.
    #[inline]
    fn insert(&self, next: Segment) -> Result<(), SegmentNodeData> {
        // [self] -> [next]
        next.init_node(self);
        let cur = self.load();
        if cur.next == next.node_offset {
            // we are already linked to the next node.
            return Ok(());
        }
        let inserted = cur.insert(next);
        match self.compare_exchange_data(cur, inserted, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => {
                #[cfg(feature = "tracing")]
                tracing::debug!("[Arena]: create segment node: {:?}", next);
                Ok(())
            }
            Err(word) => Err(word),
        }
    }

    /// Skip a `SegmentNode` related to `self` by `self.next = this.next`.
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

    #[inline]
    fn merge(&self, next: &SegmentNode, size: Size) -> Result<SegmentNodeData, SegmentNodeData> {
        let cur = self.load();
        let next_data = next.load();
        let merged = unsafe { cur.merge(next_data, size) };
        // if next.remove().is_err() {
        //     return Err(cur);
        // }
        match self.compare_exchange_data(cur, merged, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => Ok(merged),
            Err(word) => Err(word),
        }
    }
}

type SegmentView<'a> = (&'a SegmentNode, SegmentNodeData);

#[derive(Copy, Clone, PartialEq, Eq)]
struct Segment {
    base_ptr: *const u8,
    node_offset: Offset,
    data_size: Size,
}

impl core::fmt::Debug for Segment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_sentinel() {
            f.debug_struct("Segment Setinel")
                .field("node_offset", &self.node_offset)
                .finish()
        } else {
            f.debug_struct("Segment")
                .field("node_offset", &self.node_offset)
                .field("data_size", &self.data_size)
                .field("end_offset", &self.end_offset())
                .finish()
        }
    }
}

impl Segment {
    /// # Safety
    /// - offset must point to a well-aligned and in-bounds SegmentNode
    #[inline]
    const unsafe fn from_raw(base_ptr: *const u8, node_offset: Offset, data_size: Size) -> Self {
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
        unsafe { Some(Self::from_raw(base_ptr, aligned_offset, available)) }
    }

    #[inline]
    const fn is_sentinel(&self) -> bool {
        self.data_size == SENTINEL_SIZE
    }

    #[inline]
    const fn data_offset(&self) -> Offset {
        self.node_offset + SEGMENT_NODE_SIZE
    }

    #[inline]
    const fn end_offset(&self) -> Offset {
        self.data_offset().checked_add(self.data_size).unwrap_or(0)
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
    const fn node(&self) -> &SegmentNode {
        // Safety: Segment node offset should be valid in its initiation.
        unsafe {
            &*self
                .base_ptr
                .add(self.node_offset.cast_into())
                .cast::<SegmentNode>()
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
        let data_size = size.checked_sub(overhead)?;
        if data_size < min_data_size {
            return None;
        }

        Some((aligned_offset, overhead, data_size))
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
        let node = self.node();
        let prev_data = prev.load();
        let node_data = SegmentNodeData {
            size: self.data_size,
            next: prev_data.next,
        };
        node.store(node_data);
    }
}

#[derive(Debug, Clone, Copy)]
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
            read_only: false,
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

pub type RefArena<'a, S> = Arena<&'a Header<S>, S>;
pub type MapArena<S, A, M> = Arena<MapHeader<S, A, M>, S>;

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
        val > next_node_size
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
    fn alloc(&self, layout: core::alloc::Layout) -> Result<Meta, Error> {
        let size = cap_bound_ok(layout.size())?;
        let align = cap_bound_ok(layout.align())?;
        self.alloc(size, align)
    }
}

unsafe impl<H: const Deref<Target = Header<S>>, S: Strategy> mem::MemDealloc for Arena<H, S> {
    fn dealloc(&self, meta: Meta, _layout: alloc::Layout) -> bool {
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

impl<S: Strategy, A: AddrSpec, M: Mmap<A>> TryFrom<MapLayout<A, M>> for MapArena<S, A, M> {
    type Error = mem::Error<A, M>;

    fn try_from(area: MapLayout<A, M>) -> Result<Self, Self::Error> {
        Self::from_layout(area, Config::default())
    }
}

impl<S: Strategy, A: AddrSpec, M: Mmap<A>> MapArena<S, A, M> {
    #[inline]
    pub fn as_ref(&self) -> RefArena<'_, S> {
        RefArena {
            header: self.header(),
            size: self.size,
            max_retries: self.max_retries,
            read_only: self.read_only,
        }
    }

    #[inline]
    pub fn from_layout(area: MapLayout<A, M>, conf: Config) -> Result<Self, mem::Error<A, M>> {
        use mem::MemOps;
        let mut area = area;

        let mconf = MetaConfig::default::<S>();
        let reserve = area.reserve::<Header<S>>()?;
        let size = cap_bound(area.size() - area.ptr_offset(&reserve));
        let handle = area.commit(reserve, mconf)?;

        Ok(Self::from_conf(handle, size, conf))
    }
}

impl<H: const Deref<Target = Header<S>>, S: Strategy> Arena<H, S> {
    #[inline]
    const fn base_ptr(&self) -> *const u8 {
        (self.header() as *const Header<S>).cast()
    }

    #[inline]
    fn meta(&self, seg: ReqSegment, align: Offset) -> Meta {
        Meta::from_req_seg(seg).align_to(align)
        // unsafe { meta.clear(self) }
    }

    /// Initiate a new `segment` by normalized `offset` and `size`.
    #[inline]
    fn new_segment(&self, offset: Offset, size: Size) -> Option<Segment> {
        if offset == 0 || size == 0 {
            return None;
        }
        Segment::new(self.base_ptr(), offset, size, self.header().min_data_size())
    }

    /// Initiate a raw new `segment` by given `offset` and `size`.
    #[inline]
    unsafe fn raw_segment(&self, offset: Offset, data_size: Size) -> Segment {
        // Safety: when constructing the Segment, we have checked the ptr_offset is in bounds and well-aligned.
        unsafe { Segment::from_raw(self.base_ptr(), offset, data_size) }
    }

    /// Resolve a segment of a node by its previous `node.next` and its `node.size`.
    #[inline]
    unsafe fn segment_from_pair(&self, prev: SegmentNodeData, cur: SegmentNodeData) -> Segment {
        unsafe { self.raw_segment(prev.next, cur.size) }
    }

    // #[inline]
    // fn segment_of(&self, node: &SegmentNode) -> Segment {
    //     unsafe { self.raw_segment(node.offset(self.base_ptr()), node.load().size) }
    // }

    /// # Safety: the offset is in bounds and well aligned.
    ///
    /// Resolve a segment node by the given offset.
    #[inline]
    unsafe fn raw_segment_node(&self, offset: Offset) -> &SegmentNode {
        unsafe {
            let ptr = self.base_ptr().add(offset.cast_into());
            &*ptr.cast()
        }
    }

    /// Resolve a segment node by its previous `SegmentNodeData`
    #[inline]
    fn next_segment_node(&self, data: SegmentNodeData) -> &SegmentNode {
        unsafe { self.raw_segment_node(data.next) }
    }

    /// Require a segment and resolve its next node by its previous node and size.
    ///
    /// If previous node or its next node is removed, mark as `Ok(None)` for retrying.
    ///
    /// Else return `Err(())`
    #[inline]
    fn req_segment(
        &self,
        prev: &SegmentNode,
        size: Size,
    ) -> Result<Option<(ReqSegment, &SegmentNode)>, ()> {
        let prev_data = prev.load();
        if prev_data.is_empty() {
            return Err(());
        }

        // Removed state indicates retry
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
    fn split_segment(&self, segment: ReqSegment) -> Option<(ReqSegment, Segment)> {
        let req_size = segment.req_size;
        let rem_size = segment.data_size - req_size;
        let rem_offset = (segment.data_offset() + req_size).align_up_of::<SegmentNode>();
        let alloc_segment = unsafe { self.raw_segment(segment.node_offset, req_size) };
        #[cfg(feature = "tracing")]
        tracing::debug!("[Arena]: split: offset {}, size {}", rem_offset, rem_size);
        // unsafe {
        //     let _ = self.dealloc_by(rem_offset, rem_size);
        // };
        let rem_segment = self.new_segment(rem_offset, rem_size)?;

        Some((
            ReqSegment {
                seg: alloc_segment,
                req_size,
            },
            rem_segment,
        ))
    }

    // #[inline]
    // fn merge_segment(&self, prev: &SegmentNode, cur: &SegmentNode) {
    //     let prev_seg = self.segment_of(prev);
    //     let cur_seg = self.segment_of(cur);

    //     #[cfg(feature = "tracing")]
    //     tracing::debug!(
    //         "[Arena]: merge: prev_seg: {:?} [end]: {}, cur_seg: {:?} [node]: {}",
    //         prev_seg,
    //         prev_seg.end_offset().align_up_of::<SegmentNode>(),
    //         cur_seg,
    //         cur_seg.node_offset,
    //     );

    //     let prev_data = prev.load();
    //     if prev_data.is_removed() || prev_data.next_is_removed() {
    //         return;
    //     }

    //     if prev_seg.is_sentinel() {
    //         return;
    //     }

    //     let prev_end = prev_seg.end_offset().align_up_of::<SegmentNode>();
    //     let cur_end = cur_seg.end_offset();
    //     if prev_end != cur_seg.node_offset || prev_end >= self.size {
    //         return;
    //     }

    //     let size = cur_end - prev_seg.data_offset();
    //     if let Ok(merge) = prev.merge(cur, size) {
    //         #[cfg(feature = "tracing")]
    //         tracing::debug!(
    //             "[Arena]: merge checked: prev_seg: {:?} [end]: {}, cur_seg: {:?} [node]: {}, new {:?}",
    //             prev_seg,
    //             prev_end,
    //             cur_seg,
    //             cur_seg.node_offset,
    //             merge
    //         );
    //     }
    // }

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

    // #[inline]
    // fn find_by_offset(&self, seg: Segment) -> Result<SegmentView<'_>, SegmentView<'_>> {
    //     let backoff = Backoff::new();

    //     let header = self.header();
    //     let mut cur = &header.sentinel;
    //     let mut cur_data = cur.load();

    //     loop {
    //         // the list is empty
    //         if cur_data.is_empty() {
    //             return Err((cur, cur_data));
    //         }

    //         if cur_data.is_removed() {
    //             if cur_data.next_is_tail() {
    //                 return Err((cur, cur_data));
    //             }
    //             cur = self.next_segment_node(cur_data);
    //             cur_data = cur.load();
    //             continue;
    //         }

    //         // the next is the tail, then we should insert the value after the current node.
    //         if cur_data.next_is_tail() {
    //             return Err((cur, cur_data));
    //         }

    //         let next = self.next_segment_node(cur_data);
    //         let next_data = next.load();
    //         if next_data.is_removed() {
    //             backoff.snooze();
    //             continue;
    //         }

    //         let cur_seg = self.segment_of(cur);
    //         let next_seg = self.segment_of(next);
    //         let end = cur_seg.end_offset();
    //         if seg.node_offset > end && seg.node_offset < next_seg.node_offset {
    //             let re_cur = cur.load();
    //             if re_cur.is_removed() {
    //                 backoff.snooze();
    //                 cur = &header.sentinel;
    //                 cur_data = cur.load();
    //                 continue;
    //             }
    //             return Ok((cur, cur_data));
    //         }

    //         cur = next;
    //         cur_data = next_data;
    //     }
    // }

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

    /// Deallocate a `meta`
    pub fn dealloc(&self, meta: Meta) -> bool {
        unsafe {
            match self.dealloc_by(meta.raw.start_offset, meta.raw.size) {
                Ok(_) => true,
                Err(Some(_)) => true,
                Err(None) => false,
            }
        }
    }

    #[inline]
    // remove the last node: `offset+size` or return false
    fn dealloc_last(&self, offset: Offset, size: Size) -> bool {
        let Some(new_offset) = offset.checked_add(size) else {
            return false;
        };
        if self
            .header()
            .allocated
            .compare_exchange(new_offset, offset, Ordering::SeqCst, Ordering::Relaxed)
            .is_ok()
        {
            return true;
        }

        false
    }

    /// Deallocate by given `offset` and `size`.
    ///
    /// If deallocate fastly, return `Ok(())`,
    ///
    /// Else if deallocate by creating node, return `Option<Segment>`.
    unsafe fn dealloc_by(&self, offset: UInt, size: UInt) -> Result<(), Option<Segment>> {
        if self.dealloc_last(offset, size) {
            return Ok(());
        }

        // initiate a new segment
        let Some(segment) = self.new_segment(offset, size) else {
            return Err(None);
        };

        let backoff = Backoff::new();

        loop {
            // find the previous node for current segment
            let (prev, prev_data) = match self.find_by(size) {
                Ok(res) => res,
                Err(res) => res,
            };

            if prev_data.is_removed() {
                backoff.snooze();
                continue;
            }

            // found original node, refind again
            if segment.node_offset == prev_data.next {
                backoff.snooze();
                continue;
            }

            match prev.insert(segment) {
                Ok(_) => {
                    let _prev_data = prev.load();
                    #[cfg(feature = "tracing")]
                    tracing::debug!(
                        "[Arena]: new prev: {:?}, with next segment {:?}",
                        prev_data,
                        segment,
                    );
                    //
                    // let _ = self.merge_segment(prev, self.next_segment_node(prev.load()));

                    // TODO: improve this.
                    self.header().incre_discarded(SEGMENT_NODE_SIZE);
                    return Err(Some(segment));
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

    /// Allocate by given `size` and `align`.
    pub fn alloc(&self, size: UInt, align: UInt) -> Result<Meta, Error> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        if size == 0 {
            use mem::Meta;
            return Ok(Meta::null());
        }

        if let Some(meta) = self.alloc_fast(size, align) {
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

    fn alloc_slow_by(&self, prev: &SegmentNode, size: Size, align: Offset) -> Result<Meta, Error> {
        use mem::MemAllocInfo;

        let backoff = Backoff::new();

        let want = size + align - 1;
        loop {
            // require this segment and resolve next node with given constrain.
            let (cur_seg, cur) = match self.req_segment(prev, want) {
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

            if cur.remove().is_err() {
                backoff.snooze();
                continue;
            };

            match prev.advance(cur) {
                Ok(_) => {
                    if let Some((alloc_seg, rem_seg)) = self.split_segment(cur_seg) {
                        rem_seg.init_node(prev);
                        #[cfg(feature = "tracing")]
                        tracing::debug!("[Arena]: allocate as segment with split: {:?}", alloc_seg);
                        return Ok(self.meta(alloc_seg, align));
                    }
                    #[cfg(feature = "tracing")]
                    tracing::debug!("[Arena]: allocate as segment: {:?}", cur_seg);
                    return Ok(self.meta(cur_seg, align));
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

    #[inline]
    fn alloc_fast(&self, size: UInt, align: UInt) -> Option<Meta> {
        let header = self.header();
        let mut allocated = header.allocated();

        loop {
            let want = allocated.align_up(align).checked_add(size)?;
            if want > self.size {
                break None;
            }

            match header.allocated.compare_exchange_weak(
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
                            "[Arena]: fast allocate with offset {}, size {}",
                            allocated,
                            raw_size,
                        );
                        let meta = Meta::from_raw(allocated, raw_size).align_to(align);
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
}
