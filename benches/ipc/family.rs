use std::time::{Duration, Instant};

#[cfg(all(unix, feature = "local-socket"))]
use super::local;
use super::{
    drive::Deadline,
    evering,
    model::{self, Cell, Condition, Observed},
    stream,
};

type Runner = fn(&Cell, u64, u64, u64, Deadline, &str) -> Result<stream::Counts, stream::RunError>;
type Matrix = fn(&str) -> Option<Vec<(Condition, &'static Arm)>>;

pub struct Arm {
    pub key: &'static str,
    pub label: &'static str,
    transport: &'static str,
    resources: u8,
    pub run: Runner,
}

impl Arm {
    pub fn admits(&self, value: &Observed) -> bool {
        let present = u8::from(value.extent.is_some())
            | u8::from(value.allocator.is_some()) << 1
            | u8::from(value.socket_send.is_some()) << 2
            | u8::from(value.socket_recv.is_some()) << 3;
        value.transport == self.transport
            && present == self.resources
            && value.extent.is_none_or(|value| value > 0)
            && value
                .allocator
                .as_deref()
                .is_none_or(|value| !value.is_empty())
            && value.socket_send.is_none_or(|value| value > 0)
            && value.socket_recv.is_none_or(|value| value > 0)
    }
}

const fn evering(key: &'static str, label: &'static str, run: Runner) -> Arm {
    Arm {
        key,
        label,
        transport: "shared-memory",
        resources: 3,
        run,
    }
}

pub static BUSY: Arm = evering("evering/busy", "Evering busy", evering::busy);
pub static ADAPTIVE: Arm = evering("evering/adaptive", "Evering adaptive", evering::adaptive);
pub static NOTIFIED: Arm = evering("evering/notified", "Evering notified", evering::notified);
pub static STREAM: Arm = Arm {
    key: "tcp/readiness",
    label: "TCP readiness",
    transport: "ipv4-loopback",
    resources: 12,
    run: stream::run,
};
#[cfg(all(unix, feature = "local-socket"))]
pub static LOCAL_STREAM: Arm = Arm {
    key: "uds/readiness",
    label: "Unix-domain socket readiness",
    transport: "unix-domain-stream",
    resources: 12,
    run: local::run,
};

pub struct Family {
    pub key: &'static str,
    pub revision: u32,
    pub baseline: &'static Arm,
    arms: &'static [&'static Arm],
    modes: &'static [(&'static str, u64, u32)],
    pub members: Matrix,
}

impl Family {
    pub fn arm(&self, key: &str) -> Option<&'static Arm> {
        self.arms.iter().copied().find(|arm| arm.key == key)
    }

    pub fn mode(&self, mode: &str) -> Option<(Duration, u32)> {
        let &(_, seconds, blocks) = self.modes.iter().find(|entry| entry.0 == mode)?;
        Some((Duration::from_secs(seconds), blocks))
    }

    pub fn deadline(&self, mode: &str, now: Instant) -> Result<Deadline, String> {
        Deadline::after(now, self.mode(mode).ok_or("unknown family mode")?.0)
    }
}

fn pairs(payloads: &[u64], right: &'static Arm) -> Vec<(Condition, &'static Arm)> {
    payloads
        .iter()
        .flat_map(|&payload| {
            let cell = model::condition(payload, 8, 8);
            [(cell, &ADAPTIVE), (cell, right)]
        })
        .collect()
}

fn core(mode: &str) -> Option<Vec<(Condition, &'static Arm)>> {
    if mode == "smoke" {
        let empty = model::condition(0, 1, 1);
        let large = model::condition(64 * 1024, 8, 3);
        return Some(vec![
            (empty, &BUSY),
            (empty, &ADAPTIVE),
            (empty, &STREAM),
            (large, &NOTIFIED),
            (large, &STREAM),
        ]);
    }
    if !matches!(mode, "screening" | "focused") {
        return None;
    }
    let mut members = pairs(&[0, 64, 1024, 16 * 1024, 64 * 1024], &STREAM);
    if mode == "screening" {
        for payload in [0, 64, 1024, 16 * 1024, 64 * 1024] {
            let cell = model::condition(payload, 8, 8);
            members.extend([(cell, &BUSY), (cell, &NOTIFIED)]);
        }
        for (capacity, in_flight) in [
            (1, 8),
            (256, 8),
            (8, 1),
            (8, 64),
            (1, 1),
            (1, 64),
            (256, 1),
            (256, 64),
        ] {
            let cell = model::condition(1024, capacity, in_flight);
            members.extend([(cell, &NOTIFIED), (cell, &STREAM)]);
        }
    }
    Some(members)
}

static CORE_ARMS: [&Arm; 4] = [&BUSY, &ADAPTIVE, &NOTIFIED, &STREAM];
static CORE_MODES: [(&str, u64, u32); 4] = [
    ("smoke", 90, 1),
    ("pilot", 90, 1),
    ("screening", 180, 3),
    ("focused", 240, 15),
];
pub static CORE: Family = Family {
    key: "core-ipc",
    revision: 1,
    baseline: &STREAM,
    arms: &CORE_ARMS,
    modes: &CORE_MODES,
    members: core,
};
#[cfg(all(unix, feature = "local-socket"))]
fn local(mode: &str) -> Option<Vec<(Condition, &'static Arm)>> {
    let payloads: &[_] = if mode == "smoke" {
        &[1024]
    } else if matches!(mode, "screening" | "focused") {
        &[0, 64, 1024, 16 * 1024, 64 * 1024]
    } else {
        return None;
    };
    Some(pairs(payloads, &LOCAL_STREAM))
}
#[cfg(all(unix, feature = "local-socket"))]
static LOCAL_ARMS: [&Arm; 2] = [&ADAPTIVE, &LOCAL_STREAM];
#[cfg(all(unix, feature = "local-socket"))]
static LOCAL_MODES: [(&str, u64, u32); 4] = [
    ("smoke", 45, 1),
    ("pilot", 45, 1),
    ("screening", 90, 3),
    ("focused", 240, 15),
];
#[cfg(all(unix, feature = "local-socket"))]
pub static LOCAL: Family = Family {
    key: "local-ipc-unix",
    revision: 1,
    baseline: &LOCAL_STREAM,
    arms: &LOCAL_ARMS,
    modes: &LOCAL_MODES,
    members: local,
};
#[cfg(all(unix, feature = "local-socket"))]
static FAMILIES: [&Family; 2] = [&CORE, &LOCAL];
#[cfg(not(all(unix, feature = "local-socket")))]
static FAMILIES: [&Family; 1] = [&CORE];

pub fn find(key: &str) -> Option<&'static Family> {
    FAMILIES.iter().copied().find(|family| family.key == key)
}
