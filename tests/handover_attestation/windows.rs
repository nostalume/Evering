//! The Windows face of this claim: the same return half of a handover, off the mechanism
//! this host actually has — the child writes its arrival down the pipe instance it met over, the parent
//! reads it, and the kernel names the client that wrote it out of the instance rather than out of a field
//! beside the message. Nothing in `announcement` reaches this far: that leg's child stops
//! at the announcement, and the promise these cases are about is what answers come *back*.
//!
//! Running it here is what settled the two faces of a child that will not report, which the seam documented
//! as unmeasured on this host. A receipt the child wrote before it left is still read after the exit, and
//! `GetNamedPipeClientProcessId` still names the departed client — so the order the Unix file uses, reap
//! first and read second, is available here and is kept here. A child that met, wrote nothing and exited is
//! refused at once with `UnexpectedEof`: the instance can say nobody is left to report, so a caller's bound
//! never gets to be that answer — while the same shape on the datagram rendezvous reports nothing at all
//! and the read is ended only by the deadline. That deadline is a live answer rather than a formality here,
//! because the other face is silence from a client that has not gone: it met, held the instance open and
//! said nothing, and the read gives up at exactly the bound its caller named. Same three answers — whose,
//! what, promised-what — one of them reached by a different road.
//!
//! What is not evidence here: that the credit is exclusive, which is `nMaxInstances == 1` in `bind` and no
//! case in this file widens it; that a writer naming somebody else is refused by the host, which this arm
//! does not attempt at all (the pipe answers with the client of the instance, and the impostor the other
//! direction is the `handover_admission` claim); and which member a death reached for a process that
//! was never promised anything, since an unpromised child has no receipt to reconcile and its parent is
//! left with [`assume_dead`](crate::Session::assume_dead).

use std::{
    env,
    io::ErrorKind,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    process::Command,
    time::{Duration, Instant},
};

use evering::{
    Session, SessionError,
    layout::RegionId,
    notify::Notify,
    os::{Attendance, Event, Grant, Handoff, Ring, Shared, shared},
    process::{Bootstrap, Death, Introduction, Supervisor},
};
use windows_sys::Win32::{
    Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING, ReadFile, WriteFile},
};

use crate::support::{introduce, region, role};

const SIZE: usize = 1 << 20;
const REGION: RegionId = RegionId::new(0x0053_E490, 1);
/// A second region, made by the same parent, which a death about [`REGION`] must never reach.
const OTHER: RegionId = RegionId::new(0x0053_E491, 1);
/// The key `src/process.rs` freezes for the granted rendezvous; here its value is the pipe's address, and
/// a child can write into that address without meeting over it.
const RENDEZVOUS: &str = "EVERING_RENDEZVOUS";
/// The offer's tag, which is the frame a receipt read exists to refuse. `src/process.rs` freezes both tags
/// and asserts they differ; this is the two of them meeting over a real pipe.
const OFFER_TAG: &[u8; 4] = b"EVR1";

/// Exited by a child that arrived in the slot it was promised and reported exactly that.
const REPORTED: i32 = 3;
/// Exited by a promised child that joined a slot of its own and reported *that* one.
const DIVERGED: i32 = 4;
/// Exited by a child that met the handoff and left without reporting anything.
const SILENT: i32 = 5;
/// Exited by a child that wrote an offer-shaped frame where its receipt would go.
const FORGED: i32 = 7;
/// Exited by a child that met, stayed connected and reported nothing until its parent gave up.
const PARKED: i32 = 8;
/// The roles this leg defines, kept beside their bodies. The claim root's single `role_entry` dispatches over
/// this table and the announcement leg's, because the harness re-enters a child by exact item name and only
/// one item in a target may answer to it.
pub(crate) const ROLES: &[(&str, role::Body)] = &[
    ("reported", reported),
    ("diverged", diverged),
    ("silent", silent),
    ("forged", forged),
    ("parked", parked),
];

