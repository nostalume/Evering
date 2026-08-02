use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mechanism {
    ReservePublish,
    ClaimRecycle,
    AllocateRelease,
    Notify,
    SignalConsume,
    ProcessExchange,
}

impl Mechanism {
    fn name(self) -> &'static str {
        match self {
            Self::ReservePublish => "reserve-publish",
            Self::ClaimRecycle => "claim-recycle",
            Self::AllocateRelease => "allocate-release",
            Self::Notify => "notify",
            Self::SignalConsume => "signal-consume",
            Self::ProcessExchange => "process-exchange",
        }
    }

    fn placement(self) -> &'static str {
        match self {
            Self::ProcessExchange => "cross-process",
            _ => "same-process",
        }
    }
}

#[derive(Clone, Copy)]
pub struct Sample {
    pub elapsed: Duration,
    pub operations: u64,
    pub reset: bool,
}

pub struct Row {
    pub mechanism: Mechanism,
    pub iterations: u64,
    pub gross_ns: u128,
    pub control_ns: u128,
    pub net_ns: i128,
    pub geometry: String,
}

impl Row {
    pub fn encode(&self) -> String {
        format!(
            "MICRO\t{}\t{}\t{}\t{}\t{}\t{}\t{}\twall-clock\n",
            self.mechanism.name(),
            self.mechanism.placement(),
            self.iterations,
            self.gross_ns,
            self.control_ns,
            self.net_ns,
            self.geometry
        )
    }
}

pub fn measure(
    mechanism: Mechanism,
    iterations: u64,
    geometry: &str,
    mut step: impl FnMut(u64) -> Result<Sample, String>,
) -> Result<Row, String> {
    if iterations == 0 || geometry.is_empty() {
        return Err("invalid micro configuration".into());
    }
    let mut gross_ns = 0;
    let mut control_ns = 0;
    for iteration in 0..iterations {
        let started = Instant::now();
        std::hint::black_box(iteration);
        control_ns += started.elapsed().as_nanos();
        let sample = step(iteration)?;
        if sample.operations != 1 || !sample.reset {
            return Err("invalid micro transition".into());
        }
        gross_ns += sample.elapsed.as_nanos();
    }
    Ok(Row {
        mechanism,
        iterations,
        gross_ns,
        control_ns,
        net_ns: gross_ns as i128 - control_ns as i128,
        geometry: geometry.into(),
    })
}
