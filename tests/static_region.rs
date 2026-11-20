#![cfg(unix)]

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

/// The alignment a port arranges its own base to, written as the literal `repr(align)` needs.
const ARRANGE: usize = 1 << 21;
const SIZE: usize = 1 << 16;
const REGION: RegionId = RegionId::new(0x0053_E480, 1);

/// The bound is only worth publishing if an extent can be arranged to it: an alignment the crate
/// asks for that no static could carry would make `Source` impossible to implement here.
const _: () = assert!(ARRANGE >= REGION_ALIGN);

#[repr(C, align(2_097_152))]
struct Extent {
    bytes: [u8; SIZE],
}

// The composition writes the header and the directory into these bytes, so they cannot be a `static`.
// Nothing takes a reference to them: the only handle is the raw base pointer `Fixed::map` makes, which
// is what a region placed at an address by a linker script looks like from the process that has it.
static mut EXTENT: Extent = Extent { bytes: [0; SIZE] };

/// The region that was there before `main` and is never given back.
struct Fixed;

unsafe impl Source for Fixed {
    type Error = io::Error;

    fn map(self, request: Request) -> Result<Map, Self::Error> {
        if request.len == 0 || request.len > SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the fixed extent bounds neither a zero nor a longer request",
            ));
        }
        // Naming the address of the extent is not an access to its bytes.
        let base = addr_of_mut!(EXTENT).cast::<u8>();
        // SAFETY: the extent is never empty, so its address is never null.
        let start = unsafe { NonNull::new_unchecked(base) };
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
fn a_region_fixed_in_the_image_composes_and_survives_every_owner_leaving_it() {
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
fn a_fixed_extent_claimed_read_only_refuses_the_write_every_composition_asks_for() {
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
    let base = addr_of_mut!(EXTENT).cast::<u8>().addr();
    assert_eq!(
        base % REGION_ALIGN,
        0,
        "the arrangement this port chose stops satisfying the crate's bound"
    );
}
