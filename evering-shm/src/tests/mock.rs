#![cfg(test)]

use core::ops::{Deref, DerefMut};

use memory_addr::{MemoryAddr, VirtAddr};

use crate::area::{AddrSpec, MemBlk, Mmap, Mprotect, RawMemBlk};

const MAX_ADDR: usize = 0x10000;

type MockFlags = u8;
type MockPageTable = [MockFlags; MAX_ADDR];
type MockMemBlk<'a> = MemBlk<MockAddr, MockBackend<'a>, ()>;

struct MockAddr;

impl AddrSpec for MockAddr {
    type Addr = VirtAddr;
    type Flags = MockFlags;
}

struct MockBackend<'a>(&'a mut MockPageTable);

impl<'a> Deref for MockBackend<'a> {
    type Target = MockPageTable;

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

impl<'a> DerefMut for MockBackend<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0
    }
}

impl MockBackend<'_> {
    fn start(&self) -> VirtAddr {
        self.0.as_ptr().addr().into()
    }

    fn arr_addr(&self, addr: VirtAddr) -> usize {
        // addr - self.start
        addr.sub_addr(self.start())
    }
}

impl<'a> Mmap<MockAddr> for MockBackend<'a> {
    type Config = ();
    type Error = ();

    fn map(
        self,
        start: Option<<MockAddr as AddrSpec>::Addr>,
        size: usize,
        flags: <MockAddr as AddrSpec>::Flags,
        _cfg: (),
    ) -> Result<RawMemBlk<MockAddr, Self>, Self::Error> {
        let start = match start {
            Some(start) => start,
            None => 0.into(),
        };
        for entry in self.0.iter_mut().skip(start.as_usize()).take(size) {
            if *entry != 0 {
                return Err(());
            }
            *entry = flags;
        }
        let start = self.start().add(start.as_usize());
        Ok(RawMemBlk::from_raw(start, size, flags, self))
    }

    fn unmap(area: &mut RawMemBlk<MockAddr, Self>) -> Result<(), Self::Error> {
        let start = area.a.start();
        let arr_start = area.bk.arr_addr(start);
        let size = area.a.size();
        for entry in area.bk.iter_mut().skip(arr_start).take(size) {
            if *entry == 0 {
                return Err(());
            }
            *entry = 0;
        }
        Ok(())
    }
}

impl<'a> Mprotect<MockAddr> for MockBackend<'a> {
    fn protect(
        area: &mut RawMemBlk<MockAddr, Self>,
        new_flags: <MockAddr as AddrSpec>::Flags,
    ) -> Result<(), Self::Error> {
        let start = area.start();
        let arr_start = area.bk.arr_addr(start);
        let size = area.size();
        for entry in area.bk.iter_mut().skip(arr_start).take(size) {
            if *entry == 0 {
                return Err(());
            }
            *entry = new_flags;
        }
        Ok(())
    }
}

fn mock_area(pt: &mut MockPageTable, start: Option<VirtAddr>, size: usize) -> MockMemBlk<'_> {
    let bk = MockBackend(pt);
    let a = MemBlk::init(bk, start, size, 0, ()).unwrap();
    a
}

#[test]
fn area_test() {
    const STEP: usize = 0x2000;
    let mut pt = [0; MAX_ADDR];
    for start in (0..MAX_ADDR).step_by(STEP) {
        let a = mock_area(&mut pt, Some(start.into()), STEP);
        let header = a.header();
        dbg!(format!("Header: {:?}", header));
    }
}
// type MySpinGma<'a> = ShmSpinGma<MockBackend<'a>, MockSpec>;
// type MyTlsf<'a> = ShmSpinTlsf<MockBackend<'a>, MockSpec>;
// type MyBlink<'a> = ShmSpinGma<MockBackend<'a>, MockSpec>;

// fn box_test(alloc: &impl MemBase) {
//     let mut bb = ShmBox::new_in(1u8, alloc);
//     dbg!(format!("box: {:?}", bb.as_ptr()));
//     dbg!(format!("start: {:?}", bb.allocator().start_ptr()));
//     assert_eq!(*bb, 1);
//     bb.add_assign(2);
//     assert_eq!(*bb, 3);
// }

// fn token_test(alloc: &impl MemBase) {
//     // 8 bits offset
//     let bb = ShmBox::new_in(1u8, alloc);
//     dbg!(format!("box: {:?}", bb.as_ptr()));
//     dbg!(format!("start: {:?}", bb.allocator().start_ptr()));
//     let token = ShmToken::from(bb);
//     dbg!(format!("offset: {:?}", token.offset()));
//     let bb = ShmBox::from(token);
//     dbg!(format!("translated box: {:?}", bb.as_ptr()));
//     dbg!(format!(
//         "translated start: {:?}",
//         bb.allocator().start_ptr()
//     ));
//     assert_eq!(*bb, 1);
// }

// fn spec_test(alloc: &(impl MemBase + ShmHeader)) {
//     let bb = ShmBox::new_in(32u16, &alloc);
//     alloc.init_spec(bb, 0);
//     match unsafe { alloc.spec_ref::<u16>(0) } {
//         Some(spec) => {
//             dbg!(format!("spec address: {:?}", spec.as_ptr()));
//             assert_eq!(*spec, 32);
//         }
//         None => {
//             panic!("spec is not initialized");
//         }
//     }
// }

macro_rules! alloc_test {
    ($alloc:ty) => {{
        let mut pt = [0; MAX_ADDR];
        for start in (0..MAX_ADDR).step_by(0x2000) {
            let bk = MockBackend(&mut pt);
            let area = bk.map(Some(start.into()), 0x2000, 0, ()).unwrap();
            let alloc = <$alloc>::from_area(area).unwrap();
            box_test(&alloc);
            token_test(&alloc);
        }
    }};
}
