#![cfg(all(feature = "os", unix))]

//! What the promise looks like across a process boundary: one process reserves a member slot, admits
//! a child with it, and the child — a second mapping of a region it only ever received as a descriptor
//! — arrives in exactly the slot it was told about. `src/tests/unix/talc.rs` covers the same
//! transitions between two mappings of one region inside one process; nothing there can show what a
//! child reads out of its own environment, which is the half these four cases are about.
//!
//! The identity each child checks against arrives out of band, as `slot:generation` in a second key.
//! A child that compared an announcement against nothing would also pass if the transport swapped the
//! slot and the generation, and a swapped pair is exactly the failure this step could introduce. What
//! is *not* evidence here: that a promise is a secret — any process that can map the region joins as a
//! member of its own, and the roster refuses a displaced member, not an eavesdropper; and what a
//! promise does while its member is still standing, since a child on its way out releases the slot it
//! arrived in before the parent gives the promise back, which is the case
//! `src/tests/unix/talc.rs` holds open instead. Nothing on the Windows leg has been run on that host
//! either; it writes the same key in the same order.
//!
//! The fourth case is the one where the death machinery has nothing to do, and that is as far as it
//! goes: a promised child that met and then died before arriving leaves the roster holding a promise
//! rather than a member, so the slot comes back through the parent's `Seat` alone. What the transport
//! cannot yet say is which member a death *did* reach: an announced identity is the parent's word
//! about a slot, not a record of who stood in it, so binding a reaped child to the member it became is
//! a decision of its own and no part of this file can make it. Reaping an identity a caller can name
//! for itself is what `src/tests/unix/recovery.rs` and `tests/ipc_process.rs` show.

use std::{
    env,
    process::Command,
    thread,
    time::{Duration, Instant},
};

use evering::{
    Session, SessionError,
    layout::RegionId,
    mapping::{Access, Peer, Request},
    os::{Grant, Handoff, Shared, shared},
    process::{Bootstrap, Supervisor},
};

const SIZE: usize = 1 << 20;
const REGION: RegionId = RegionId::new(0x0053_E480, 1);
const ROLE: &str = "EVERING_SEAT_ROLE";
/// The identity to check against, named by the parent and carried nowhere by the crate.
const EXPECT: &str = "EVERING_SEAT_EXPECT";
/// The key `src/process.rs` freezes for a promised slot, named here so a scribble can be written to it.
const SEAT: &str = "EVERING_SEAT";

/// Exited by a child that arrived in the slot it was announced and became that member.
const ARRIVED: i32 = 3;
/// Exited by a child that met without an announcement and joined as a member of its own.
const UNANNOUNCED: i32 = 4;
/// Exited by a child refused a meeting it was handed a scribbled announcement for.
const SCRIBBLE_REFUSED: i32 = 5;
/// Exited by a promised child that met and then died without arriving in the slot it was told of.
const VANISHED: i32 = 6;
/// Exited by a child for any other reason, which the line printed beside it names.
const FAILED: i32 = 1;

fn fail(reason: &str) -> i32 {
    eprintln!("{reason}");
    FAILED
}

fn expected() -> Peer {
    let named = env::var(EXPECT).expect("the child is told out of band which member to expect");
    let mut fields = named.split(':');
    let slot = fields.next().unwrap().parse().unwrap();
    let generation = fields.next().unwrap().parse().unwrap();
    Peer::from_parts(slot, generation)
}

fn announced() -> i32 {
    let (_, resources, _report) = match Handoff::receive() {
        Ok(meeting) => meeting,
        Err(error) => return fail(&format!("the promised child meets no one: {error}")),
    };
    let Some(seat) = resources.seat else {
        return fail("no member slot was announced to the child it was promised to");
    };
    if (seat.region(), seat.peer()) != (REGION, expected()) {
        return fail(&format!(
            "the announcement named region {:?} slot {} at generation {}, and the parent promised it {:?}",
            seat.region(),
            seat.peer().slot(),
            seat.peer().generation(),
            expected()
        ));
    }
    let arrived = match Session::open_as(
        resources.mapping,
        Request::new(SIZE, Access::READ | Access::WRITE),
        seat.region(),
        seat.peer(),
    ) {
        Ok(arrived) => arrived,
        Err(error) => {
            return fail(&format!(
                "the slot announced to this process refused its arrival: {error}"
            ));
        }
    };
    if arrived.peer() != seat.peer() {
        return fail(&format!(
            "the member that arrived is {:?}, not the announced {:?}",
            arrived.peer(),
            seat.peer()
        ));
    }
    ARRIVED
}

fn unannounced() -> i32 {
    let (_, resources, _report) = match Handoff::receive() {
        Ok(meeting) => meeting,
        Err(error) => return fail(&format!("the unpromised child meets no one: {error}")),
    };
    if resources.seat.is_some() {
        return fail("a grant that promised nothing announced a member slot anyway");
    }
    let joined = match Session::open(
        resources.mapping,
        Request::new(SIZE, Access::READ | Access::WRITE),
        REGION,
    ) {
        Ok(joined) => joined,
        Err(error) => return fail(&format!("the unpromised child could not join: {error}")),
    };
    if joined.peer() == expected() {
        return fail(&format!(
            "a child promised nothing arrived as the member already standing in {:?}",
            joined.peer()
        ));
    }
    UNANNOUNCED
}

