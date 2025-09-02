#![cfg(test)]

use core::ops::AddAssign;

use memory_addr::{MemoryAddr, VirtAddr};

use crate::area::{ShmArea, ShmBackend, ShmProtect, ShmSpec};
use crate::boxed::{ShmBox, ShmToken};
use crate::perlude::{ShmAllocator, ShmHeader, ShmSpinGma, ShmSpinTlsf};

const MAX_ADDR: usize = 0x10000;

type MockFlags = u8;
type MockPageTable = [MockFlags; MAX_ADDR];

struct MockSpec;

impl ShmSpec for MockSpec {
    type Addr = VirtAddr;
    type Flags = MockFlags;
}

struct MockBackend<'a>(&'a mut MockPageTable);

impl MockBackend<'_> {
    fn start(&self) -> VirtAddr {
        self.0.as_ptr().addr().into()
    }

    fn arr_addr(&self, addr: VirtAddr) -> usize {
        // addr - self.start
        addr.sub_addr(self.start())
    }
}

impl<'a> ShmBackend<MockSpec> for MockBackend<'a> {
    type Config = ();
    type Error = ();

    fn map(
        self,
        start: Option<<MockSpec as ShmSpec>::Addr>,
        size: usize,
        flags: <MockSpec as ShmSpec>::Flags,
        _cfg: (),
    ) -> Result<ShmArea<MockSpec, Self>, Self::Error> {
        let Some(start) = start else {
            return Err(());
        };
        for entry in self.0.iter_mut().skip(start.as_usize()).take(size) {
            if *entry != 0 {
                return Err(());
            }
            *entry = flags;
        }
        let start = self.start().add(start.as_usize());
        Ok(ShmArea::new(start, size, flags, self))
    }

    fn unmap(area: &mut ShmArea<MockSpec, Self>) -> Result<(), Self::Error> {
        let start = area.start();
        let arr_start = area.backend().arr_addr(start);
        let size = area.size();
        for entry in area.backend_mut().0.iter_mut().skip(arr_start).take(size) {
            if *entry == 0 {
                return Err(());
            }
            *entry = 0;
        }
        Ok(())
    }
}

impl<'a> ShmProtect<MockSpec> for MockBackend<'a> {
    fn protect(
        area: &mut ShmArea<MockSpec, Self>,
        new_flags: <MockSpec as ShmSpec>::Flags,
    ) -> Result<(), Self::Error> {
        let start = area.start();
        let arr_start = area.backend().arr_addr(start);
        let size = area.size();
        for entry in area.backend_mut().0.iter_mut().skip(arr_start).take(size) {
            if *entry == 0 {
                return Err(());
            }
            *entry = new_flags;
        }
        Ok(())
    }
}

type MySpinGma<'a> = ShmSpinGma<MockBackend<'a>, MockSpec>;
type MyTlsf<'a> = ShmSpinTlsf<MockBackend<'a>, MockSpec>;
type MyBlink<'a> = ShmSpinGma<MockBackend<'a>, MockSpec>;

fn box_test(alloc: &impl ShmAllocator) {
    let mut bb = ShmBox::new_in(1u8, alloc);
    dbg!(format!("box: {:?}", bb.as_ptr()));
    dbg!(format!("start: {:?}", bb.allocator().start_ptr()));
    assert_eq!(*bb, 1);
    bb.add_assign(2);
    assert_eq!(*bb, 3);
}

fn token_test(alloc: &impl ShmAllocator) {
    // 8 bits offset
    let bb = ShmBox::new_in(1u8, alloc);
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

fn spec_test(alloc: &(impl ShmAllocator + ShmHeader)) {
    let bb = ShmBox::new_in(32u16, &alloc);
    alloc.init_spec(bb, 0);
    match unsafe { alloc.spec_ref::<u16>(0) } {
        Some(spec) => {
            dbg!(format!("spec address: {:?}", spec.as_ptr()));
            assert_eq!(*spec, 32);
        }
        None => {
            panic!("spec is not initialized");
        }
    }
}

macro_rules! alloc_test {
    ($alloc:ty) => {{
        let mut pt = [0; MAX_ADDR];
        for start in (0..MAX_ADDR).step_by(0x2000) {
            let bk = MockBackend(&mut pt);
            let area = bk.map(Some(start.into()), 0x2000, 0, ()).unwrap();
            let alloc = <$alloc>::from_area(area).unwrap();
            box_test(&alloc);
            token_test(&alloc);
            spec_test(&alloc);
        }
    }};
}

#[test]
fn area_alloc() {
    alloc_test!(MySpinGma); // 8/1512 bits offset
    alloc_test!(MyTlsf); // 16/1712 bits offset
    alloc_test!(MyBlink); // 41/1512 bits offset
}
