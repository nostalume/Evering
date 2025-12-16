#![cfg(test)]

mod arena;
mod talc;

use core::ops::{Deref, DerefMut};

use memory_addr::{MemoryAddr, VirtAddr};

use crate::mem::{Access, Accessible, AddrSpec, MapLayout, Mmap, Mprotect, RawMap};

const MAX_ADDR: usize = 0x20000;

type MockFlags = u8;
type MockPageTable = [MockFlags];

impl const From<Access> for MockFlags {
    fn from(_value: Access) -> Self {
        0
    }
}

impl Accessible for MockFlags {
    fn permits(self, _access: crate::mem::Access) -> bool {
        true
    }
}

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
    // Due to mock addr, start is not real addr.
    // We take handle as offset from ptr of array.
    type Handle = usize;
    type MapFlags = ();
    type Error = ();

    fn map(
        self,
        _start: Option<<MockAddr as AddrSpec>::Addr>,
        size: usize,
        _mflags: (),
        pflags: <MockAddr as AddrSpec>::Flags,
        handle: usize,
    ) -> Result<RawMap<MockAddr, Self>, Self::Error> {
        for entry in self.0.iter_mut().skip(handle).take(size) {
            if *entry != 0 {
                return Err(());
            }
            *entry = pflags;
        }
        let start = self.start().add(handle);
        Ok(unsafe { RawMap::from_raw(start, size, pflags, self) })
    }

    fn unmap(area: &mut RawMap<MockAddr, Self>) -> Result<(), Self::Error> {
        let start = area.spec.start();
        let arr_start = area.bk.arr_addr(start);
        let size = area.spec.size();
        for entry in area.bk.iter_mut().skip(arr_start).take(size) {
            *entry = 0;
        }
        Ok(())
    }
}

impl<'a> Mprotect<MockAddr> for MockBackend<'a> {
    unsafe fn protect(
        area: &mut RawMap<MockAddr, Self>,
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

impl MockBackend<'_> {
    fn shared(self, start: usize, size: usize) -> MapLayout<MockAddr, Self> {
        MapLayout::new(self.map(None, size, (), 0, start).unwrap()).unwrap()
    }
}