/// The child that keeps its promise: it arrives in the slot it was announced, reports what it became and
/// leaves without releasing it.
fn reported() -> i32 {
    let (_, resources, report) = match Handoff::receive() {
        Ok(meeting) => meeting,
        Err(error) => return role::fail(&format!("the promised child meets no one: {error}")),
    };
    let Some(promised) = resources.seat else {
        return role::fail("a promised child met a handoff that announced it no member slot");
    };
    let arrived = match Session::open_as(
        resources.mapping,
        region::extent(SIZE),
        promised.region(),
        promised.peer(),
    ) {
        Ok(arrived) => arrived,
        Err(error) => {
            return role::fail(&format!("the announced slot refused its arrival: {error}"));
        }
    };
    if arrived.peer() != promised.peer() {
        return role::fail("the member that arrived is not the slot this process was promised");
    }
    // What is reported is read back out of this process's own session rather than echoed from the
    // announcement: an echo would tell the parent nothing it did not already hold.
    if report.arrived(arrived.region(), arrived.peer()).is_err() {
        return role::fail("the rendezvous closed before the arrival could be reported");
    }
    role::leave(REPORTED)
}

/// The child that breaks it: promised one slot, it joins as a member of its own and reports the identity
/// it actually took. The report is honest and still worthless, which is the point of the case.
fn diverged() -> i32 {
    let (_, resources, report) = match Handoff::receive() {
        Ok(meeting) => meeting,
        Err(error) => return role::fail(&format!("the diverging child meets no one: {error}")),
    };
    let promised = match resources.seat {
        Some(promised) => promised,
        None => {
            return role::fail("a promised child met a handoff that announced it no member slot");
        }
    };
    let joined = match Session::open(resources.mapping, region::extent(SIZE), promised.region()) {
        Ok(joined) => joined,
        Err(error) => return role::fail(&format!("the diverging child could not join: {error}")),
    };
    if joined.peer() == promised.peer() {
        return role::fail(
            "a child promised one slot arrived in it while meaning to report another",
        );
    }
    if report.arrived(joined.region(), joined.peer()).is_err() {
        return role::fail("the rendezvous closed before the divergent arrival could be reported");
    }
    role::leave(DIVERGED)
}

/// The child that meets and goes, saying nothing about what it became.
fn silent() -> i32 {
    let (_, resources, _report) = match Handoff::receive() {
        Ok(meeting) => meeting,
        Err(error) => {
            return role::fail(&format!(
                "the child that leaves wordless meets no one: {error}"
            ));
        }
    };
    if resources.seat.is_none() {
        return role::fail("a promised child met a handoff that announced it no member slot");
    }
    role::leave(SILENT)
}

/// The child that meets, keeps the instance open and reports nothing. This is the one silence a parent
/// cannot be told about by the host: the client has not gone anywhere, so the only way the read ends is
/// the way the caller said it would.
fn parked() -> i32 {
    let (_, resources, _report) = match Handoff::receive() {
        Ok(meeting) => meeting,
        Err(error) => {
            return role::fail(&format!(
                "the child that stays connected meets no one: {error}"
            ));
        }
    };
    if resources.seat.is_none() {
        return role::fail("a promised child met a handoff that announced it no member slot");
    }
    let mut doorbell = [resources.event];
    if Event::wait_any(&mut doorbell).is_err() {
        return role::fail("the doorbell handed over with the region cannot be waited on");
    }
    role::leave(PARKED)
}

