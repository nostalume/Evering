//! What a target that has no libc, no descriptor and no process model actually meets when it plugs into
//! this crate. The two replacements `evering::os` documents for a host — a `Source` over bytes in the
//! program's own image, and a `Notify`/`Wait` pair over a doorbell that arrives by interrupt — are written
//! here the way a port writes them, with no `std` anywhere in sight. This crate consumes `evering` from
//! outside it and through the published surface only, so a capability missing here is a promise the surface
//! did not keep, not an internal that was reached by accident. The answer this crate gives is the build:
//! "compiles at `riscv32imac-unknown-none-elf`" is the evidence, and it can never be an execution, because
//! a bare target has no harness to run it. See `const_image` for the other question a bare port asks — what
//! it can hand such a `Source` already initialized.

#![no_std]
#![allow(static_mut_refs)]

extern crate alloc;

use core::ptr::{NonNull, addr_of_mut};

use evering::{
    Session,
    layout::RegionId,
    mapping::{Access, Map, REGION_ALIGN, Request, Source},
    notify::{Adaptive, Notify, Signals, Wait},
};

const SIZE: usize = 1 << 16;
const AREA: usize = SIZE + REGION_ALIGN;
const REGION: RegionId = RegionId::new(0x0053_E480, 1);

/// The port's own failure word. There is no `io::Error` here and no `std::error::Error` to box it into,
/// which is the part of the seam a bare target reaches first.
#[derive(Debug)]
pub struct NoRoom;

#[repr(C, align(4096))]
struct Extent {
    bytes: [u8; AREA],
}

static mut EXTENT: Extent = Extent { bytes: [0; AREA] };

struct Fixed;

unsafe impl Source for Fixed {
    type Error = NoRoom;

    fn map(self, request: Request) -> Result<Map, Self::Error> {
        if request.len == 0 || request.len > AREA {
            return Err(NoRoom);
        }
        let area = addr_of_mut!(EXTENT).cast::<u8>();
        let slide = area.addr().next_multiple_of(REGION_ALIGN) - area.addr();
        if slide + request.len > AREA {
            return Err(NoRoom);
        }
        // SAFETY: the check above keeps the offset inside the object, the extent is placed by the linker
        // and outlives every map made from it, and there is nothing to release.
        let start = unsafe { NonNull::new_unchecked(area.add(slide)) };
        Ok(unsafe { Map::from_static(start, request.len, request.access) })
    }
}

/// A mailbox in shared SRAM that the peer's ISR sets and this core's wait loop clears.
pub struct Mailbox;

impl Notify for Mailbox {
    type Error = NoRoom;

    fn notify(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Wait for Mailbox {
    type Error = NoRoom;

    fn wait(&self) -> impl core::future::Future<Output = Result<(), Self::Error>> + '_ {
        // A real port parks on WFI and is woken by the same flag; `Ready` stands in for the wake so this
        // file says nothing about scheduling that it has not measured.
        async move { Ok(()) }
    }
}

/// Compose and attach over bytes nobody mapped, then hang the portable retry ladder on the port's own
/// doorbell. Everything a bare target can reach has to be reachable from here.
pub fn composed() -> Option<RegionId> {
    let access = Access::READ | Access::WRITE;
    let first = Session::create(Fixed, Request::new(SIZE, access), REGION).ok()?;
    let second = Session::open(Fixed, Request::new(SIZE, access), REGION).ok()?;
    let _ = (first.peer(), second.peer().generation());
    let signals = Signals::new(&Mailbox, &Mailbox);
    let _patience = Adaptive::new(&Mailbox);
    let _ = &signals;
    Some(REGION)
}

mod const_image;
