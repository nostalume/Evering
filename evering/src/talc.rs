use core::{marker::PhantomData, ptr::NonNull};

use crate::numeric::AlignPtr;

type Word = usize;
const WORD_SIZE: usize = core::mem::size_of::<Word>();
const WORD_ALIGN: usize = core::mem::align_of::<Word>();
const WORD_BITS: usize = Word::BITS as usize;
// type UInt = usize;
// type RelPtr = UInt;

#[derive(Debug)]
#[repr(transparent)]
struct Rel<T> {
    pub offset: usize,
    _marker: PhantomData<T>,
}
type RelPtr = Rel<u8>;

impl<T> Rel<T> {
    #[inline]
    unsafe fn as_raw(self, base_ptr: *mut u8) -> *mut T {
        unsafe { base_ptr.add(self.offset).cast() }
    }

    #[inline]
    unsafe fn as_ptr(self, base_ptr: *mut u8) -> NonNull<T> {
        unsafe { NonNull::new_unchecked(self.as_raw(base_ptr)) }
    }

    #[inline]
    unsafe fn from_raw(ptr: *mut T, base_ptr: *const u8) -> Rel<T> {
        Rel {
            offset: ptr.addr() - base_ptr.addr(),
            _marker: PhantomData,
        }
    }
}

impl<T> Clone for Rel<T> {
    fn clone(&self) -> Self {
        Self {
            offset: self.offset,
            _marker: self._marker.clone(),
        }
    }
}

impl<T> Copy for Rel<T> {}

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
struct Tag(Word);

impl Tag {
    const SIZE: usize = core::mem::size_of::<Self>();

    /// The minimal offset of a tag from the base ptr, which is the node size.
    pub const MIN_TAG_OFFSET: usize = FreeNode::SIZE;

    pub const ALLOCATED_FLAG: usize = 1 << 0;
    pub const IS_ABOVE_FREE_FLAG: usize = 1 << 1;
    pub const ALL_FLAG: usize = Self::IS_ABOVE_FREE_FLAG | Self::ALLOCATED_FLAG;

    const BASE_MASK: usize = !(Self::IS_ABOVE_FREE_FLAG | Self::ALLOCATED_FLAG);

    unsafe fn from_base(ptr: *mut u8, size: usize) -> (*mut u8, Tag) {
        unsafe {
            let post = ptr.add(size).align_up_of::<Word>();
            // Suppose it's a ptr to `Tag` or directly a `Tag`.
            let tag_or_tag_ptr = post.cast::<*mut u8>().read();

            if tag_or_tag_ptr > post {
                // If it's indeed a ptr to the real `Tag`
                (tag_or_tag_ptr, tag_or_tag_ptr.cast::<Tag>().read())
            } else {
                // Else it's directly a `Tag`
                let tag = tag_or_tag_ptr.cast::<Word>().read();
                (post, Tag(tag))
            }
        }
    }

    unsafe fn write(tag: &mut Self, chunk_base: RelPtr, is_above_free: bool) {
        // let base_value = chunk_base.addr();
        debug_assert!(
            chunk_base.offset & !Self::BASE_MASK == 0,
            "Chunk base must be aligned."
        );

        let flags = if is_above_free {
            Self::ALL_FLAG
        } else {
            Self::ALLOCATED_FLAG
        };

        let tag_value = chunk_base.offset | flags;
        *tag = Self(tag_value);
    }

    pub fn chunk_base_offset(self) -> usize {
        self.0 & Self::BASE_MASK
    }

    pub fn is_above_free(self) -> bool {
        self.0 & Self::IS_ABOVE_FREE_FLAG != 0
    }

    pub fn is_allocated(self) -> bool {
        self.0 & Self::ALLOCATED_FLAG != 0
    }

    pub unsafe fn set_above_free(tag: *mut Self) {
        let cur = unsafe { tag.read() };
        debug_assert!(!cur.is_above_free());
        let cur = Self(cur.0.wrapping_add(Self::IS_ABOVE_FREE_FLAG));
        debug_assert!(cur.is_above_free());
        unsafe { tag.write(cur) }
    }

