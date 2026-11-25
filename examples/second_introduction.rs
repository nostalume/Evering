//! The second introduction route, measured against the API as it ships.
//!
//! Every handover today begins with [`Handoff::bind`], which makes a meeting and the one grant that
//! admits a process to it, and [`Grant::admit`], which puts that grant into a `Command` this process
//! is about to spawn. Card 3.4's second target and the A4 gate item both ask for a meeting that does
//! not have that shape: two processes reach each other where **neither one spawned the other**, and
//! the process taking the handover learns the rendezvous by a route of its own instead of reading an
//! announcement somebody else wrote into its environment. This program is that meeting, written
//! without touching the library, and what it delivers is the list of places the shipped API will not
//! bend — the friction list the A4 item asks for before any core change is admitted.
//!
//! Three signatures were predicted to be the ones that bite:
//!
//! 1. [`Grant::admit`], which defines "grant" as modifying one spawn.
//! 2. [`Handoff::receive`], which takes no argument and reads the environment instead, so a target
//!    that learned the address itself has to write the announcement into its own process — under
//!    edition 2024 that is `unsafe`.
//! 3. The Unix arm of the attestation, which asserts that the peer behind the rendezvous *is this
//!    process's parent*.
//!
//! The weld this measurement fed has since landed, and round three below runs the route as the
//! shipped seam now opens it: [`Handoff::granted`] takes the rendezvous as a descriptor rather than a
//! number read out of an announcement, which removes blockage 2 as a way in — the target never
//! writes a key for itself in the round that matters — and it also removes the fourth place in its
//! predicted shape. There is still no constructor that turns a rendezvous somebody can look up into
//! a [`Handoff`], but none is needed on the sender's side: an introducer makes its *own* meeting with
//! [`Handoff::bind`] and moves the grant across as a descriptor over the address the target published,
//! the same SCM route the untyped legs here already use. What survives of prediction 3 is the whole
//! verdict: on Linux the typed entrance classifies the descriptor, asks it the one kernel question,
//! and then refuses a peer the kernel names truthfully but who is not the reader's parent. And
//! prediction 1 survives in a new place, because `Handoff::send` still demands a `&Supervisor` even
//! where the arm ignores it — named below by a process this introducer spawned for nothing else.
//!
//! The two hosts then stop in different places, and the example prints which:
//!
//! - On Windows the blockage is on the **sending** side, and round three is not even run: the host
//!   has no placed entrance, and handles reach only a process the sender duplicated into — the one
//!   named by the same `&Supervisor` whose client check `Listener::accept` applies. The introducer
//!   has to spawn an unrelated process just to have something to name, and the target waits on a
//!   handle nobody can write.
//! - On Linux the sending side completes: the grant crosses the learned address, the typed send is
//!   accepted, and an `Attendance` is held. The route then stops on the **receiving** side at the
//!   attestation sentence alone, with the introducer's true pid in the kernel's answer. Whether that
//!   assertion may name a non-parent is the provider decision this measurement feeds, and its shape
//!   is now measured rather than guessed: a pidfd delivered beside the grant witnesses the introducer
//!   and turns readable when it exits, but watches without reaping.
//!
//! # Running
//!
//! ```text
//! cargo run --features os --example second_introduction
//! ```
//!
//! With no arguments the program is the launcher: it makes a directory under the host's temp
//! directory, spawns the introducer and the target into it as **its own** two children — so neither
//! is the other's parent, which is the topology being measured — and reports each one's exit. The
//! roles `--introduce` and `--arrive` exist for a human to run under a debugger; they are what the
//! launcher spawns. Every wait one role has on another is bounded by a budget, and the launcher also
//! carries a longer one for each child, because a leg can end only inside a wait the seam does not
//! bound — that is the case the budgets are written for. On both hosts today every role exits on its
//! own, and the launcher reports the two codes rather than a kill.
//!
//! # What this does not show
//!
//! It shows where the route is refused, on the two hosts this repository executes. What is *not* an
//! observed outcome is a completed meeting — a target that takes the handover and is attested to the
//! introducer that fed it — so the example exits nonzero; that is its result, not its failure. On Linux
//! the sender's typed `send` into its own meeting now completes and an `Attendance` is held, and the
//! grant's descriptor crosses a learned address, so the blockage has moved entirely to the receiver's
//! parent assertion. Two things it deliberately does not
//! decide: whether that refusal should be lifted (that is the provider decision this measurement
//! feeds), and what a non-parent introducer would be *attested* as (that is the re-measurement task,
//! whose evidence the readings printed here are meant to serve).
//!
//! [`Handoff::bind`]: evering::os::Handoff::bind
//! [`Grant::admit`]: evering::os::Grant::admit
//! [`Handoff::receive`]: evering::os::Handoff::receive
//! [`Handoff::granted`]: evering::os::Handoff::granted
//! [`Handoff`]: evering::os::Handoff
//! [`Attendance`]: evering::os::Attendance