fn scribbled() -> i32 {
    match Handoff::receive() {
        Ok(_) => fail("a scribbled slot announcement was met as if it named a slot"),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => SCRIBBLE_REFUSED,
        Err(error) => fail(&format!(
            "the scribbled announcement was refused for a different reason: {error}"
        )),
    }
}

fn vanished() -> i32 {
    let (_, resources, _report) = match Handoff::receive() {
        Ok(meeting) => meeting,
        Err(error) => return fail(&format!("the child that was to die meets no one: {error}")),
    };
    if resources.seat.is_none() {
        return fail("a promised child met the handoff it was never announced a slot for");
    }
    // Meeting and leaving is the whole role. The region this process was handed closes with it, and
    // the slot its announcement named is one it never stood in.
    VANISHED
}

fn wait_for(child: &mut Supervisor, label: &str, budget: Duration) -> i32 {
    let until = Instant::now() + budget;
    loop {
        if let Some(exit) = child.try_wait().unwrap() {
            return exit.status().code().unwrap_or(-1);
        }
        if Instant::now() >= until {
            let _ = child.kill_wait();
            panic!("{label} never finished");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// A region, the session that made it, and the meeting a child is admitted to.
fn meeting(name: &str) -> (Shared, Session, Handoff, Grant, Bootstrap) {
    let access = Access::READ | Access::WRITE;
    let mapping = shared(name, SIZE, access).expect("a region to hold a member in");
    let session = Session::create(mapping.borrow(), Request::new(SIZE, access), REGION)
        .expect("create the session that admits");
    let (handoff, grant) = Handoff::bind().expect("a meeting to admit a child to");
    let bootstrap = Bootstrap::new(name.as_bytes()).expect("a bootstrap within the ceiling");
    (mapping, session, handoff, grant, bootstrap)
}

/// The test binary, re-entering as one role of one case.
fn child_command(test: &str, role: &str) -> Command {
    let mut command = Command::new(env::current_exe().expect("the test binary is its own parent"));
    command
        .args(["--exact", test])
        .env(ROLE, role)
        .stdout(std::process::Stdio::null());
    command
}

/// Moves the region, a doorbell and a bell to the process the grant was admitted to. Neither bell is
/// rung: what these cases ask of the child is an exit code, and the objects go along because a
/// handover carries them.
fn deliver(handoff: Handoff, child: &Supervisor, mapping: &Shared, bootstrap: &Bootstrap) {
    let (_notify, child_wait) = evering::os::event().expect("a doorbell for the child");
    let (child_ring, _event) = evering::os::event().expect("a bell the child can ring");
    handoff
        .send(
            child,
            Instant::now() + Duration::from_secs(5),
            bootstrap,
            mapping,
            &child_wait,
            &child_ring,
        )
        .expect("hand the region to the admitted child");
}

#[test]
fn a_slot_announced_over_a_real_handoff_is_arrived_in_by_the_child_it_was_announced_to() {
    const TEST: &str =
        "a_slot_announced_over_a_real_handoff_is_arrived_in_by_the_child_it_was_announced_to";
    if env::var(ROLE).ok().as_deref() == Some("announced") {
        std::process::exit(announced());
    }

    let (mapping, parent, handoff, grant, bootstrap) = meeting("seat-announced");
    let seat = parent.reserve_seat().expect("promise a member slot");
    let promised = seat.peer();

    let mut command = child_command(TEST, "announced");
    command.env(
        EXPECT,
        format!("{}:{}", promised.slot(), promised.generation()),
    );
    grant
        .promising(seat.introduction())
        .admit(&mut command)
        .expect("admit a child to the meeting, naming the slot promised it");
    let mut child = Supervisor::spawn(&mut command).expect("spawn the promised child");
    deliver(handoff, &child, &mapping, &bootstrap);

    assert_eq!(
        wait_for(&mut child, "the promised child", Duration::from_secs(10)),
        ARRIVED,
        "the child the slot was promised to is the one that arrives in it"
    );

    // The child arrived and left: `announced` returns before it exits, so the member released its own
    // slot while the parent still held a promise over it. Giving that promise back afterwards changes
    // nothing, and what the next joiner finds is the slot again at a generation the arrival spent —
    // which is the promise's whole purpose from the other side: the identity it named is answerable
    // for once, by one process.
    drop(seat);
    let later = Session::open(
        mapping.borrow(),
        Request::new(SIZE, Access::READ | Access::WRITE),
        REGION,
    )
    .expect("join a third member after the promised one came and went");
    assert_eq!(
        later.peer().slot(),
        promised.slot(),
        "a slot its member left is the next joiner's to take"
    );
    assert!(
        later.peer().generation() > promised.generation(),
        "{:?} repeats the identity {:?} that already arrived in it",
        later.peer(),
        promised
    );
}

#[test]
fn a_handoff_that_promised_nothing_announces_no_member_slot() {
    const TEST: &str = "a_handoff_that_promised_nothing_announces_no_member_slot";
    if env::var(ROLE).ok().as_deref() == Some("unannounced") {
        std::process::exit(unannounced());
    }

    let (mapping, parent, handoff, grant, bootstrap) = meeting("seat-unannounced");
    let held = parent.peer();

    let mut command = child_command(TEST, "unannounced");
    command.env(EXPECT, format!("{}:{}", held.slot(), held.generation()));
    grant
        .admit(&mut command)
        .expect("admit a child the session promised nothing to");
    let mut child = Supervisor::spawn(&mut command).expect("spawn the unpromised child");
    deliver(handoff, &child, &mapping, &bootstrap);

    assert_eq!(
        wait_for(&mut child, "the unpromised child", Duration::from_secs(10)),
        UNANNOUNCED,
        "an admitted process with no promise is told nothing and joins as a member of its own"
    );
}

#[test]
fn a_scribbled_member_slot_announcement_is_refused_as_the_text_it_is() {
    const TEST: &str = "a_scribbled_member_slot_announcement_is_refused_as_the_text_it_is";
    if env::var(ROLE).ok().as_deref() == Some("scribbled") {
        std::process::exit(scribbled());
    }

    let (mapping, _parent, handoff, grant, bootstrap) = meeting("seat-scribbled");

    let mut command = child_command(TEST, "scribbled");
    grant
        .admit(&mut command)
        .expect("admit a child before its announcement is scribbled over");
    // A grant that promised nothing writes no such key, so this is the shape of an environment some
    // process edited rather than one a session made: a real meeting, an announcement that names no
    // slot. The child holds the descriptor, so it meets — and is refused for the text alone.
    command.env(SEAT, "not-a-slot");
    let mut child = Supervisor::spawn(&mut command).expect("spawn the scribbling child");
    deliver(handoff, &child, &mapping, &bootstrap);

    assert_eq!(
        wait_for(&mut child, "the scribbled child", Duration::from_secs(10)),
        SCRIBBLE_REFUSED,
        "an unreadable announcement is refused rather than arrived at"
    );
}

#[test]
fn a_promised_child_that_dies_between_meeting_and_arriving_leaves_nothing_to_reap() {
    const TEST: &str =
        "a_promised_child_that_dies_between_meeting_and_arriving_leaves_nothing_to_reap";
    if env::var(ROLE).ok().as_deref() == Some("vanished") {
        std::process::exit(vanished());
    }

    let (mapping, parent, handoff, grant, bootstrap) = meeting("seat-vanished");
    let seat = parent.reserve_seat().expect("promise a member slot");
    let promised = seat.peer();

    let mut command = child_command(TEST, "vanished");
    grant
        .promising(seat.introduction())
        .admit(&mut command)
        .expect("admit a child to the meeting, naming the slot promised it");
    let mut child = Supervisor::spawn(&mut command).expect("spawn the promised child");
    deliver(handoff, &child, &mapping, &bootstrap);

    assert_eq!(
        wait_for(
            &mut child,
            "the child that never arrived",
            Duration::from_secs(10)
        ),
        VANISHED,
        "the promised process met and left, and it is the reaped status — not a poll — that says so"
    );

    // The death is as authoritative as this crate lets it get: the kernel has reported this exact
    // child gone. It still gives the roster nothing to do — nobody materialised in the promised slot,
    // so there is no member to mark, no records to requeue, and no recovery for a caller to run. The
    // slot is the promise's to give back and nothing else's to reap.
    assert!(
        unsafe { parent.assume_dead(promised) }.is_none(),
        "a promised process that died before arriving left a promise, not a member"
    );

    drop(seat);
    let later = Session::open(
        mapping.borrow(),
        Request::new(SIZE, Access::READ | Access::WRITE),
        REGION,
    )
    .expect("join the slot a dead child's promise gave back");
    assert_eq!(
        later.peer().slot(),
        promised.slot(),
        "the slot the dead child never stood in is the next joiner's to take"
    );
    assert!(
        later.peer().generation() > promised.generation(),
        "a promise spends its generation even when the process it was promised to dies"
    );

    // Somebody else could have read the dead child's announcement — that is the leak this feature
    // documents rather than closes — and it buys that process nothing: the promise spent that
    // generation, so the identity the text names is nobody's to arrive in any more.
    let imposter = Session::open_as(
        mapping.borrow(),
        Request::new(SIZE, Access::READ | Access::WRITE),
        REGION,
        promised,
    );
    let Err(refused) = imposter else {
        panic!("a process arrived in the member identity a dead child was promised");
    };
    assert!(
        matches!(refused, SessionError::ArrivalRefused),
        "the spent identity was refused, but for a different reason: {refused}"
    );
}