    pub unsafe fn clear_above_free(tag: *mut Self) {
        let cur = unsafe { tag.read() };
        debug_assert!(cur.is_above_free());
        let cur = Self(cur.0.wrapping_sub(Self::IS_ABOVE_FREE_FLAG));
        unsafe { tag.write(cur) }
    }
}

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
    const SIZE: usize = core::mem::size_of::<Self>();

    /// The ptr to the free node's `next`(ptr to the next free node) field.
    #[inline]
    pub fn next_ptr(ptr: *mut Self) -> *mut FreeNodeLink {
        ptr.cast()
    }

    #[inline]
    pub fn next_rel(ptr: *mut Self, base_ptr: *mut u8) -> Rel<FreeNodeLink> {
        unsafe { Rel::from_raw(Self::next_ptr(ptr), base_ptr) }
    }

    pub unsafe fn insert(
        node: *mut Self,
        next: FreeNodeLink,
        prev_next: Rel<FreeNodeLink>,
        base_ptr: *mut u8,
    ) {
        unsafe {
            debug_assert!(!node.is_null());
            let prev_next_ptr = prev_next.as_raw(base_ptr);
            debug_assert!(!prev_next_ptr.is_null());

            node.write(Self { next, prev_next });
            *prev_next_ptr = Some(Rel::from_raw(node, base_ptr));

            if let Some(next) = next {
                (*next.as_raw(base_ptr)).prev_next = Self::next_rel(node, base_ptr);
            }
        }
    }

    pub unsafe fn remove(node: *mut Self, base_ptr: *mut u8) {
        debug_assert!(!node.is_null());
        unsafe {
            let FreeNode { next, prev_next } = node.read();
            let prev_next_ptr = prev_next.as_raw(base_ptr);
            debug_assert!(!prev_next_ptr.is_null());
            *prev_next_ptr = next;

            if let Some(next) = next {
                (*next.as_raw(base_ptr)).prev_next = prev_next;
            }
        }
    }
}

/// Header structure at the base of a free chunk.
///
/// The overall chunk layout is: `[FreeHead] [Data Area] [FreeTail/Tag]`
#[repr(C)]
struct FreeHead {
    node: FreeNode,
    size_low: usize,
}

impl FreeHead {
    const NODE_OFFSET: usize = 0;
    const LOW_SIZE_OFFSET: usize = FreeNode::SIZE;

    /// Creates a pointer to the FreeHead struct from the raw chunk base pointer.
    #[inline]
    unsafe fn from_base(base: *mut u8) -> *mut Self {
        base.cast()
    }

    /// Calculates the chunk base pointer from a NonNull pointer to the LlistNode.
    #[inline]
    unsafe fn from_node(node: NonNull<FreeNode>) -> *mut u8 {
        // The LlistNode is at offset 0 within FreeHead, and FreeHead is at the chunk base.
        // Therefore, the node pointer is the same as the FreeHead pointer (and the chunk base).
        node.as_ptr().cast()
    }

    /// Gets the raw base pointer of the chunk from a pointer to the FreeHead.
    #[inline]
    unsafe fn to_base(head: *mut Self) -> *mut u8 {
        head.cast()
    }

    /// Calculates the acme pointer (the address at the exclusive *end* of the chunk)
    /// by reading the `size_low` field.
    #[inline]
    unsafe fn to_acme(head: *mut Self) -> *mut u8 {
        unsafe {
            let size = (*head).size_low;
            head.cast::<u8>().add(size)
        }
    }
}

/// Tail structure at the base of a free chunk.
///
/// The overall chunk layout is: `(base)[FreeHead] [Data Area] [FreeTail/Tag](acme)`
#[repr(transparent)]
struct FreeTail {
    // Stores the size of the entire chunk
    size_high: usize,
}

impl FreeTail {
    /// Typically equal to `WORD_SIZE`
    const SIZE: usize = core::mem::size_of::<Self>();

    /// Creates a pointer to the `FreeTail` from the chunk acme ptr.
    #[inline]
    unsafe fn from_raw(acme: *mut u8) -> *mut Self {
        unsafe { acme.sub(FreeTail::SIZE).cast() }
    }

    /// Calculates the chunk base pointer from the chunk acme pointer
    /// by reading the `size_high` field.
    #[inline]
    unsafe fn to_base(acme: *mut u8) -> *mut u8 {
        unsafe {
            let tail_ptr = Self::from_raw(acme);
            let size = (*tail_ptr).size_high;
            acme.sub(size)
        }
    }
}

const MIN_CHUNK_SIZE: usize = Tag::MIN_TAG_OFFSET + Tag::SIZE;
/// Returns whether the range is greater than `MIN_CHUNK_SIZE`.
fn is_chunk_size(base: *mut u8, acme: *mut u8) -> bool {
    debug_assert!(acme >= base, "!(acme {:p} >= base {:p})", acme, base);
    acme.addr() - base.addr() >= MIN_CHUNK_SIZE
}