use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use evering::{
    mapping::Access,
    os::{Handoff, event, shared},
    process::Bootstrap,
};

#[cfg(unix)]
use evering::os::unix::{UnixFd, process::Socket};
#[cfg(windows)]
use evering::os::windows::process::{Listener, Socket};
use evering::process::Supervisor;
#[cfg(unix)]
use std::mem;
#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd};

/// The environment keys a grant would have written, spelled here because writing them is the point.
const RENDEZVOUS: &str = "EVERING_RENDEZVOUS";
#[cfg(windows)]
const INTRODUCER: &str = "EVERING_INTRODUCER";
/// Read by the target's own arm and reported, because a route that is refused *with* this word set
/// proves the policy opens no door for a self-announced introduction.
#[cfg(unix)]
const TRUST: &str = "EVERING_TRUST";

/// The descriptor number a grant installs a rendezvous at on unix, where the announcement is a number
/// rather than a name. Announcing it is only enough if the descriptor behind it was inherited, which is
/// what the target here does not have and the run has to place by hand.
#[cfg(unix)]
const AT: i32 = 3;

const INTRODUCE: &str = "--introduce";
const ARRIVE: &str = "--arrive";
const SIZE: usize = 4096;
const BOOTSTRAP: &[u8] = b"second-introduction";

/// How long a role waits for another role's step before calling the run broken.
const STEP: Duration = Duration::from_secs(12);
/// How long the launcher lets the introducer live, and then the target. The target's budget is the
/// longer one so that a leg which ends only in an unbounded wait is seen while its introducer still
/// holds the rendezvous open, which is the difference between "the announcement outlived the meeting"
/// and "the meeting is waiting for an offer nobody can write".
const INTRODUCER_BUDGET: Duration = Duration::from_secs(30);
const TARGET_BUDGET: Duration = Duration::from_secs(45);

const MET: i32 = 0;
/// The target reached the typed seam and was refused by it.
const REFUSED: i32 = 3;
/// The introducer could not put the offer into the meeting the target was told to reach.
const NO_MEETING: i32 = 4;
/// The placed entrance (`Handoff::granted`) took the descriptor the introducer moved across and was
/// refused by the attestation's parent assertion alone — the peer it names is the introducer, which
/// this process did not have as a parent. This is the round-three verdict on Linux, and it is the one
/// sentence the provider decision is about.
const NOT_PARENT: i32 = 6;
/// Something this example does not expect: a host error, a step that never happened, a leg that
/// answered in a shape nobody recorded.
const BROKEN: i32 = 1;

fn report(role: &str, line: impl AsRef<str>) {
    println!("SECOND\t{role}\t{}", line.as_ref());
}

fn publish(dir: &Path, name: &str, text: &str) -> io::Result<()> {
    let final_ = dir.join(name);
    fs::write(dir.join(format!("{name}.part")), text)?;
    fs::rename(dir.join(format!("{name}.part")), final_)
}

/// Reads a step the other role writes, until [`STEP`] says it is not coming.
fn await_step(dir: &Path, name: &str) -> Option<String> {
    let until = Instant::now() + STEP;
    loop {
        if let Ok(text) = fs::read_to_string(dir.join(name)) {
            return Some(text);
        }
        if Instant::now() >= until {
            return None;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// The three lines the introducer publishes, in the order it writes them: its own reachable name, the
/// rendezvous as a grant would have announced it, and its process id. The middle line is empty on unix,
/// which is one of the differences this example is about: a grant there announces a descriptor number
/// this process cannot name before it exists, while a grant here announces a pipe name. The last line is
/// checked rather than trusted — announcing a peer that is not a number is the shape of a broken
/// announcement, and the target's report has to be able to say which of the two it read.
fn announced(dir: &Path) -> io::Result<[String; 3]> {
    let text = await_step(dir, "address").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "the introducer never published its address",
        )
    })?;
    let lines = text.lines().collect::<Vec<_>>();
    let three = |reason: &str| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{text} — {reason}, and this example publishes exactly three lines"),
        )
    };
    let mut lines =
        <[&str; 3]>::try_from(lines.as_slice()).map_err(|_| three("too few or too many"))?;
    if !lines[2].bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(three("the introducer's process id is not a number"));
    }
    let [first, second, pid] = &mut lines;
    Ok([(*first).to_owned(), (*second).to_owned(), (*pid).to_owned()])
}