/// The child that never meets at all and writes an offer-shaped frame into the rendezvous instead. The two
/// frames differ in front of a receipt read by their tag and by nothing else, so this is the one shape a
/// receipt reader must refuse while the writer behind it is credited and real.
///
/// The address is opened rather than met over, exactly as the Unix role reaches through the descriptor the
/// grant installed: what is being answered for is the read, and taking the handover first would put a
/// receipt writer in this process's hands. The offer is then waited for before leaving, because on this arm
/// a parent writing to a client that already went away is told the pipe broke — a different failure from
/// the one this case is about, and one the parent would see rather than the reader.
fn forged() -> i32 {
    let announced = match env::var(RENDEZVOUS) {
        Ok(announced) => announced,
        Err(_) => return role::fail("the forging child was granted no rendezvous to write into"),
    };
    let address: Vec<u16> = announced.encode_utf16().chain([0]).collect();
    // SAFETY: the pointer is a live null-terminated wide string this call owns, and the handle that comes
    // back is either the invalid one or an object this process now owns and closes on the way out.
    let opened = unsafe {
        CreateFileW(
            address.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            core::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            core::ptr::null_mut(),
        )
    };
    if opened == INVALID_HANDLE_VALUE {
        return role::fail(&format!(
            "the granted rendezvous could not be opened: {}",
            std::io::Error::last_os_error()
        ));
    }
    let rendezvous = unsafe { OwnedHandle::from_raw_handle(opened.cast()) };

    let mut frame = Vec::new();
    frame.extend_from_slice(OFFER_TAG);
    frame.extend_from_slice(&0u16.to_le_bytes());
    frame.extend_from_slice(&0u16.to_le_bytes());
    let mut written = 0;
    // SAFETY: the buffer outlives the call and its length is the count handed over with it.
    let ok = unsafe {
        WriteFile(
            rendezvous.as_raw_handle().cast(),
            frame.as_ptr().cast(),
            frame.len() as u32,
            &mut written,
            core::ptr::null_mut(),
        )
    };
    if ok == 0 || written as usize != frame.len() {
        return role::fail(&format!(
            "the forged frame never reached the rendezvous: {}",
            std::io::Error::last_os_error()
        ));
    }

    let mut offer = [0u8; 512];
    let mut read = 0;
    // SAFETY: as above, over a buffer this call owns for the duration of the read.
    let landed = unsafe {
        ReadFile(
            rendezvous.as_raw_handle().cast(),
            offer.as_mut_ptr().cast(),
            offer.len() as u32,
            &mut read,
            core::ptr::null_mut(),
        )
    };
    if landed == 0 {
        return role::fail(&format!(
            "the offer this child waited for never arrived: {}",
            std::io::Error::last_os_error()
        ));
    }
    role::leave(FORGED)
}

/// Admits `role` with `promised` announced to it and moves the region, a doorbell and a bell over the
/// meeting, keeping the parent's end of the instance and the ring that lets a parked child go. The command
/// is dropped on the way out for the same reason the Unix arm drops it, even though nothing of the
/// rendezvous travels in it here: one instance serves one client, so a second participant is the pipe's
/// problem rather than this caller's.
fn admit(
    grant: Grant,
    promised: Introduction,
    role: &str,
    handoff: Handoff,
    mapping: &Shared,
    bootstrap: &Bootstrap,
) -> (Supervisor, Attendance, Ring) {
    let mut command = role::child(role);
    grant
        .promising(promised)
        .admit(&mut command)
        .expect("admit a child to the meeting, naming the slot promised it");
    let child = Supervisor::spawn(&mut command).expect("spawn the promised child");
    drop(command);
    let (notify, child_wait) = evering::os::event().expect("a doorbell for the child");
    let (child_ring, _event) = evering::os::event().expect("a bell the child can ring");
    let attendance = handoff
        .send(
            &child,
            Instant::now() + Duration::from_secs(5),
            bootstrap,
            mapping,
            child_wait,
            &child_ring,
        )
        .expect("hand the region to the admitted child");
    (child, attendance, notify)
}

