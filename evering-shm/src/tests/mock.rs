#![cfg(test)]

use core::ops::{Deref, DerefMut};

use memory_addr::{MemoryAddr, VirtAddr};

use super::super::{
    area::{AddrSpec, MemBlk, Mmap, Mprotect, RawMemBlk},
    arena::{ArenaMemBlk, Optimistic},
    malloc::MemAllocInfo,
};

const MAX_ADDR: usize = 0x10000;

type MockFlags = u8;
type MockPageTable = [MockFlags; MAX_ADDR];
type MockMemBlk<'a> = MemBlk<MockAddr, MockBackend<'a>>;
type MockArena<'a> = ArenaMemBlk<MockAddr, MockBackend<'a>, Optimistic>;

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

fn mock_arena(pt: &mut MockPageTable, start: Option<VirtAddr>, size: usize) -> MockArena<'_> {
    let bk = MockBackend(pt);
    let a = ArenaMemBlk::init(bk, start, size, 0, ()).unwrap();
    a
}

#[test]
fn area_test() {
    const STEP: usize = 0x2000;
    let mut pt = [0; MAX_ADDR];
    for start in (0..MAX_ADDR).step_by(STEP) {
        let a = mock_area(&mut pt, Some(start.into()), STEP);
    }
}

#[test]
fn arena_test() {
    use std::sync::Barrier;
    use std::thread;
    
    const BYTES_SIZE: u32 = 50;
    const REDUCED_SIZE:u32 = 35;

    let mut pt = [0u8; MAX_ADDR];
    let mem = mock_arena(&mut pt, Some(0.into()), MAX_ADDR);
    let a = mem.arena();

    let bar = Barrier::new(5);
    let mut metas = Vec::new();

    for _ in 1..=5 {
        let bytes = a.alloc_bytes(BYTES_SIZE).unwrap().unwrap();
        metas.push(bytes);
    }

    let remained = a.remained();
    let _remained_bytes = a.alloc_bytes(remained as u32).unwrap();
    metas.drain(..).for_each(|meta| {
        a.dealloc(meta);
    });

    thread::scope(|s| {
        for _ in (1..=5).rev() {
            let a = &a;
            let bar = &bar;

            s.spawn(move || {
                bar.wait();
                let mut bytes = a.alloc_bytes(REDUCED_SIZE).unwrap().unwrap();
                // do something with bytes...
            });
        }
    });
}