fn main() -> io::Result<()> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some(role @ (INTRODUCE | ARRIVE)) => {
            let dir = PathBuf::from(args.next().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a role needs the directory the launcher made",
                )
            })?);
            let code = if role == INTRODUCE {
                introduce(&dir)?
            } else {
                arrive(&dir)?
            };
            std::process::exit(code);
        }
        _ => launcher(),
    }
}

/// The launcher, which is the only process here that spawns anything, and what makes the topology the
/// one being measured: both roles are its children, so neither is the other's.
fn launcher() -> io::Result<()> {
    let dir = env::temp_dir().join(format!(
        "evering-second-introduction-{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir)?;
    let exe = env::current_exe()?;
    report(
        "launcher",
        format!(
            "spawning two unrelated processes into {}; neither one is the other's parent",
            dir.display()
        ),
    );
    let mut introducer = Command::new(&exe);
    introducer.args([INTRODUCE]).arg(&dir);
    let mut target = Command::new(&exe);
    target.args([ARRIVE]).arg(&dir);
    let mut introducer = introducer.spawn()?;
    let mut target = target.spawn()?;

    let introducer = finish("introducer", &mut introducer, INTRODUCER_BUDGET)?;
    let target = finish("target", &mut target, TARGET_BUDGET)?;
    let _ = fs::remove_dir_all(&dir);

    // The verdict is read off the two roles' own codes, and it names the leg that stopped rather than
    // the reason: the reasons are in the lines above, and they differ by host.
    let outcome = match (introducer, target) {
        (_, MET) => {
            report(
                "launcher",
                "verdict: the target completed a typed handover over an address it learned itself, \
                 with neither side spawned by the other",
            );
            MET
        }
        (MET, REFUSED) => {
            report(
                "launcher",
                "verdict: the offer went out over the learned address and the target was refused \
                 where it tried to take it through the seam — the blockage is on the receiving side, \
                 and below it the mechanism works",
            );
            REFUSED
        }
        (MET, NOT_PARENT) => {
            report(
                "launcher",
                "verdict: the grant crossed a learned address as a descriptor and the typed entrance \
                 read it; the introducer completed a typed `send` into its own meeting without ever \
                 spawning the target, and the one sentence that still refuses is the attestation's \
                 parent assertion. Whether a non-parent introducer may evidence the peer the kernel \
                 names is the provider decision this measurement feeds — a pidfd delivered beside the \
                 grant witnesses the introducer and turns readable when it exits, but watches without \
                 reaping",
            );
            NOT_PARENT
        }
        (NO_MEETING, REFUSED) => {
            report(
                "launcher",
                "verdict: the introducer could not write an offer into a meeting it did not spawn, \
                 and the target was refused where it tried to take one — the blockage is on the \
                 sending side, and the receiving side reached a rendezvous it could name but not be \
                 fed",
            );
            REFUSED
        }
        (_, REFUSED) => {
            report(
                "launcher",
                "verdict: the target reached `Handoff::receive` and was refused by it; the lines \
                 above name which signature",
            );
            REFUSED
        }
        (NO_MEETING, _) => {
            report(
                "launcher",
                "verdict: the route stops on the introducer's side, at the point where naming a peer \
                 means naming a process it spawned",
            );
            NO_MEETING
        }
        _ => {
            report(
                "launcher",
                "verdict: the route stopped somewhere the roles do not name; read the lines above in \
                 order",
            );
            BROKEN
        }
    };
    std::process::exit(outcome);
}

fn finish(role: &str, child: &mut std::process::Child, budget: Duration) -> io::Result<i32> {
    let until = Instant::now() + budget;
    loop {
        if let Some(status) = child.try_wait()? {
            let code = status.code().unwrap_or(-1);
            report(role, format!("exited {status} as {code}"));
            return Ok(code);
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let status = child.wait()?;
            report(
                role,
                format!(
                    "{budget:?} passed with no exit; killed it ({status}) — this is the leg whose \
                     wait nothing in the seam bounds, so the only way to end it is from outside"
                ),
            );
            return Ok(-1);
        }
        thread::sleep(Duration::from_millis(25));
    }
}

/// The introducer: it makes the objects a handover carries, publishes an address the target can look
/// up, and tries to move the objects into that address through the typed seam.
#[cfg(unix)]
fn introduce(dir: &Path) -> io::Result<i32> {
    let mapping = shared("second-introduction", SIZE, Access::READ | Access::WRITE)?;
    let (_rings_toward_target, wakes_target) = event()?;
    let (rings_from_target, _wakes_here) = event()?;
    let bootstrap = Bootstrap::new(BOOTSTRAP).expect("the label fits the bootstrap bound");

    // A name the target can reach before anything is offered, so that the lookup route itself is
    // measured and not assumed. `bind` makes a datagram socket with no peer, which is the whole reason
    // the attestation question is interesting here rather than on a pair.
    let named = Socket::bind(dir.join("introducer.sock"))?;
    publish(
        dir,
        "address",
        &format!(
            "{}\n\n{}",
            dir.join("introducer.sock").display(),
            std::process::id()
        ),
    )?;
    report(
        "introducer",
        "published my own name and my process id, with the rendezvous line left empty — on this host a \
         grant announces a descriptor number, and there is no number to announce before it exists; the \
         target reaches these by reading this directory, not by an announcement I wrote into its \
         environment",
    );

    // Round one asks the host, not the crate, what a rendezvous with an address is. The target binds and
    // connects an endpoint and reads its peer; this end then tries to answer it. Whatever the two
    // readings say is what the attestation of a non-parent introducer would have to be built on.
    let probe = await_step(dir, "probed").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "the target never published the endpoint that is addressable and connected",
        )
    })?;
    let label = Bootstrap::new(b"probe").expect("the probe label fits the bootstrap bound");
    // A receipt frame — bytes with nothing beside them — because this round measures who may reach
    // whom, and no real object should be spent finding out.
    let reaching = match Socket::connect(&probe) {
        Err(error) => format!(
            "the association itself was refused: {error} (kind {:?})",
            error.kind()
        ),
        Ok(reaching) => match reaching.send(&label, &[]) {
            Ok(()) => "the datagram went out".to_owned(),
            Err(error) => format!(
                "the datagram was written to and the host refused it: {error} (kind {:?})",
                error.kind()
            ),
        },
    };
    report(
        "introducer",
        format!(
            "answering the endpoint that is addressable *and* connected: {reaching}; the end this name \
             was bound at answers about its peer: {:?}",
            named.peer_credentials()
        ),
    );

    // Round two is the route. `Handoff::bind` is the only constructor and its socket field is private,
    // so a rendezvous somebody can look up cannot be handed to the typed sender at all; the offer below
    // goes out through the untyped leg, which is also why no `Attendance` can ever be read for it.
    report(
        "introducer",
        "`Handoff::bind` is the only constructor and its socket field is private, so this offer is \
         written by an untyped `Socket::send` and nobody holds an `Attendance` for it",
    );
    let meeting = await_step(dir, "reply").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "the target never published the meeting my offer goes to",
        )
    })?;
    let reaching = Socket::connect(&meeting).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("the meeting address refused even an association: {error}"),
        )
    })?;
    if let Err(error) = reaching.send(
        &bootstrap,
        &[
            mapping.as_fd(),
            wakes_target.as_fd(),
            rings_from_target.as_fd(),
        ],
    ) {
        report(
            "introducer",
            format!(
                "REFUSED at the sending leg, below the typed seam: {error} (kind {:?}) — the offer \
                 never reached the address the target bound",
                error.kind()
            ),
        );
        return Ok(NO_MEETING);
    }
    publish(dir, "offered", "written")?;
    report(
        "introducer",
        format!(
            "the offer is in the meeting: {} bytes of bootstrap plus three descriptors, written by a \
             process the target did not spawn; the kernel's answer about the end it went to: {:?}",
            bootstrap.as_ref().len(),
            reaching.peer_credentials()
        ),
    );

    await_step(dir, "taken").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "the target never finished, so the objects it was offered are still held here",
        )
    })?;

    // Round three runs the route as the seam now opens it. Prediction 4 still holds — there is no
    // constructor that turns a look-up-able rendezvous into a `Handoff` — but none is needed on the
    // sender's side: this process makes its OWN meeting with `Handoff::bind` and moves that meeting's
    // grant across as a single descriptor, over the address the target publishes for it, on the same SCM
    // route the untyped legs used. What the target then holds is the other end of a socketpair it did
    // not create and was never announced a number for.
    let grant_meeting = await_step(dir, "grantme").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "the target never published where to move the grant",
        )
    })?;
    let (meeting, grant) = Handoff::bind()?;
    let moved_over = Socket::connect(&grant_meeting)?;
    let grant_label = Bootstrap::new(b"grant-leg").expect("the grant-leg label fits");
    moved_over.send(&grant_label, &[grant.as_fd()])?;
    publish(dir, "grant-sent", "moved")?;
    report(
        "introducer",
        "moved a meeting's grant across as one descriptor over the address the target published; the \
         target now holds the other end of a socketpair this process made, with no `EVERING_RENDEZVOUS` \
         ever written for it",
    );

    // Prediction 1, in its new place: the typed sender still demands a `&Supervisor` this arm ignores.
    // `Handoff::send` will not take the offer without naming a child, and the only way to obtain a
    // `&Supervisor` is to spawn one — so a process is made here for nothing else, and Linux reads neither
    // it nor the deadline.
    let mut unrelated = Command::new("sleep");
    unrelated.arg("2");
    let named = Supervisor::spawn(&mut unrelated)?;
    report(
        "introducer",
        format!(
            "`Handoff::send` still wants a `&Supervisor` it discards on this arm, so I spawned a process \
             unrelated to the meeting (id {named_pid}) purely to satisfy the signature — the offer is \
             bound to the descriptor the grant crossed, not to the child named here",
            named_pid = named.id(),
        ),
    );
    let _attendance = meeting.send(
        &named,
        Instant::now() + STEP,
        &bootstrap,
        &mapping,
        &wakes_target,
        &rings_from_target,
    )?;
    report(
        "introducer",
        "the typed `send` completed into a meeting whose grant crossed a learned address, and an \
         `Attendance` is held for the receipt the target would write back — the sender-side blockage of \
         round two is gone on this host; what remains is the receiver's attestation",
    );

    await_step(dir, "grant-answer").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "the target never answered the placed entrance",
        )
    })?;
    drop((named, _attendance, grant, moved_over));
    drop((
        mapping,
        wakes_target,
        rings_from_target,
        _rings_toward_target,
    ));
    let _ = fs::remove_file(dir.join("introducer.sock"));
    Ok(MET)
}

