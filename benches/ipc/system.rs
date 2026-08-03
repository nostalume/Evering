use std::collections::HashSet;

use super::{
    drive::PathCounts,
    model::{Cell, Observed},
    study::{self, Header, Identity, Observation, Schema},
};

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Resources {
    pub topology: String,
    pub transport: String,
    pub extent: bool,
    pub allocator: bool,
    pub socket_send: bool,
    pub socket_recv: bool,
}

impl Resources {
    pub fn admits(&self, observed: &Observed) -> bool {
        observed.topology == self.topology
            && observed.transport == self.transport
            && observed.extent.is_some() == self.extent
            && observed.allocator.is_some() == self.allocator
            && observed.socket_send.is_some() == self.socket_send
            && observed.socket_recv.is_some() == self.socket_recv
            && observed.extent.is_none_or(|value| value > 0)
            && observed
                .allocator
                .as_deref()
                .is_none_or(|value| !value.is_empty())
            && observed.socket_send.is_none_or(|value| value > 0)
            && observed.socket_recv.is_none_or(|value| value > 0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Case {
    pub workload: Cell,
    pub resources: Resources,
}

impl Identity for Case {
    fn identity(&self, hash: &mut blake3::Hasher) {
        self.workload.arm.identity(hash);
        self.workload.payload.identity(hash);
        self.workload.capacity.identity(hash);
        self.workload.in_flight.identity(hash);
        self.workload.memory.identity(hash);
        self.resources.topology.identity(hash);
        self.resources.transport.identity(hash);
        hash.update(&[
            self.resources.extent as u8,
            self.resources.allocator as u8,
            self.resources.socket_send as u8,
            self.resources.socket_recv as u8,
        ]);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum Role {
    Primary,
    Guardrail,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Contrast {
    pub candidate: String,
    pub baseline: String,
    pub delta: f64,
    pub role: Role,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Specification {
    pub family: String,
    pub family_revision: u32,
    pub mode: String,
    pub blocks: u32,
    pub spin: u32,
    pub timeout_ms: u64,
    pub alpha: f64,
    pub calibration: Option<String>,
    pub contrasts: Vec<Contrast>,
}

impl Identity for Specification {
    fn identity(&self, hash: &mut blake3::Hasher) {
        self.family.identity(hash);
        self.family_revision.identity(hash);
        self.mode.identity(hash);
        self.blocks.identity(hash);
        self.spin.identity(hash);
        self.timeout_ms.identity(hash);
        hash.update(&self.alpha.to_bits().to_le_bytes());
        match &self.calibration {
            Some(value) => {
                hash.update(&[1]);
                value.identity(hash);
            }
            None => {
                hash.update(&[0]);
            }
        };
        for contrast in &self.contrasts {
            contrast.candidate.identity(hash);
            contrast.baseline.identity(hash);
            hash.update(&contrast.delta.to_bits().to_le_bytes());
            hash.update(&[contrast.role as u8]);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Measure {
    pub requested: u64,
    pub accepted: u64,
    pub completed: u64,
    pub validated: u64,
    pub elapsed_ns: u64,
    pub phase_ns: [u64; 3],
    pub observed: Observed,
    pub path: PathCounts,
}

pub struct System;
pub type Evidence = study::Study<System>;

impl Schema for System {
    const NAME: &'static str = "system";
    const REVISION: u32 = 1;
    type Specification = Specification;
    type Case = Case;
    type Measure = Measure;

    fn validate_header(header: &Header<Specification, Case>) -> Result<(), String> {
        let spec = &header.specification;
        if spec.family.is_empty()
            || spec.family_revision == 0
            || spec.mode.is_empty()
            || spec.blocks == 0
            || spec.timeout_ms == 0
            || !spec.alpha.is_finite()
            || !(0.0..1.0).contains(&spec.alpha)
            || header.run.budget_ms == 0
            || header.cases.is_empty()
            || header.run.schedule.is_empty()
        {
            return Err("incomplete system specification".into());
        }
        let ids = header
            .cases
            .iter()
            .map(study::case_id::<System>)
            .collect::<HashSet<_>>();
        let mut contrasts = HashSet::with_capacity(spec.contrasts.len());
        if ids.len() != header.cases.len()
            || spec.contrasts.iter().any(|contrast| {
                let case = |id: &str| {
                    header
                        .cases
                        .iter()
                        .find(|case| study::case_id::<System>(case) == id)
                };
                !contrast.delta.is_finite()
                    || !(0.0..1.0).contains(&contrast.delta)
                    || contrast.candidate == contrast.baseline
                    || !ids.contains(&contrast.candidate)
                    || !ids.contains(&contrast.baseline)
                    || !contrasts.insert((&contrast.candidate, &contrast.baseline))
                    || case(&contrast.candidate)
                        .zip(case(&contrast.baseline))
                        .is_none_or(|(candidate, baseline)| {
                            candidate.workload.condition() != baseline.workload.condition()
                        })
            })
        {
            return Err("invalid system case or contrast".into());
        }
        Ok(())
    }

    fn validate(
        header: &Header<Specification, Case>,
        row: &Observation<Measure>,
    ) -> Result<(), String> {
        let case = header
            .cases
            .get(row.case as usize)
            .ok_or("foreign system case")?;
        let measure = &row.measure;
        let pending_send = measure.path.send_full.checked_add(measure.path.send_busy);
        let pending_recv = measure.path.recv_empty.checked_add(measure.path.recv_busy);
        if measure.requested == 0
            || measure.accepted != measure.requested
            || measure.completed != measure.accepted
            || measure.validated != measure.completed
            || measure.elapsed_ns == 0
            || measure.observed.payload != case.workload.payload
            || measure.observed.capacity != case.workload.capacity
            || measure.observed.in_flight != case.workload.in_flight
            || measure.observed.window
                != super::model::window(
                    measure.requested,
                    case.workload.capacity,
                    case.workload.in_flight,
                )
            || !case.resources.admits(&measure.observed)
            || measure.path.wakes > measure.path.waits
            || measure.path.stale_wakes > measure.path.wakes
            || pending_send.is_none_or(|count| count > measure.path.send_attempts)
            || pending_recv.is_none_or(|count| count > measure.path.recv_attempts)
        {
            return Err("invalid system measure".into());
        }
        Ok(())
    }
}

pub fn load(path: &std::path::Path) -> Result<Evidence, String> {
    study::load::<System>(path).map_err(|error| format!("{error:?}"))
}