#[test]
fn a_member_a_child_reported_and_a_child_reaped_recover_over_the_pipe() {
    let (mapping, parent, handoff, grant, bootstrap) =
        region::meeting("receipt-reported", SIZE, REGION);
    let seat = parent.reserve_seat().expect("promise a member slot");
    let promised = seat.introduction();
    let (mut child, attendance, _notify) =
        admit(grant, promised, "reported", handoff, &mapping, &bootstrap);

    // The exit is taken first, which is the order the Unix file uses and this arm has now shown it can
    // keep: the instance still names its client, and the message it wrote is still queued to be read.
    assert_eq!(
        introduce::exit_code(&mut child, "the promised child", Duration::from_secs(10)),
        REPORTED,
        "the child arrived in the slot it was promised, reported it and left the member standing"
    );
    let arrival = attendance
        .arrival(&child, promised, Instant::now() + Duration::from_secs(5))
        .expect("the receipt of the child this grant promised");
    assert_eq!(
        (arrival.region(), arrival.peer(), arrival.pid()),
        (promised.region(), promised.peer(), child.id()),
        "an arrival is the promised identity beside the process the host credited with reporting it"
    );

    let exit = child.wait().expect("the status of a child already reaped");
    assert_eq!(exit.status().code(), Some(REPORTED));
    assert_eq!(exit.pid(), arrival.pid());
    let death = Death::witnessed(&arrival, &exit).expect("one process reported and then died");
    assert_eq!(
        (death.region(), death.peer(), death.pid()),
        (promised.region(), promised.peer(), child.id())
    );

    // The halves are the point of the type: an arrival read off one child and an exit taken from another
    // name two processes, and the reap they would authorize belongs to neither.
    let mut decoy = {
        let mut command =
            Command::new(env::current_exe().expect("the test binary is its own parent"));
        command.args(["--exact", "no-role-runs-in-the-parent"]);
        Supervisor::spawn(&mut command).expect("spawn a process admitted to nothing")
    };
    let decoy_exit = decoy.wait().expect("reap the decoy");
    let mismatched = Death::witnessed(&arrival, &decoy_exit)
        .expect_err("a receipt from one child and an exit of another compose into a death");
    assert_eq!(mismatched.kind(), ErrorKind::PermissionDenied);

    let recovery = parent
        .recover(&death)
        .expect("the promised slot was stood in, so this roster owes a reap");
    assert_eq!(recovery.peer(), promised.peer());
    if let Err(unreaped) = recovery.reap() {
        panic!(
            "a member that published nothing still finished its reap: {:?}",
            unreaped.peer()
        );
    }
    assert!(
        parent.recover(&death).is_none(),
        "a reaped identity is spent and cannot be reaped a second time"
    );

    // The same evidence in front of another roster is nothing at all: a `Death` answers for one region.
    let other = shared("receipt-other", SIZE, region::access()).expect("a second region");
    let outsider = Session::create(other.borrow(), region::extent(SIZE), OTHER)
        .expect("a session that was never in this meeting");
    assert!(
        outsider.recover(&death).is_none(),
        "a death about one region reached into another"
    );

    // The slot came back through the reap, not through the promise: the child never released its member
    // and the parent never gave its `Seat` back before this point.
    drop(seat);
    let later = Session::open(mapping.borrow(), region::extent(SIZE), REGION)
        .expect("join the slot a reaped child left");
    assert_eq!(
        later.peer().slot(),
        promised.peer().slot(),
        "a completely reaped slot becomes reusable"
    );
    assert!(
        later.peer().generation() > promised.peer().generation(),
        "slot reuse cannot recreate the identity the death released"
    );
}

#[test]
fn a_report_of_a_member_nobody_promised_that_child_is_refused_over_the_pipe() {
    let (mapping, parent, handoff, grant, bootstrap) =
        region::meeting("receipt-diverged", SIZE, REGION);
    let seat = parent.reserve_seat().expect("promise a member slot");
    let promised = seat.introduction();
    let (mut child, attendance, _notify) =
        admit(grant, promised, "diverged", handoff, &mapping, &bootstrap);

    assert_eq!(
        introduce::exit_code(&mut child, "the diverging child", Duration::from_secs(10)),
        DIVERGED,
        "the child took a slot of its own and reported that one"
    );
    let refused =
        match attendance.arrival(&child, promised, Instant::now() + Duration::from_secs(5)) {
            Ok(arrival) => panic!(
                "a receipt naming {:?} was read as the arrival of {:?}",
                arrival.peer(),
                promised.peer()
            ),
            Err(error) => error,
        };
    assert_eq!(
        refused.kind(),
        ErrorKind::PermissionDenied,
        "the report was refused for a different reason: {refused}"
    );

    // The refusal keeps a reap away from somebody else's member: the parent holds no `Arrival`, so it
    // holds no `Death`, and what the child actually took is a live roster entry behind a dead process that
    // this crate cannot name for the caller.
    let exit = child
        .wait()
        .expect("reap the child that reported somewhere else");
    assert!(!exit.success(), "the diverging child leaves by exit code");

    // And the promise the refused report did not fill is the parent's to spend or give back, unchanged.
    drop(seat);
    let imposter = Session::open_as(
        mapping.borrow(),
        region::extent(SIZE),
        REGION,
        promised.peer(),
    );
    let Err(refused) = imposter else {
        panic!("a process arrived in the member slot a refused report never filled");
    };
    assert!(
        matches!(refused, SessionError::ArrivalRefused),
        "the promised identity was refused, but for a different reason: {refused}"
    );
}