/// The target: it learns the addresses, builds the two endpoint shapes a rendezvous can have, places one
/// of them where the announcement protocol says to look, and asks the typed seam for the handover.
#[cfg(unix)]
fn arrive(dir: &Path) -> io::Result<i32> {
    let [introducer, _rendezvous, _pid] = announced(dir)?;

    // Shape one: address *and* peer, which the seam's two constructors do not give in one call —
    // `Socket::bind` leaves the peer empty and `Socket::connect` leaves the name empty. So the target
    // builds it outside the crate and hands it in through `Socket::from_owned_fd`, which is the shape a
    // rendezvous would have to be able to take if a route like this is ever to be typed. Connecting is
    // also what proves the learned address is a working one rather than a string.
    let probe = dir.join("probe.sock");
    let reached = std::os::unix::net::UnixDatagram::bind(&probe)?;
    reached.connect(&introducer)?;
    let reached = Socket::from_owned_fd(reached.into());
    report(
        "target",
        format!(
            "bound a name and connected it to the introducer's name, which is how the learned address \
             is checked rather than assumed; the host's answer about that peer: {:?}",
            reached.peer_credentials()
        ),
    );
    publish(dir, "probed", &probe.display().to_string())?;

    // Shape two: the one the offer can actually arrive on. The introducer's line above reports that this
    // host will not even associate with a datagram endpoint that is already connected elsewhere, so a
    // rendezvous another process writes to is a bound socket with no peer — and that is the socket whose
    // peer the attestation then asks about.
    let meeting = dir.join("meeting.sock");
    let endpoint = Socket::bind(&meeting)?;
    report(
        "target",
        format!(
            "bound the meeting address unconnected, since a connected endpoint cannot be reached at all \
             from another process here; the host's answer about its peer: {:?}",
            endpoint.peer_credentials()
        ),
    );
    publish(dir, "reply", &meeting.display().to_string())?;

    // Prediction 2: `receive` takes no argument, so the announcement has to be in this process's own
    // environment, and writing it there is `unsafe` under edition 2024. The descriptor behind it is one
    // this process made, which is exactly the claim the adoption rule is built to refuse.
    let offered = await_step(dir, "offered").is_some();
    let slot = {
        // The only way a process has of naming a free descriptor is to take one: a fresh open lands on
        // the lowest unused number, and `dup2` below replaces that entry. Placing the rendezvous at the
        // number a grant would have used is not always possible, because the seam takes ownership of
        // whatever number the environment names and closes it as its own — and a number this process
        // already holds would then have two owners.
        let null = fs::File::open("/dev/null")?;
        let free = null.as_fd().as_raw_fd();
        mem::forget(null);
        free
    };
    let placed = unsafe { nix::libc::dup2(endpoint.as_fd().as_raw_fd(), slot) >= 0 };
    unsafe {
        env::set_var(RENDEZVOUS, slot.to_string());
        env::set_var(TRUST, "1");
    }
    report(
        "target",
        format!(
            "wrote my own announcement ({RENDEZVOUS}={slot}) and the introducer's policy word \
             ({TRUST}=1) into my own environment; the descriptor was placed by `dup2`: {placed}. The \
             number a grant writes is {AT}, which was {} here: the seam accepts any number the \
             environment names and closes it as its own, so a self-announced route has to pick a free \
             slot rather than trusting that one",
            if slot == AT {
                "free, as it is in a child"
            } else {
                "already taken by this process"
            }
        ),
    );

    let outcome = Handoff::receive();
    let code = match outcome {
        Ok((provenance, resources, _report)) => {
            report(
                "target",
                format!(
                    "MEETS: {provenance:?}, bootstrap {:?}, region {} bytes",
                    String::from_utf8_lossy(resources.bootstrap.as_ref()),
                    resources.mapping.size()
                ),
            );
            MET
        }
        Err(error) => {
            report(
                "target",
                format!(
                    "REFUSED at the typed seam: {error} (kind {:?}) — this is with {TRUST}=1 \
                     announced by me, so what the policy did not do is open a door for an \
                     introduction nobody granted",
                    error.kind()
                ),
            );
            REFUSED
        }
    };

    // Below the seam, the same three descriptors, read by a process nobody introduced. Only on the
    // refusal: a completed handover has already taken the offer, and this read would then wait for a
    // second one that no introducer is writing. The seam attests before it reads, so a refusal costs
    // this process only the descriptor it duplicated, not the offer waiting behind it.
    if code == REFUSED && offered {
        let (bootstrap, resources) = endpoint.recv(3)?.into_parts();
        let mut resources = resources.into_vec();
        let mapping = UnixFd::from_fd(resources.remove(0)).map_err(io::Error::from)?;
        let event = resources.remove(0);
        let ring = resources.remove(0);
        report(
            "target",
            format!(
                "the learned address does carry the handover, untyped: bootstrap {:?}, region {} \
                 bytes, and the two doorbell descriptors arrived as {} and {}; the three are \
                 installed in this process's table by `Socket::recv`, which checks nothing about who \
                 wrote the message",
                String::from_utf8_lossy(bootstrap.as_ref()),
                mapping.size(),
                event.as_raw_fd(),
                ring.as_raw_fd()
            ),
        );
    } else if code == REFUSED {
        report(
            "target",
            "no offer was written to the address I bound, so the untyped take was not attempted",
        );
    }
    publish(dir, "taken", "done")?;
    for path in [&probe, &meeting] {
        let _ = fs::remove_file(path);
    }

    // Round three: the placed entrance, met the way a host that speaks no evering protocol would place
    // it. Nothing here reads `EVERING_RENDEZVOUS` — the introducer published no announcement and did not
    // spawn this process; it moved the grant's descriptor to an address published below, and
    // `Handoff::granted` is handed that descriptor. The one kernel question (`is_rendezvous`) is answered
    // yes because a socketpair end is a datagram endpoint — the very question round two's pipe failed.
    let grant_me = dir.join("grantme.sock");
    let leg = Socket::bind(&grant_me)?;
    publish(dir, "grantme", &grant_me.display().to_string())?;
    report(
        "target",
        "bound an address for the grant itself and published it; round three has the introducer move a \
         rendezvous here as a descriptor rather than announce a number for me to read",
    );
    await_step(dir, "grant-sent").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "the introducer never moved its grant to the address I published",
        )
    })?;
    let (_grant_label, handed) = leg.recv(1)?.into_parts();
    let mut handed = handed.into_vec();
    let rendezvous = handed.remove(0);
    report(
        "target",
        "received the grant's descriptor over the SCM route as an owned `OwnedFd`; calling \
         `Handoff::granted` with it and reading no `EVERING_RENDEZVOUS` at all",
    );
    let code = match Handoff::granted(rendezvous) {
        Ok((provenance, _resources, _receipt)) => {
            report(
                "target",
                format!("MEETS through the placed entrance: {provenance:?}"),
            );
            MET
        }
        Err(error) if error.to_string().contains("parent") => {
            report(
                "target",
                format!(
                    "REFUSED at the placed entrance solely by the attestation's parent assertion: {error} \
                     — the descriptor passed the one kernel question (a socketpair end is a datagram \
                     endpoint, unlike round two's pipe), and the introducer's {TRUST}=1 still stands in my \
                     environment from round two and opened nothing here, because the kernel answered and no \
                     policy word turns that answer into a different pid: the peer it names is my \
                     introducer, which is not my parent",
                ),
            );
            NOT_PARENT
        }
        Err(error) => {
            report(
                "target",
                format!(
                    "REFUSED at the placed entrance below the attestation: {error} (kind {:?}) — the \
                     descriptor did not answer the one kernel question, so it names no rendezvous",
                    error.kind(),
                ),
            );
            REFUSED
        }
    };
    publish(dir, "grant-answer", "answered")?;
    let _ = fs::remove_file(&grant_me);
    Ok(code)
}

