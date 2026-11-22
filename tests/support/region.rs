//! One extent per claim, made the same way on either host.
//!
//! What a claim chooses here is only the *identity* of the extent — its region id, its size, the label a
//! unix object carries — because the host seam underneath is already a pair: `memfd` against `Section`,
//! both behind one call. Two files that asked for the same size under two different region ids are two
//! claims, and this module says so by taking the id as an argument instead of hiding one.
//!
//! What is deliberately not here: the `/dev/shm` name guard, which exactly one claim file uses. A helper
//! with one holder stays with that holder, and a host seam a claim reaches alone is that claim's own
//! business — `tests/support.rs` states the rule.

use evering::{
    Session,
    layout::RegionId,
    mapping::{Access, Request},
};
#[cfg(feature = "process")]
use evering::{
    os::{Grant, Handoff, Shared, shared},
    process::Bootstrap,
};

/// The access declaration every multi-process suite asks for. `shared` and `memfd` record it on the object,
/// so a request narrower than this is what gets refused at mapping time rather than a silently wider map.
pub fn access() -> Access {
    Access::READ | Access::WRITE
}

/// A request for `bytes` under [`access`], which is the shape the suites repeat line for line.
pub fn extent(bytes: usize) -> Request {
    Request::new(bytes, access())
}

/// A region, the session that made it, and the meeting a child is admitted to — the four things every
/// admitted-child case builds before it can admit anything. The size and the id are arguments, not defaults:
/// the three copies this replaces shared a body, not a number.
#[cfg(feature = "process")]
pub fn meeting(
    name: &str,
    bytes: usize,
    region: RegionId,
) -> (Shared, Session, Handoff, Grant, Bootstrap) {
    let mapping = shared(name, bytes, access()).expect("a region to hold a member in");
    let session = Session::create(mapping.borrow(), extent(bytes), region)
        .expect("create the session that admits");
    let (handoff, grant) = Handoff::bind().expect("a meeting to admit a child to");
    let bootstrap = Bootstrap::new(name.as_bytes()).expect("a bootstrap within the ceiling");
    (mapping, session, handoff, grant, bootstrap)
}

/// Two sessions over one anonymous extent, made and opened rather than admitted to: the shape a claim
/// needs when both sides are already members and the question is what a notification does between them.
pub fn pair(label: &str, bytes: usize, region: RegionId) -> (Session, Session) {
    // The label buys a name only where the host gives the object one. An anonymous section is reached by
    // handle, so on Windows the same argument carries no fact about the extent.
    let _ = label;
    #[cfg(unix)]
    let source = evering::os::unix::UnixFd::memfd(label, bytes, false)
        .expect("a region wide enough for two sessions");
    #[cfg(windows)]
    let source = evering::os::windows::Section::anonymous(bytes, access())
        .expect("a region wide enough for two sessions");

    let request = extent(bytes);
    (
        Session::create(source.borrow(), request, region).expect("create the first session"),
        Session::open(source.borrow(), request, region).expect("open the second session"),
    )
}
