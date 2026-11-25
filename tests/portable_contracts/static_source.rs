//! The port seam, written the way a port writes it: one `Source` over an extent that is already in
//! this binary's own image, so no memfd is created, no section is opened, no descriptor travels and
//! there is nothing to release. `src/tests/unix.rs` reaches the same session calls through
//! `Source::map` on a *real* backend, which is the only way to exercise the reserve-then-place path —
//! and therefore the only way that path can fail to show what a target with no mapping call at all
//! would answer for. What is asserted here is the shortest honest list of that: the bound a base has
//! to sit on, that a region composes and detaches over bytes nobody unmaps, and that a read-only
//! extent refuses the write every composition has to make.
//!
//! Two things this cannot show. The extent belongs to one process and one thread at a time, which is
//! what a link-script region means but not the whole promise `Map::from_static` asks for: a port that
//! shares one across threads keeps that promise itself. And nothing on this host can make a peer map
//! these same bytes, so the residue agreement is asserted only of the base this port arranged — the
//! peer case is `src/tests/unix/talc.rs` and the cross-process suites, not this file.

use std::{
    io,
    ptr::{NonNull, addr_of_mut},
};

use evering::{
    Session, SessionError,
    layout::RegionId,
    mapping::{Access, Map, REGION_ALIGN, Request, Source},
};

const SIZE: usize = 1 << 16;
/// The bytes the extent carries: what a request asks for, plus the slack reaching the bound costs.
const AREA: usize = SIZE + REGION_ALIGN;
const REGION: RegionId = RegionId::new(0x0053_E480, 1);

/// The bound is only worth publishing if a base can be moved onto it at all, and moving up to one is
/// arithmetic only a power of two makes sense of.
const _: () = assert!(REGION_ALIGN.is_power_of_two());

/// An alignment an image carries on every host this file is compiled for, which is deliberately not the
/// bound the region is arranged against. Declaring the bound rather than reaching it was the shape this
/// file had, and it is not available everywhere: on `x86_64-pc-windows-msvc`, a static aligned past the
/// 4 KiB a section is placed by aborts the compiler that writes it — measured from 8 KiB up, on every
/// toolchain installed on that host. A port there moves up inside its extent to a base with no residue,
/// which is the arrangement `os::unix` makes with a mapping when it reserves a window and places a file
/// in it.
#[repr(C, align(4096))]
struct Extent {
    bytes: [u8; AREA],
}

// The composition writes the header and the directory into these bytes, so they cannot be a `static`.
// Nothing takes a reference to them: the only handle is the raw base pointer `Fixed::map` makes, which
// is what a region placed at an address by a linker script looks like from the process that has it.
static mut EXTENT: Extent = Extent { bytes: [0; AREA] };

/// The first address inside the extent that carries no residue, with `len` bytes of extent left from
/// there. A request for nothing has no base either: it never reaches the question this port answers.
fn arranged(len: usize) -> Option<NonNull<u8>> {
    if len == 0 {
        return None;
    }
    let area = addr_of_mut!(EXTENT).cast::<u8>();
    // Naming the address of the extent is not an access to its bytes.
    let slide = area.addr().next_multiple_of(REGION_ALIGN) - area.addr();
    if slide + len > AREA {
        return None;
    }
    // SAFETY: the check above keeps the offset inside the object, and the object's address is never
    // null because the extent is never empty.
    Some(unsafe { NonNull::new_unchecked(area.add(slide)) })
}

/// The region that was there before `main` and is never given back.
struct Fixed;

unsafe impl Source for Fixed {
    type Error = io::Error;

    fn map(self, request: Request) -> Result<Map, Self::Error> {
        let Some(start) = arranged(request.len) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the fixed extent bounds neither a zero nor a longer request",
            ));
        };
        assert!(
            start.addr().get() % REGION_ALIGN == 0,
            "a base no peer could reproduce a block address from"
        );
        // SAFETY: the extent is owned by nobody else in this process, lives as long as the mapping
        // that is made from it, and has nothing to release — which is what `from_static` declares
        // rather than what `from_raw_parts` would be lied about.
        Ok(unsafe { Map::from_static(start, request.len, request.access) })
    }
}

#[test]
fn a_static_region_composes_and_survives_every_owner_leaving() {
    let access = Access::READ | Access::WRITE;
    let first = Session::create(Fixed, Request::new(SIZE, access), REGION)
        .expect("compose a region over bytes that were never mapped");
    let second = Session::open(Fixed, Request::new(SIZE, access), REGION)
        .expect("attach a second member to the same fixed bytes");
    assert_ne!(
        first.peer(),
        second.peer(),
        "two members of one fixed extent hold one identity"
    );
    let left = second.peer();

    // Dropping both is the half only a static extent can show. An owner of a real mapping unmaps on
    // the way out; an owner of bytes it never acquired has to simply let them go, and if that
    // distinction were missing this is where it would panic.
    drop(first);
    drop(second);

    let third = Session::open(Fixed, Request::new(SIZE, access), REGION)
        .expect("attach after every owner of the fixed bytes left");
    // The generation counter a member leaves behind lives in the region's own bytes, so a number
    // higher than the one just released says those bytes were still there to be read — which is the
    // whole difference between an extent that was unmapped and one that was merely let go.
    assert!(
        third.peer().generation() > left.generation(),
        "the roster of a fixed extent forgot the members that left it: {:?} after {:?}",
        third.peer(),
        left
    );
}

#[test]
fn a_read_only_fixed_extent_refuses_the_write_composition_asks_for() {
    let refused = Session::create(Fixed, Request::new(SIZE, Access::READ), REGION);
    let Err(error) = refused else {
        panic!("a region composed over bytes that cannot be written");
    };
    assert!(
        matches!(
            error,
            SessionError::PermissionDenied { requested } if requested == Access::WRITE
        ),
        "the read-only extent was refused for another reason: {error}"
    );
}

#[test]
fn the_extent_a_port_arranges_sits_on_the_bound_the_crate_publishes() {
    // Computing an address is not an access, so this reads nothing the other two tests are writing.
    let area = addr_of_mut!(EXTENT).cast::<u8>().addr();
    let base = arranged(SIZE).expect(
        "an extent padded by one bound has a base for its own request wherever it was loaded",
    );
    assert_eq!(
        base.addr().get() % REGION_ALIGN,
        0,
        "the arrangement this port chose stops satisfying the crate's bound"
    );
    assert!(
        base.addr().get() + SIZE <= area + AREA,
        "a base moved up to the bound has to leave the request that used it inside the extent"
    );
}