/// The introducer on the host where the meeting is a named pipe the sender listens on.
#[cfg(windows)]
fn introduce(dir: &Path) -> io::Result<i32> {
    let mapping = shared("second-introduction", SIZE, Access::READ | Access::WRITE)?;
    let (_rings_toward_target, _wakes_target) = event()?;
    let bootstrap = Bootstrap::new(BOOTSTRAP).expect("the label fits the bootstrap bound");
    let accepted = Listener::bind()?;
    let waiting = Listener::bind()?;
    publish(
        dir,
        "address",
        &format!(
            "{}\n{}\n{}",
            accepted.name(),
            waiting.name(),
            std::process::id()
        ),
    )?;
    report(
        "introducer",
        "published two pipe names and my process id; the typed seam would carry them only through \
         `Grant::admit`, which writes them into a `Command` I am about to spawn, and the process I \
         need here is one I did not spawn",
    );

    // Prediction 1, run rather than asserted: to name a peer at all on this host, a sender has to hold
    // a `Supervisor` for it, and the only constructor is a spawn. This one is a process with no
    // relation to the meeting, made only so there is something to pass.
    let retained = Supervisor::spawn(
        Command::new("cmd.exe")
            .args(["/C", "exit"])
            .env("SECOND_INTRODUCTION_DUMMY", "1"),
    )?;
    report(
        "introducer",
        format!(
            "`Supervisor::spawn` is the only way to obtain the `&Supervisor` that `accept` and \
             `Socket::send` both ask for, so I spawned a process unrelated to the meeting (id {}) \
             to have one to pass",
            retained.id()
        ),
    );

    await_step(dir, "probed").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "nobody connected to the pipe whose name I published",
        )
    })?;
    let code = match accepted.accept(&retained, Instant::now() + STEP) {
        Ok(_) => {
            report(
                "introducer",
                "accepted a client that is not the process it retained — which is the failure the \
                 check exists to prevent, so this line means the check did not run",
            );
            BROKEN
        }
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            report(
                "introducer",
                format!(
                    "REFUSED at the sender by the seam: {error} — the peer the kernel named is the \
                     target, and the only peer this process is allowed to name is one it spawned",
                ),
            );
            NO_MEETING
        }
        Err(error) => {
            report(
                "introducer",
                format!(
                    "the accept never reached the peer check: {error} (kind {:?}, host code {:?}) — \
                     the instance had no client left to name",
                    error.kind(),
                    error.raw_os_error()
                ),
            );
            NO_MEETING
        }
    };
    publish(dir, "accepted", "attempted")?;
    report(
        "introducer",
        "round three is not run on this host: `Handoff` has no placed entrance here — `Handoff::granted` \
         is compiled out, so there is no descriptor route a foreign host could be handed an offer \
         through. The only way an offer reaches a process here is `Socket::send` duplicating each handle \
         into the process behind a `&Supervisor` this process spawned, which is the same blockage the \
         accept check above names",
    );
    report(
        "introducer",
        "not run, because there is nothing to run it on: `Socket::send` duplicates each handle into \
         the process behind the `Supervisor` it was given, so an offer for a process I did not spawn \
         has no target to duplicate into. The second name above stays unaccepted while I hold it, \
         which is what lets the target's own typed attempt reach a server at all",
    );
    // Stay long enough for the target's attempt to be about a live meeting rather than a dead name.
    await_step(dir, "taken");
    drop((
        mapping,
        _rings_toward_target,
        _wakes_target,
        bootstrap,
        waiting,
        retained,
    ));
    Ok(code)
}

