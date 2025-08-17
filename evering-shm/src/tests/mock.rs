#![cfg(test)]

use core::ops::AddAssign;

use memory_addr::{MemoryAddr, VirtAddr};

use crate::shm_alloc::{ShmAllocator, ShmHeader, ShmSpinGma, ShmSpinTlsf};
use crate::shm_area::{ShmArea, ShmBackend, ShmProtect, ShmSpec};
use crate::shm_box::{ShmBox, ShmToken};

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

type MySpinGma<'a> = ShmSpinGma<MockSpec, MockBackend<'a>>;
type MyTlsf<'a> = ShmSpinTlsf<MockSpec, MockBackend<'a>>;
type MyBlink<'a> = ShmSpinGma<MockSpec, MockBackend<'a>>;

fn box_test(allocator: &impl ShmAllocator) {
    let mut bb = ShmBox::new_in(1u8, allocator);
    dbg!(format!("box: {:?}", bb.as_ptr()));
    dbg!(format!("start: {:?}", bb.allocator().start_ptr()));
    assert_eq!(*bb, 1);
    bb.add_assign(2);
    assert_eq!(*bb, 3);
}

fn token_test(allocator: &impl ShmAllocator) {
    // 8 bits offset
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

macro_rules! header_spec_test {
    ($name:ident, $alloc:ty) => {
        fn $name<'a>(allocator: &'a $alloc) {
            let bb = ShmBox::new_in(32u16, allocator);
            allocator.init_spec(bb,0);
            match allocator.spec_raw::<u16>(0) {
                Some(spec) => {
                    let spec = unsafe { spec.as_ref() };
                    dbg!(format!("spec address: {:?}", spec));
                    assert_eq!(*spec, 32);
                }
                None => {
                    panic!("spec not initialized");
                }
            }
        }
    };
}

header_spec_test!(header_spec_test_gma, MySpinGma);
header_spec_test!(header_spec_test_blink, MyBlink);
header_spec_test!(header_spec_test_tlsf, MyTlsf);

macro_rules! alloc_test {
    ($alloc:ty, $spec_fn:ident) => {{
        let mut pt = [0; MAX_ADDR];
        for start in (0..MAX_ADDR).step_by(0x2000) {
            let bk = MockBackend(&mut pt);
            let area = bk.map(Some(start.into()), 0x2000, 0, ()).unwrap();
            let alloc = <$alloc>::from_area(area).unwrap();
            box_test(&alloc);
            token_test(&alloc);
            $spec_fn(&alloc);
        }
    }};
}

#[test]
fn area_alloc() {
    alloc_test!(MySpinGma, header_spec_test_gma); // 8/1512 bits offset
    alloc_test!(MyTlsf, header_spec_test_tlsf); // 16/1712 bits offset
    alloc_test!(MyBlink, header_spec_test_blink); // 41/1512 bits offset
}
