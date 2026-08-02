use std::time::{Duration, Instant};

use super::model::{payload, valid_response};

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
    seed: u64,
}

impl Expected {
    pub fn matches(self, operation: u64, bytes: &[u8]) -> bool {
        operation == self.operation && valid_response(self.seed, operation, self.payload_len, bytes)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Path {
    pub send_stalled: bool,
    pub recv_stalled: bool,
    pub wait_entered: bool,
    pub wait_returned: bool,
    pub stale_wake: bool,
    pub partial_io: bool,
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
    pub path: Path,
    pub setup_ns: u64,
    pub elapsed_ns: u64,
}

pub struct MeasureError<E> {
    pub timed: bool,
    pub error: Error<E>,
    pub path: Path,
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

    fn stage(&mut self, operation: u64, payload: Vec<u8>) -> Result<(), Self::Error>;
    fn try_send(&mut self, path: &mut Path) -> Result<Step<(), Self::Error>, Self::Error>;
    fn try_recv(
        &mut self,
        path: &mut Path,
        expected: Expected,
    ) -> Result<Step<bool, Self::Error>, Self::Error>;
    fn wait(
        &mut self,
        interest: Interest,
        deadline: Deadline,
        path: &mut Path,
    ) -> Result<(), Self::Error>;
    fn abort(&mut self) -> Result<(), Self::Error>;
}

fn abort<E: Endpoint>(endpoint: &mut E, kind: Kind<E::Error>, counts: Counts) -> Error<E::Error> {
    let _ = endpoint.abort();
    Error { kind, counts }
}

pub fn transfer<E: Endpoint>(
    endpoint: &mut E,
    work: Work,
    deadline: Deadline,
    path: &mut Path,
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
                    .stage(operation, payload(work.seed, operation, work.payload))
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
                        seed: work.seed,
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
    Ok(counts)
}

pub fn measure<E: Endpoint>(
    endpoint: &mut E,
    work: Work,
    warmup: u64,
    began: Instant,
    deadline: Deadline,
) -> Result<Measured, MeasureError<E::Error>> {
    let mut path = Path::default();
    let setup = transfer(
        endpoint,
        Work {
            start: work.count,
            count: warmup,
            ..work
        },
        deadline,
        &mut path,
    )
    .and_then(|_| {
        transfer(
            endpoint,
            Work {
                start: u64::MAX,
                count: 1,
                window: 1,
                payload: 0,
                seed: work.seed,
            },
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
    path = Path::default();
    let started = Instant::now();
    match transfer(endpoint, work, deadline, &mut path) {
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
