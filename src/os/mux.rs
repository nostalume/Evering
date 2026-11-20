//! The unix leg of the multiplexed doorbell park: one `poll` across a whole slice of latches, so a
//! caller that owns N peers has one parking point rather than one per peer. Both unix arms route through
//! here because a latch is a readable descriptor on each, while the Windows arm asks the kernel to wait on
//! an array of handles instead — a different mechanism with a 64-object ceiling this file has no analogue
//! of, so that ceiling is the arm's own answer and not a limit of the seam.
//!
//! Two answers a bare `poll` does not give are owned here. Which latch loses its readiness: exactly the
//! one reported, and through that arm's own `clear`, because a counter, a byte queue and a shared bit
//! each consume in their own way and only the arm knows what an empty latch costs. And what to do with a
//! report that names nothing: `poll` returns `EINTR` for a signal rather than retrying inside nix, and an
//! interrupted park is not a doorbell failing, so the loop re-asks; a wake that sets a flag no arm reads
//! as readiness — `POLLNVAL` on a descriptor this process does not hold — is reported by the arm's
//! `clear` as the error it is instead of parking again on a latch that can never answer.
//!
//! Nothing here can be checked from the other two arms: the pipe arm's departed notifier arrives as a
//! hang-up whose `clear` returns `BrokenPipe`, while the counter arm cannot report a notifier that left at
//! all, so how this loop treats a ready-but-unreadable latch is behaviour one host shows and two hide.
use std::{
    io,
    os::fd::{AsFd, BorrowedFd},
};

use nix::{
    errno::Errno,
    poll::{PollFd, PollFlags, PollTimeout, poll},
};

use crate::os::Event;

/// Parks this thread until one of `events` answers, takes that latch's readiness and reports its index.
///
/// No count is returned because readiness is coalesced: a hundred kicks on one latch are one answer, so a
/// report that also said how many peers had rung would promise what the primitive does not hold. When
/// more than one latch is ready the lowest index is reported first and the rest keep their readiness, so
/// a caller that parks again is answered at once until the set is drained.
pub(crate) fn park(events: &mut [Event]) -> io::Result<usize> {
    if events.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a park over no doorbell answers at once, so a caller looping on it would spin",
        ));
    }
    let index = {
        // The borrows live only as long as the report: the latch that answered is cleared once every
        // borrow of the slice is gone, which is what lets this hand back an index the caller can use to
        // reach the peer behind it.
        let latches: Vec<BorrowedFd<'_>> = events.iter().map(AsFd::as_fd).collect();
        let mut watched: Vec<PollFd<'_>> = latches
            .iter()
            .map(|latch| PollFd::new(*latch, PollFlags::POLLIN))
            .collect();
        loop {
            match poll(&mut watched, PollTimeout::NONE) {
                Ok(_) => {}
                Err(Errno::EINTR) => continue,
                Err(error) => return Err(error.into()),
            }
            if let Some(index) = watched
                .iter()
                .position(|latch| latch.revents().is_some_and(|flags| !flags.is_empty()))
            {
                break index;
            }
        }
    };
    events[index].clear().map_err(io::Error::from)?;
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::park;
    use crate::{
        notify::Notify,
        os::{Event, event},
    };

    use std::{
        io,
        sync::mpsc::{self, RecvTimeoutError},
        thread,
        time::Duration,
    };

    /// How long a park that was told to wake may take before the answer counts as wrong.
    const WAKE: Duration = Duration::from_secs(5);
    /// How long a park that was told nothing stays asleep, which is how long a false report of readiness
    /// has to be wrong by before this suite calls it one.
    const PARKED: Duration = Duration::from_millis(50);

    /// Parks on another thread, so a park that never wakes fails as a timeout instead of taking the
    /// harness with it. The thread owns the set for as long as it is parked, which is what a caller
    /// holding N peers does with them.
    fn parked_alone(latches: Vec<Event>) -> mpsc::Receiver<io::Result<usize>> {
        let (sent, received) = mpsc::channel();
        thread::spawn(move || {
            let mut latches = latches;
            let _ = sent.send(park(&mut latches));
        });
        received
    }

    #[test]
    fn a_parked_mux_wakes_for_the_latch_that_rang() {
        let (_ring_a, a) = event().unwrap();
        let (ring_b, b) = event().unwrap();
        let answered = parked_alone(vec![a, b]);

        ring_b.notify().unwrap();

        assert_eq!(
            answered.recv_timeout(WAKE).unwrap().unwrap(),
            1,
            "the index names the latch that was rung, not the first in the set"
        );
    }

    #[test]
    fn one_park_reports_one_latch_and_leaves_the_others_ready() {
        let (ring_a, a) = event().unwrap();
        let (ring_b, b) = event().unwrap();
        ring_b.notify().unwrap();
        ring_b.notify().unwrap();
        ring_a.notify().unwrap();
        let mut latches = vec![a, b];

        // Both rang before either park, so nothing below can block on a readiness that is not there.
        assert_eq!(
            park(&mut latches).unwrap(),
            0,
            "a set with two ready latches reports the lower index first"
        );
        assert_eq!(
            park(&mut latches).unwrap(),
            1,
            "the other latch keeps its readiness through the first park"
        );

        // Each park took the readiness of the latch it reported, so the third one sleeps.
        let answered = parked_alone(latches);
        assert!(
            matches!(
                answered.recv_timeout(PARKED),
                Err(RecvTimeoutError::Timeout)
            ),
            "a drained set parks again instead of reporting a readiness somebody already took"
        );
        ring_a.notify().unwrap();
        assert_eq!(
            answered.recv_timeout(WAKE).unwrap().unwrap(),
            0,
            "a set every latch of which was once drained still answers the next kick"
        );
    }

    #[test]
    fn a_park_over_no_doorbell_is_refused() {
        let error = park(&mut []).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    /// Sixty-five because that is one past the ceiling the Windows arm refuses at: on this leg one park
    /// covers a set that wide, which is the claim the multiplexed wait exists for.
    #[test]
    fn many_peers_park_on_one_call() {
        let mut rings = Vec::new();
        let mut latches = Vec::new();
        for _ in 0..65 {
            let (ring, event) = event().unwrap();
            rings.push(ring);
            latches.push(event);
        }
        rings[64].notify().unwrap();

        assert_eq!(park(&mut latches).unwrap(), 64);
    }
}
