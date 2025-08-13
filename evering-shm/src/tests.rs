#![cfg(test)]
use core::ops::AddAssign;

use memory_addr::VirtAddr;
use memory_set::{MappingBackend, MemoryArea, MemorySet};

use crate::shm_alloc::{self, ShmAllocator};
use crate::shm_box::{ShmBox, ShmToken};

const MAX_ADDR: usize = 0x10000;

type MockFlags = u8;
type MockPageTable = [MockFlags; MAX_ADDR];

type MockMemorySet = MemorySet<MockBackend>;
#[derive(Clone, Copy)]
struct MockBackend;

impl MappingBackend for MockBackend {
    type Addr = VirtAddr;
    type Flags = MockFlags;
    type PageTable = MockPageTable;

    fn map(&self, start: VirtAddr, size: usize, flags: MockFlags, pt: &mut MockPageTable) -> bool {
        for entry in pt.iter_mut().skip(start.as_usize()).take(size) {
            if *entry != 0 {
                return false;
            }
            *entry = flags;
        }
        true
    }

    fn unmap(&self, start: VirtAddr, size: usize, pt: &mut MockPageTable) -> bool {
        for entry in pt.iter_mut().skip(start.as_usize()).take(size) {
            if *entry == 0 {
                return false;
            }
            *entry = 0;
        }
        true
    }

    fn protect(
        &self,
        start: VirtAddr,
        size: usize,
        new_flags: MockFlags,
        pt: &mut MockPageTable,
    ) -> bool {
        for entry in pt.iter_mut().skip(start.as_usize()).take(size) {
            if *entry == 0 {
                return false;
            }
            *entry = new_flags;
        }
        true
    }
}

fn box_test(allocator: &impl ShmAllocator) {
    let mut bb = ShmBox::new_in(1u8, allocator);
    dbg!(format!("box: {:?}", bb.as_ptr()));
    dbg!(format!("start: {:?}", bb.allocator().start_ptr()));
    assert_eq!(*bb, 1);
    bb.add_assign(2);
    assert_eq!(*bb, 3);
}

fn token_test(allocator: &impl ShmAllocator) {
    let bb = ShmBox::new_in(1u8, allocator);
    dbg!(format!("box: {:?}", bb.as_ptr()));
    dbg!(format!("start: {:?}", bb.allocator().start_ptr()));
    let token = ShmToken::from(bb);
    dbg!(format!("offset: {:?}", token.offset()));
    let bb = ShmBox::from(token);
    dbg!(format!("translated box: {:?}", bb.as_ptr()));
    dbg!(format!(
        "translated start: {:?}",
        bb.allocator().start_ptr()
    ));
    assert_eq!(*bb, 1);
}

macro_rules! alloc_test {
    ($alloc:ty) => {{
        let pt = [0; MAX_ADDR];

        for start in (0..MAX_ADDR).step_by(0x2000) {
            let start = pt.as_ptr().addr() + start;
            let area = MemoryArea::new(start.into(), 0x1000, 1, MockBackend);
            let alloc = <$alloc>::from_area(area);
            box_test(&alloc);
            token_test(&alloc);
        }
    }};
}

#[test]
fn area_alloc() {
    alloc_test!(shm_alloc::ShmSpinGma<MockBackend>); // 8 bits offset
    alloc_test!(shm_alloc::ShmSpinTlsf<MockBackend>); // 32 bits offset
    alloc_test!(shm_alloc::ShmBlinkGma<MockBackend>); // 41 bits offset
}
