#![cfg(all(feature = "os", unix))]

//! The return leg of a handover: the process a grant admitted writes down on the same rendezvous which
//! member it became, and the parent that promised it a slot reads that answer, is told by the kernel who
//! wrote it, and only then — after reaping the child — holds a death it may spend on the roster.
//! `tests/seat_announcement.rs` stops one step earlier, at the announcement going *out*; nothing there can
//! show what comes back, and the difference is what these four cases are about.
//!
//! No application byte is written by either process. Neither makes a pool, a channel or a token: the only
//! bytes that cross are the offer's bootstrap and the receipt's text, and the region's block space holds
//! nothing but the two roster entries its members made. That is deliberate — the whole claim of the receipt
//! is that a parent can learn *who stood in which slot* from the host alone, so a test that also carried the
//! identity in application data would not show which of the two the reap rested on.
//!
//! What is not evidence here: that an arrival is a death, which needs the reaped exit beside it
//! (`Death::witnessed` compares the two pids and is the only way to hold a `Death` at all); that the host
//! refuses a writer naming somebody else, which is a property of `SCM_CREDENTIALS` that the seam documents
//! and no test in this crate re-proves; and which member a death reached for a process that was never
//! promised anything, since an unpromised child has no receipt to reconcile and its parent is left with
//! [`assume_dead`](crate::Session::assume_dead). The third case is where a receipt buys a caller the least
//! and says so: a child that reports nothing gives the parent nothing to refuse either.
//!
//! One obligation is the parent's rather than the crate's: the `Command` a grant was admitted into holds the
//! parent's own copy of the granted end until it is dropped, so a parent that means to have exactly one child
//! on a rendezvous lets go of the command as soon as it has spawned it. What dropping it does not buy is what
//! the third case measures: on a datagram rendezvous the host reports a peer's departure as nothing at all, so
//! a child that met, said nothing and exited is answered by the caller's own deadline — the same answer a slow
//! child gets — and it is the reap, not the socket, that tells a parent its child is gone. The Windows leg
//! is `tests/arrival_receipt_windows.rs`: the same three answers, in the same order, off the pipe instance
//! rather than a field beside the message, and with the difference this paragraph is about — a departed
//! child is something that instance can report, so the bound is left to the silence of a client that has
//! not gone anywhere.

use std::{
    env,
    io::ErrorKind,
    os::unix::{
        io::{FromRawFd, RawFd},
        net::UnixDatagram,
    },
    process::Command,
    time::{Duration, Instant},
};

use evering::{
    Session, SessionError,
    layout::RegionId,
    os::{Attendance, Grant, Handoff, Shared, shared},
    process::{Bootstrap, Death, Introduction, Supervisor},
};

mod support;
use crate::support::{introduce, region, role};

const SIZE: usize = 1 << 20;
const REGION: RegionId = RegionId::new(0x0053_E490, 1);
/// A second region, made by the same parent, which a death about [`REGION`] must never reach.
const OTHER: RegionId = RegionId::new(0x0053_E491, 1);
/// The key `src/process.rs` freezes for the granted descriptor, named here so a child can write into the
/// rendezvous through it without meeting over it.
const RENDEZVOUS: &str = "EVERING_RENDEZVOUS";
/// The offer's tag, which is the frame a receipt read exists to refuse. `src/process.rs` freezes both tags
/// and asserts they differ; this is the two of them meeting over a real socket.
const OFFER_TAG: &[u8; 4] = b"EVR1";

/// Exited by a child that arrived in the slot it was promised and reported exactly that.
const REPORTED: i32 = 3;
/// Exited by a promised child that joined a slot of its own and reported *that* one.
const DIVERGED: i32 = 4;
/// Exited by a child that met the handoff and left without reporting anything.
const SILENT: i32 = 5;
/// Exited by a child that wrote an offer-shaped frame where its receipt would go.
const FORGED: i32 = 7;
/// The child's whole program: the roles this claim defines, and nothing else.
#[test]
fn role_entry() {
    role::dispatch(&[
        ("reported", reported),
        ("diverged", diverged),
        ("silent", silent),
        ("forged", forged),
    ]);
}

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

/// The child that never meets at all and writes an offer-shaped frame into the rendezvous instead. The two
/// frames differ in front of a receipt read by their tag and by nothing else, so this is the one shape a
/// receipt reader must refuse while the writer behind it is credited and real.
fn forged() -> i32 {
    let announced = match env::var(RENDEZVOUS) {
        Ok(announced) => announced,
        Err(_) => return role::fail("the forging child was granted no rendezvous to write into"),
    };
    let raw: RawFd = match announced.parse() {
        Ok(raw) => raw,
        Err(_) => return role::fail("the announced rendezvous is not a descriptor number"),
    };
    let mut frame = Vec::new();
    frame.extend_from_slice(OFFER_TAG);
    frame.extend_from_slice(&0u16.to_le_bytes());
    frame.extend_from_slice(&3u16.to_le_bytes());
    // SAFETY: `raw` names the descriptor the grant installed in this process, which nothing else here owns;
    // the process leaves through `leave`, so it is closed exactly once.
    let rendezvous = unsafe { UnixDatagram::from_raw_fd(raw) };
    match rendezvous.send(&frame) {
        Ok(_) => role::leave(FORGED),
        Err(error) => role::fail(&format!(
            "the forged frame never reached the rendezvous: {error}"
        )),
    }
}