/// The target on the host where the announcement is a pipe name and an introducer's process id.
#[cfg(windows)]
fn arrive(dir: &Path) -> io::Result<i32> {
    let [reached, waiting, introducer] = announced(dir)?;
    let probe = Socket::connect(&reached)?;
    report(
        "target",
        format!(
            "connected to a pipe whose name I read out of a directory; the kernel says its server is \
             {}, and {} is the id the introducer published there — the two are what `receive` will \
             compare, and I am going to announce the pair myself",
            probe
                .server_pid()
                .map(|pid| pid.to_string())
                .unwrap_or_else(|error| format!("answers nothing: {error}")),
            introducer
        ),
    );
    // The client end stays open until the introducer has tried to name it: an accept over a pipe whose
    // client already left answers with the host's closed-pipe error, which says nothing about whose
    // client it *was*, and the whole point of this leg is that answer.
    publish(dir, "probed", "connected")?;
    await_step(dir, "accepted");
    drop(probe);

    // Prediction 2 again, and here it is the only route: nothing wrote these two keys for me.
    unsafe {
        env::set_var(RENDEZVOUS, &waiting);
        env::set_var(INTRODUCER, &introducer);
    }
    report(
        "target",
        format!(
            "wrote my own announcement ({RENDEZVOUS}={} and {INTRODUCER}={}) and am calling \
             `Handoff::receive`; an offer was never written to this pipe, so this call has either \
             already refused or is inside a wait the seam does not bound",
            waiting, introducer
        ),
    );
    report(
        "target",
        "round three is not run on this host: this arm is handed `Handoff::receive` only, and \
         `Handoff::granted` does not exist here, so the placed route has no entrance to measure. The \
         Windows leg stops on the introducer's side, at the accept check above",
    );
    let outcome = Handoff::receive();
    publish(dir, "taken", "done")?;
    match outcome {
        Ok((provenance, resources, _report)) => {
            report(
                "target",
                format!(
                    "MEETS: {provenance:?}, bootstrap {:?}",
                    String::from_utf8_lossy(resources.bootstrap.as_ref())
                ),
            );
            Ok(MET)
        }
        Err(error) => {
            report(
                "target",
                format!(
                    "REFUSED at the typed seam: {error} (kind {:?}, host code {:?}) — {}",
                    error.kind(),
                    error.raw_os_error(),
                    if error.kind() == io::ErrorKind::PermissionDenied {
                        "that sentence is the seam's own answer about the peer behind the name"
                    } else {
                        "the seam's own refusals all answer PermissionDenied and this is not one of \
                         them, so the announcement was read and the pipe was reached — what ended is \
                         the offer nobody could write"
                    }
                ),
            );
            Ok(REFUSED)
        }
    }
}