#[test]
fn a_child_that_met_and_never_reported_is_answered_by_the_instance_itself() {
    let (mapping, parent, handoff, grant, bootstrap) =
        region::meeting("receipt-silent", SIZE, REGION);
    let seat = parent.reserve_seat().expect("promise a member slot");
    let promised = seat.introduction();
    let (mut child, attendance, _notify) =
        admit(grant, promised, "silent", handoff, &mapping, &bootstrap);

    assert_eq!(
        introduce::exit_code(
            &mut child,
            "the child that left wordless",
            Duration::from_secs(10)
        ),
        SILENT,
        "the promised child met the handover and reported nothing"
    );
    // The bound is deliberately generous, because the answer must not be reached through it: this arm's
    // instance reports the client's departure, so the read refuses at once and never waits to be told by
    // the caller to give up. The Unix arm's datagram rendezvous says nothing about a peer that has gone,
    // so there the same silence ends at exactly the deadline the caller named.
    let silence = attendance
        .arrival(&child, promised, Instant::now() + Duration::from_secs(5))
        .expect_err("a child that says nothing hands its parent an arrival");
    assert_eq!(
        silence.kind(),
        ErrorKind::UnexpectedEof,
        "the wait ended on something other than the instance telling it nobody is left: {silence}"
    );
    assert!(
        unsafe { parent.assume_dead(promised.peer()) }.is_none(),
        "a promised process that never stood in the slot left a promise, not a member"
    );
    drop(seat);
}

#[test]
fn an_offer_shaped_frame_is_refused_where_the_pipe_reads_a_receipt() {
    let (mapping, parent, handoff, grant, bootstrap) =
        region::meeting("receipt-forged", SIZE, REGION);
    let seat = parent.reserve_seat().expect("promise a member slot");
    let promised = seat.introduction();
    let (mut child, attendance, _notify) =
        admit(grant, promised, "forged", handoff, &mapping, &bootstrap);

    assert_eq!(
        introduce::exit_code(&mut child, "the forging child", Duration::from_secs(10)),
        FORGED,
        "the child wrote an offer-shaped frame down the rendezvous"
    );
    let malformed = attendance
        .arrival(&child, promised, Instant::now() + Duration::from_secs(5))
        .expect_err("a frame with the offer's tag was read as an arrival");
    assert_eq!(
        malformed.kind(),
        ErrorKind::InvalidData,
        "the frame was refused for a different reason: {malformed}"
    );
    // The writer behind that frame was credited — the read refuses the *frame*, not the caller — so the
    // two tags are what keeps a handover's answer from being mistaken for a handover. Nothing is settled
    // about the slot either way: it stays the promise's.
    assert!(
        unsafe { parent.assume_dead(promised.peer()) }.is_none(),
        "a refused frame left a member nobody arrived in"
    );
    drop(seat);
}

#[test]
fn a_child_still_connected_and_silent_is_given_up_on_at_the_bound_the_caller_named() {
    let (mapping, parent, handoff, grant, bootstrap) =
        region::meeting("receipt-parked", SIZE, REGION);
    let seat = parent.reserve_seat().expect("promise a member slot");
    let promised = seat.introduction();
    let (mut child, attendance, notify) =
        admit(grant, promised, "parked", handoff, &mapping, &bootstrap);

    // The other face of a child that will not report: the client has not gone, so the instance has no
    // departure to answer with and the read ends on the caller's word alone. The Unix arm reaches this
    // answer for *every* silent child, because its rendezvous reports nothing about a departed peer.
    let silence = attendance
        .arrival(
            &child,
            promised,
            Instant::now() + Duration::from_millis(500),
        )
        .expect_err("a child that is connected and says nothing hands its parent an arrival");
    assert_eq!(
        silence.kind(),
        ErrorKind::TimedOut,
        "the wait ended on something other than its own bound: {silence}"
    );

    notify
        .notify()
        .expect("release the child the receipt read gave up on");
    assert_eq!(
        introduce::exit_code(&mut child, "the parked child", Duration::from_secs(10)),
        PARKED,
        "the promised child met, held the instance open and reported nothing"
    );

    // Giving up on the read settles nothing about the slot: it stays the promise's, exactly as it was
    // while the child was deciding not to report.
    assert!(
        unsafe { parent.assume_dead(promised.peer()) }.is_none(),
        "a promised process the caller gave up on never stood in the slot"
    );
    drop(seat);
}