/// Admits `role` with `promised` announced to it and moves the region, a doorbell and a bell over the
/// meeting, keeping the parent's end of the rendezvous. The command is dropped on the way out, because the
/// parent's own copy of the granted descriptor is otherwise a second writer that never reports.
fn admit(
    grant: Grant,
    promised: Introduction,
    role: &str,
    handoff: Handoff,
    mapping: &Shared,
    bootstrap: &Bootstrap,
) -> (Supervisor, Attendance) {
    let mut command = role::child(role);
    grant
        .promising(promised)
        .admit(&mut command)
        .expect("admit a child to the meeting, naming the slot promised it");
    let child = Supervisor::spawn(&mut command).expect("spawn the promised child");
    drop(command);
    let (_notify, child_wait) = evering::os::event().expect("a doorbell for the child");
    let (child_ring, _event) = evering::os::event().expect("a bell the child can ring");
    let attendance = handoff
        .send(
            &child,
            Instant::now() + Duration::from_secs(5),
            bootstrap,
            mapping,
            &child_wait,
            &child_ring,
        )
        .expect("hand the region to the admitted child");
    (child, attendance)
}

#[test]
fn a_member_a_child_reported_and_a_child_reaped_recover_without_an_application_byte() {
    let (mapping, parent, handoff, grant, bootstrap) =
        region::meeting("receipt-reported", SIZE, REGION);
    let seat = parent.reserve_seat().expect("promise a member slot");
    let promised = seat.introduction();
    let (mut child, attendance) = admit(grant, promised, "reported", handoff, &mapping, &bootstrap);

    // The exit is taken first, so by the time the parent holds both halves the process the arrival is
    // credited to is already proven gone and its own mappings are unreachable.
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

    // The halves are the point of the type, so a decoy is worth a process: an arrival read off one child
    // and an exit taken from another name two processes, and the reap they would authorize belongs to
    // neither. The decoy is spawned while the promised child is still unreaped, so no pid is reused.
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

    // The same evidence in front of another roster is nothing at all: a `Death` answers for one region,
    // and a process holding mappings of two cannot be assumed to have asked the right session.
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
fn a_report_of_a_member_nobody_promised_that_child_is_refused_and_authorizes_nothing() {
    let (mapping, parent, handoff, grant, bootstrap) =
        region::meeting("receipt-diverged", SIZE, REGION);
    let seat = parent.reserve_seat().expect("promise a member slot");
    let promised = seat.introduction();
    let (mut child, attendance) = admit(grant, promised, "diverged", handoff, &mapping, &bootstrap);

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

    // The refusal is not a failure to see the child: it is the only answer that keeps a reap away from
    // somebody else's member. The parent holds no `Arrival`, so it holds no `Death`, and `recover` has
    // nothing to be handed. What the child actually took is now a live roster entry behind a dead process
    // that this crate cannot name for the parent — reaching it is `assume_dead`, on the caller's word.
    let exit = child
        .wait()
        .expect("reap the child that reported somewhere else");
    assert!(!exit.success(), "the diverging child leaves by exit code");

    // And the promise the refused report did not fill is the parent's to spend or give back, unchanged:
    // a generation a promised process never arrived in is still spent by the promise being cancelled.
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
fn a_child_that_met_and_never_reported_is_given_up_on_at_the_bound_the_caller_named() {
    let (mapping, parent, handoff, grant, bootstrap) =
        region::meeting("receipt-silent", SIZE, REGION);
    let seat = parent.reserve_seat().expect("promise a member slot");
    let promised = seat.introduction();
    let (mut child, attendance) = admit(grant, promised, "silent", handoff, &mapping, &bootstrap);

    assert_eq!(
        introduce::exit_code(
            &mut child,
            "the child that left wordless",
            Duration::from_secs(10)
        ),
        SILENT,
        "the promised child met the handover and reported nothing"
    );
    // The child is reaped and gone, and the rendezvous still has nothing to say about it: measured on this
    // host, a datagram pair reports a peer's departure as no readiness and no hangup at all, so the bound is
    // the only end a receipt read can be given and a slow child gets the same answer as a silent one. That
    // is why `Death` takes the reap beside the arrival — the socket was never going to report the death.
    let silence = attendance
        .arrival(
            &child,
            promised,
            Instant::now() + Duration::from_millis(500),
        )
        .expect_err("a child that says nothing hands its parent an arrival");
    assert_eq!(
        silence.kind(),
        ErrorKind::TimedOut,
        "the wait ended on something other than its own bound: {silence}"
    );
    assert!(
        unsafe { parent.assume_dead(promised.peer()) }.is_none(),
        "a promised process that never stood in the slot left a promise, not a member"
    );
    drop(seat);
}

#[test]
fn a_frame_shaped_like_the_offer_is_refused_where_a_receipt_is_read() {
    let (mapping, parent, handoff, grant, bootstrap) =
        region::meeting("receipt-forged", SIZE, REGION);
    let seat = parent.reserve_seat().expect("promise a member slot");
    let promised = seat.introduction();
    let (mut child, attendance) = admit(grant, promised, "forged", handoff, &mapping, &bootstrap);

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
