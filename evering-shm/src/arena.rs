use core::{
    mem, ptr::NonNull, sync::atomic::{AtomicU32, AtomicU64, Ordering}
};

use crate::area::{AddrSpec, MemBlk, Mmap};

const SENTINEL_SEGMENT_NODE_OFFSET: u32 = u32::MAX;
const SENTINEL_SEGMENT_NODE_SIZE: u32 = u32::MAX;
const SEGMENT_NODE_SIZE: usize = mem::size_of::<SegmentNode>();
const REMOVED_SEGMENT_NODE: u32 = 0;

#[inline]
const fn decode_segment_node(val: u64) -> (u32, u32) {
    ((val >> 32) as u32, val as u32)
}

#[inline]
const fn encode_segment_node(size: u32, next: u32) -> u64 {
    ((size as u64) << 32) | next as u64
}

#[inline]
fn pad<T>() -> usize {
    let size = mem::size_of::<T>();
    let align = mem::align_of::<T>();
    size + align - 1
}

#[derive(Debug)]
#[repr(C, align(8))]
pub struct Header {
    /// The sentinel node for the ordered free list.
    pub(super) sentinel: SegmentNode,
    pub(super) allocated: AtomicU32,
    pub(super) min_segment_size: AtomicU32,
    pub(super) discarded: AtomicU32,
}

impl Header {
    #[inline]
    fn new(size: u32, min_segment_size: u32) -> Self {
        Self {
            allocated: AtomicU32::new(size),
            sentinel: SegmentNode::sentinel(),
            min_segment_size: AtomicU32::new(min_segment_size),
            discarded: AtomicU32::new(0),
        }
    }

    #[inline]
    fn load_allocated(&self) -> u32 {
        self.allocated.load(Ordering::Acquire)
    }

    #[inline]
    fn load_min_segment_size(&self) -> u32 {
        self.min_segment_size.load(Ordering::Acquire)
    }
}

#[repr(transparent)]
struct SegmentNode {
    /// The first 32 bits are the size of the memory,
    /// the last 32 bits are the offset of the next segment node.
    size_and_next: AtomicU64,
}

impl core::fmt::Debug for SegmentNode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let (offset, next) = decode_segment_node(self.size_and_next.load(Ordering::Acquire));
        f.debug_struct("SegmentNode")
            .field("offset", &offset)
            .field("next", &next)
            .finish()
    }
}

impl core::ops::Deref for SegmentNode {
    type Target = AtomicU64;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.size_and_next
    }
}

impl SegmentNode {
    #[inline]
    fn sentinel() -> Self {
        Self {
            size_and_next: AtomicU64::new(encode_segment_node(
                SENTINEL_SEGMENT_NODE_OFFSET,
                SENTINEL_SEGMENT_NODE_OFFSET,
            )),
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct Segment {
    ptr: *mut u8,
    ptr_offset: u32,
    data_offset: u32,
    data_size: u32,
}

impl Segment {
    /// ## Safety
    /// - offset must be a well-aligned and in-bounds `AtomicU64` pointer.
    // #[inline]
    // unsafe fn from_offset(arena: &Arena, offset: u32, data_size: u32) -> Self {
    //     Self {
    //         ptr: arena.ptr,
    //         ptr_offset: offset,
    //         data_offset: offset + SEGMENT_NODE_SIZE as u32,
    //         data_size,
    //     }
    // }

    #[inline]
    fn as_ref(&self) -> &SegmentNode {
        // Safety: when constructing the Segment, we have checked the ptr_offset is in bounds and well-aligned.
        unsafe {
            let ptr = self.ptr.add(self.ptr_offset as usize);
            &*ptr.cast::<SegmentNode>()
        }
    }

    #[inline]
    fn update_next_node(&self, next: u32) {
        self.as_ref()
            .store(encode_segment_node(self.data_size, next), Ordering::Release);
    }
}

struct Pessimistic;
struct Optimistic;

pub struct Arena<S> {
	magic:u16,
	version:u16,
	read_only:bool,
	retires:u16,
	// memory info
	cap:usize,
	reserved: usize,
	data_offset: usize,
	strategy: S,
}

type OArena = Arena<Pessimistic>;
type PArena = Arena<Optimistic>;