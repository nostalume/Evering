use std::time::{Duration, Instant};

#[cfg(all(unix, feature = "local-socket"))]
use super::local;
use super::{
    drive::Deadline,
    evering_arm,
    model::{self, Cell, Condition},
    stream,
    system::Resources,
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
    pub fn resources(&self) -> Resources {
        Resources {
            topology: "1c1w".into(),
            transport: self.transport.into(),
            extent: self.resources & 1 != 0,
            allocator: self.resources & 2 != 0,
            socket_send: self.resources & 4 != 0,
            socket_recv: self.resources & 8 != 0,
        }
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

pub static BUSY: Arm = evering("evering/busy", "Evering busy", evering_arm::busy);
pub static ADAPTIVE: Arm = evering(
    "evering/adaptive",
    "Evering adaptive",
    evering_arm::adaptive,
);
pub static NOTIFIED: Arm = evering(
    "evering/notified",
    "Evering notified",
    evering_arm::notified,
);
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
    modes: &'static [(&'static str, u64, u32)],
    pub members: Matrix,
}

impl Family {
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

static CORE_MODES: [(&str, u64, u32); 4] = [
    ("smoke", 90, 1),
    ("pilot", 90, 1),
    ("screening", 180, 3),
    ("focused", 240, 15),
];
pub static CORE: Family = Family {
    key: "core-ipc",
    revision: 3,
    baseline: &STREAM,
    modes: &CORE_MODES,
    members: core,
};
/// The `core-ipc` generation that evidence written before `9e4b356` records. That commit
/// raised the revision without touching this family's members or mode table, so the
/// retired definition is the current one under its old number, and an old artifact's
/// schedule stays reproducible from a compiled definition instead of being trusted.
/// A generation is retained only while an artifact needs it: the fixture named by
/// `evidence_generations` is a `core-ipc` one, and no retained `local-ipc-unix` evidence
/// predates the revision that cut its case matrix.
static CORE_RETIRED: Family = Family {
    key: "core-ipc",
    revision: 2,
    baseline: &STREAM,
    modes: &CORE_MODES,
    members: core,
};
#[cfg(all(unix, feature = "local-socket"))]
fn local(mode: &str) -> Option<Vec<(Condition, &'static Arm)>> {
    let payloads: &[_] = if mode == "smoke" {
        &[1024]
    } else if matches!(mode, "screening" | "focused") {
        &[0, 1024, 64 * 1024]
    } else {
        return None;
    };
    Some(pairs(payloads, &LOCAL_STREAM))
}
#[cfg(all(unix, feature = "local-socket"))]
static LOCAL_MODES: [(&str, u64, u32); 4] = [
    ("smoke", 45, 1),
    ("pilot", 45, 1),
    ("screening", 60, 3),
    ("focused", 90, 15),
];
#[cfg(all(unix, feature = "local-socket"))]
pub static LOCAL: Family = Family {
    key: "local-ipc-unix",
    revision: 3,
    baseline: &LOCAL_STREAM,
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

static RETIRED: [&Family; 1] = [&CORE_RETIRED];

/// The definition an artifact was written against. `find` answers for the current
/// generation only, so a retired schedule can never be run; this second lookup exists
/// because reading evidence means checking the generation the file claims, and a
/// revision no retained artifact needs stays absent — which is what makes `admit`
/// refuse an unknown generation rather than reinterpret it.
pub fn find_at(key: &str, revision: u32) -> Option<&'static Family> {
    find(key)
        .filter(|family| family.revision == revision)
        .or_else(|| {
            RETIRED
                .iter()
                .copied()
                .find(|family| family.key == key && family.revision == revision)
        })
}
