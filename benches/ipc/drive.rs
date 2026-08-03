use std::time::{Duration, Instant};

use super::model::{digest, payload};

#[derive(Clone, Copy)]
pub struct Deadline(Instant);

impl Deadline {
    pub fn after(now: Instant, duration: Duration) -> Result<Self, String> {
        Ok(Self(now.checked_add(duration).ok_or("deadline overflow")?))
    }

    pub fn within(self, now: Instant, duration: Duration) -> Result<Self, String> {
        self.remaining(now)?;
        Self::after(now, duration).map(|limit| Self(self.0.min(limit.0)))
    }

    pub fn remaining(self, now: Instant) -> Result<Duration, String> {
        (now < self.0)
            .then(|| self.0.duration_since(now))
            .ok_or("deadline expired".into())
    }
}

#[derive(Clone, Copy)]
pub struct Expected {
    operation: u64,
    payload_len: usize,
    digest: u64,
}

impl Expected {
    pub fn matches(self, operation: u64, bytes: &[u8]) -> bool {
        operation == self.operation
            && if operation == u64::MAX {
                bytes.is_empty()
            } else {
                bytes == self.digest.to_le_bytes()
            }
    }

    pub fn matches_digest(self, operation: u64, digest: u64, payload_len: usize) -> bool {
        operation == self.operation && digest == self.digest && payload_len == self.payload_len
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CountOverflow;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct PathCounts {
    pub send_attempts: u64,
    pub send_full: u64,
    pub send_busy: u64,
    pub recv_attempts: u64,
    pub recv_empty: u64,
    pub recv_busy: u64,
    pub waits: u64,
    pub wakes: u64,
    pub stale_wakes: u64,
    pub partial_io_events: u64,
    pub partial_io_bytes: u64,
}

impl PathCounts {
    #[inline(always)]
    fn add(value: &mut u64) -> Result<(), CountOverflow> {
        *value = value.checked_add(1).ok_or(CountOverflow)?;
        Ok(())
    }

    #[inline(always)]
    pub fn send_attempt(&mut self) -> Result<(), CountOverflow> {
        Self::add(&mut self.send_attempts)
    }

    #[inline(always)]
    pub fn send_full(&mut self) -> Result<(), CountOverflow> {
        Self::add(&mut self.send_full)
    }

    #[inline(always)]
    pub fn send_busy(&mut self) -> Result<(), CountOverflow> {
        Self::add(&mut self.send_busy)
    }

    #[inline(always)]
    pub fn recv_attempt(&mut self) -> Result<(), CountOverflow> {
        Self::add(&mut self.recv_attempts)
    }

    #[inline(always)]
    pub fn recv_empty(&mut self) -> Result<(), CountOverflow> {
        Self::add(&mut self.recv_empty)
    }

    #[inline(always)]
    pub fn recv_busy(&mut self) -> Result<(), CountOverflow> {
        Self::add(&mut self.recv_busy)
    }

    #[inline(always)]
    pub fn wait(&mut self) -> Result<(), CountOverflow> {
        Self::add(&mut self.waits)
    }

    #[inline(always)]
    pub fn wake(&mut self) -> Result<(), CountOverflow> {
        Self::add(&mut self.wakes)
    }

    #[inline(always)]
    pub fn stale_wake(&mut self) -> Result<(), CountOverflow> {
        Self::add(&mut self.stale_wakes)
    }

    #[inline(always)]
    pub fn partial_io(&mut self, bytes: usize) -> Result<(), CountOverflow> {
        let events = self.partial_io_events.checked_add(1).ok_or(CountOverflow)?;
        let bytes = u64::try_from(bytes).map_err(|_| CountOverflow)?;
        let total = self
            .partial_io_bytes
            .checked_add(bytes)
            .ok_or(CountOverflow)?;
        self.partial_io_events = events;
        self.partial_io_bytes = total;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Interest {
    pub read: bool,
    pub write: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub accepted: u64,
    pub completed: u64,
    pub validated: u64,
}

#[derive(Clone, Copy)]
pub struct Work {
    pub start: u64,
    pub count: u64,
    pub window: u64,
    pub payload: usize,
    pub seed: u64,
}

pub struct Measured {
    pub counts: Counts,
    pub path: PathCounts,
    pub setup_ns: u64,
    pub elapsed_ns: u64,
}

pub struct MeasureError<E> {
    pub timed: bool,
    pub error: Error<E>,
    pub path: PathCounts,
}

#[derive(Debug)]
pub enum Step<T, E> {
    Pending,
    Committed(Result<T, E>),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Kind<E> {
    Endpoint(E),
    Deadline,
    CountOverflow,
    InvalidRange,
    InvalidResponse(u64),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Error<E> {
    pub kind: Kind<E>,
    pub counts: Counts,
}

pub trait Endpoint {
    type Error;

    fn stage(&mut self, operation: u64, payload: &[u8]) -> Result<(), Self::Error>;
    fn try_send(&mut self, path: &mut PathCounts) -> Result<Step<(), Self::Error>, Self::Error>;
    fn try_recv(
        &mut self,
        path: &mut PathCounts,
        expected: Expected,
    ) -> Result<Step<bool, Self::Error>, Self::Error>;
    fn wait(
        &mut self,
        interest: Interest,
        deadline: Deadline,
        path: &mut PathCounts,
    ) -> Result<(), Self::Error>;
    fn abort(&mut self) -> Result<(), Self::Error>;
}

fn abort<E: Endpoint>(endpoint: &mut E, kind: Kind<E::Error>, counts: Counts) -> Error<E::Error> {
    let _ = endpoint.abort();
    Error { kind, counts }
}

fn transfer_inner<const COUNT: bool, E: Endpoint>(
    endpoint: &mut E,
    work: Work,
    payload: &[u8],
    expected_digest: u64,
    deadline: Deadline,
    path: &mut PathCounts,
) -> Result<Counts, Error<E::Error>> {
    if work.window == 0
        || work
            .count
            .checked_sub(1)
            .is_some_and(|last| work.start.checked_add(last).is_none())
    {
        return Err(abort(endpoint, Kind::InvalidRange, Counts::default()));
    }
    let mut counts = Counts::default();
    let mut staged = false;
    while counts.validated < work.count {
        if deadline.remaining(Instant::now()).is_err() {
            return Err(abort(endpoint, Kind::Deadline, counts));
        }
        let can_send = staged
            || (counts.accepted < work.count
                && counts.accepted - counts.completed + u64::from(staged) < work.window);
        if can_send {
            if !staged {
                let operation = work.start + counts.accepted;
                endpoint
                    .stage(operation, payload)
                    .map_err(|error| abort(endpoint, Kind::Endpoint(error), counts))?;
                staged = true;
            }
            match endpoint
                .try_send(path)
                .map_err(|error| abort(endpoint, Kind::Endpoint(error), counts))?
            {
                Step::Pending => {}
                Step::Committed(result) => {
                    staged = false;
                    counts.accepted += 1;
                    result.map_err(|error| abort(endpoint, Kind::Endpoint(error), counts))?;
                    continue;
                }
            }
        }
        if counts.completed < counts.accepted {
            let expected = work.start + counts.completed;
            let step = endpoint
                .try_recv(
                    path,
                    Expected {
                        operation: expected,
                        payload_len: work.payload,
                        digest: expected_digest,
                    },
                )
                .map_err(|error| abort(endpoint, Kind::Endpoint(error), counts))?;
            if let Step::Committed(result) = step {
                counts.completed += 1;
                let valid =
                    result.map_err(|error| abort(endpoint, Kind::Endpoint(error), counts))?;
                if !valid {
                    return Err(abort(endpoint, Kind::InvalidResponse(expected), counts));
                }
                counts.validated += 1;
                continue;
            }
        }
        endpoint
            .wait(
                Interest {
                    read: counts.completed < counts.accepted,
                    write: staged,
                },
                deadline,
                path,
            )
            .map_err(|error| abort(endpoint, Kind::Endpoint(error), counts))?;
    }
    if COUNT {
        let send_pending = path
            .send_full
            .checked_add(path.send_busy)
            .and_then(|pending| counts.accepted.checked_add(pending));
        let recv_pending = path
            .recv_empty
            .checked_add(path.recv_busy)
            .and_then(|pending| counts.completed.checked_add(pending));
        let (Some(send_attempts), Some(recv_attempts)) = (send_pending, recv_pending) else {
            return Err(abort(endpoint, Kind::CountOverflow, counts));
        };
        path.send_attempts = send_attempts;
        path.recv_attempts = recv_attempts;
    }
    Ok(counts)
}

#[cfg(test)]
#[allow(dead_code)] // The executable uses measure; isolated tests exercise transfer directly.
pub fn transfer<E: Endpoint>(
    endpoint: &mut E,
    work: Work,
    deadline: Deadline,
    path: &mut PathCounts,
) -> Result<Counts, Error<E::Error>> {
    let payload = payload(work.seed, 0, work.payload);
    transfer_inner::<true, _>(endpoint, work, &payload, digest(&payload), deadline, path)
}

#[cfg(test)]
#[allow(dead_code)] // Used by the separate optimized overhead gate, not the benchmark binary.
pub fn transfer_control<E: Endpoint>(
    endpoint: &mut E,
    work: Work,
    deadline: Deadline,
) -> Result<Counts, Error<E::Error>> {
    let payload = payload(work.seed, 0, work.payload);
    transfer_inner::<false, _>(
        endpoint,
        work,
        &payload,
        digest(&payload),
        deadline,
        &mut PathCounts::default(),
    )
}

pub fn measure<E: Endpoint>(
    endpoint: &mut E,
    work: Work,
    warmup: u64,
    began: Instant,
    deadline: Deadline,
) -> Result<Measured, MeasureError<E::Error>> {
    let payload = payload(work.seed, 0, work.payload);
    let expected_digest = digest(&payload);
    let mut path = PathCounts::default();
    let setup = transfer_inner::<true, _>(
        endpoint,
        Work {
            start: work.count,
            count: warmup,
            ..work
        },
        &payload,
        expected_digest,
        deadline,
        &mut path,
    )
    .and_then(|_| {
        transfer_inner::<true, _>(
            endpoint,
            Work {
                start: u64::MAX,
                count: 1,
                window: 1,
                payload: 0,
                seed: work.seed,
            },
            &[],
            0,
            deadline,
            &mut path,
        )
    });
    if let Err(error) = setup {
        return Err(MeasureError {
            timed: false,
            error,
            path,
        });
    }
    let setup_ns = began.elapsed().as_nanos().max(1) as u64;
    path = PathCounts::default();
    let started = Instant::now();
    match transfer_inner::<true, _>(
        endpoint,
        work,
        &payload,
        expected_digest,
        deadline,
        &mut path,
    ) {
        Ok(counts) => Ok(Measured {
            counts,
            path,
            setup_ns,
            elapsed_ns: started.elapsed().as_nanos().max(1) as u64,
        }),
        Err(error) => Err(MeasureError {
            timed: true,
            error,
            path,
        }),
    }
}
