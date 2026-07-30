use std::collections::BTreeSet;

use super::model::{
    Contrast, ContrastKey, Policy, Status, Study, StudyError, Trial, load, validate_study,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Equivalent,
    Faster,
    Slower,
    Inconclusive,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Estimate {
    pub contrast: Contrast,
    pub blocks: usize,
    pub effect: f64,
    pub low: f64,
    pub high: f64,
    pub decision: Decision,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Analysis {
    pub os: String,
    pub target: String,
    pub family: usize,
    pub artifacts: Vec<String>,
    pub estimates: Vec<Estimate>,
    pub crossovers: Vec<(Policy, u64, Decision)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Study(StudyError),
    Environment,
    Family,
    Pair,
}

fn contrast(trial: &Trial) -> Option<Contrast> {
    let candidate = match trial.cell.candidate.as_str() {
        "busy" => Policy::Busy,
        "adaptive" => Policy::Adaptive,
        "notified" => Policy::Notified,
        _ => return None,
    };
    Some(Contrast {
        key: ContrastKey {
            payload: trial.cell.payload,
            capacity: trial.cell.capacity,
            in_flight: trial.cell.in_flight,
            memory: trial.cell.memory,
        },
        candidate,
    })
}

fn random(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut value = *state;
    value = (value ^ value >> 30).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ value >> 27).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ value >> 31
}

fn quantile(values: &mut [f64], at: f64) -> f64 {
    values.sort_unstable_by(f64::total_cmp);
    values[((values.len() - 1) as f64 * at).floor() as usize]
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_unstable_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

fn estimate(key: Contrast, logs: &[f64], seed: u64, family: usize) -> Estimate {
    let mut point = logs.to_vec();
    let effect = median(&mut point).exp();
    let mut state = seed
        ^ key.key.payload
        ^ key.key.capacity.rotate_left(11)
        ^ key.key.in_flight.rotate_left(23)
        ^ (key.candidate as u64).rotate_left(37);
    let mut samples = Vec::with_capacity(10_000);
    for _ in 0..10_000 {
        let mut sample = (0..logs.len())
            .map(|_| logs[random(&mut state) as usize % logs.len()])
            .collect::<Vec<_>>();
        samples.push(median(&mut sample).exp());
    }
    let tail = 0.05 / (2.0 * family as f64);
    let low = quantile(&mut samples, tail);
    let high = quantile(&mut samples, 1.0 - tail);
    let decision = if low >= 0.95 && high <= 1.05 {
        Decision::Equivalent
    } else if low > 1.05 {
        Decision::Faster
    } else if high < 0.95 {
        Decision::Slower
    } else {
        Decision::Inconclusive
    };
    Estimate {
        contrast: key,
        blocks: logs.len(),
        effect,
        low,
        high,
        decision,
    }
}

pub fn analyze(studies: &[Study]) -> Result<Analysis, Error> {
    let first = studies.first().ok_or(Error::Family)?;
    let mut family = BTreeSet::new();
    for study in studies {
        validate_study(study).map_err(Error::Study)?;
        if study.meta.os != first.meta.os
            || study.meta.target != first.meta.target
            || study.meta.arch != first.meta.arch
            || study.meta.host != first.meta.host
            || study.meta.rustc != first.meta.rustc
            || study.meta.mode != first.meta.mode
            || study.meta.seed != first.meta.seed
            || study.meta.warmup != first.meta.warmup
            || study.meta.spin != first.meta.spin
        {
            return Err(Error::Environment);
        }
        let current = study
            .trials
            .iter()
            .filter(|trial| trial.block == 0 && trial.cell.implementation == "evering")
            .filter_map(contrast)
            .collect::<BTreeSet<_>>();
        if family.is_empty() {
            family = current;
        } else if current != family {
            return Err(Error::Family);
        }
    }
    if family.is_empty() {
        return Err(Error::Family);
    }
    let mut estimates = Vec::with_capacity(family.len());
    for key in family.iter().copied() {
        let mut logs = Vec::new();
        for study in studies {
            for block in 0..study.meta.blocks {
                let mut arms = study
                    .trials
                    .iter()
                    .filter(|trial| trial.block == block && contrast(trial) == Some(key));
                let candidate = arms
                    .clone()
                    .find(|trial| trial.cell.implementation == "evering");
                let baseline = arms.find(|trial| trial.cell.implementation == "os-stream");
                let (Some(candidate), Some(baseline)) = (candidate, baseline) else {
                    return Err(Error::Pair);
                };
                if candidate.status != Status::Ok || baseline.status != Status::Ok {
                    return Err(Error::Pair);
                }
                let rate = |trial: &Trial| {
                    trial.validated as f64 * 1e9 / trial.elapsed_ns.expect("validated study") as f64
                };
                logs.push((rate(candidate) / rate(baseline)).ln());
            }
        }
        estimates.push(estimate(key, &logs, first.meta.seed, family.len()));
    }
    let mut crossovers = Vec::new();
    for policy in [Policy::Busy, Policy::Adaptive, Policy::Notified] {
        let ordered = estimates
            .iter()
            .filter(|estimate| estimate.contrast.candidate == policy)
            .collect::<Vec<_>>();
        if let Some(pair) = ordered.windows(2).find(|pair| {
            pair[0].decision == pair[1].decision
                && matches!(pair[0].decision, Decision::Faster | Decision::Slower)
                && pair[0].contrast.key.payload < pair[1].contrast.key.payload
                && pair[0].contrast.key.capacity == pair[1].contrast.key.capacity
                && pair[0].contrast.key.in_flight == pair[1].contrast.key.in_flight
        }) {
            crossovers.push((policy, pair[0].contrast.key.payload, pair[0].decision));
        }
    }
    Ok(Analysis {
        os: first.meta.os.clone(),
        target: first.meta.target.clone(),
        family: family.len(),
        artifacts: studies
            .iter()
            .map(|study| {
                format!(
                    "{}:{}#0..{}",
                    study.meta.revision, study.meta.schedule, study.meta.blocks
                )
            })
            .collect(),
        estimates,
        crossovers,
    })
}

impl Analysis {
    pub fn markdown(&self) -> String {
        use core::fmt::Write;

        let mut output = format!(
            "# IPC analysis\n\nEnvironment: `{}` on `{}`. Family: {}. Evidence: {}.\n\n\
             | policy | payload | capacity | in-flight | memory | blocks | effect | interval | decision |\n\
             |---|---:|---:|---:|---:|---:|---:|---:|---|\n",
            self.os,
            self.target,
            self.family,
            self.artifacts.join(", ")
        );
        for estimate in &self.estimates {
            writeln!(
                output,
                "| {:?} | {} | {} | {} | {} | {} | {:.6} | [{:.6}, {:.6}] | {:?} |",
                estimate.contrast.candidate,
                estimate.contrast.key.payload,
                estimate.contrast.key.capacity,
                estimate.contrast.key.in_flight,
                estimate.contrast.key.memory,
                estimate.blocks,
                estimate.effect,
                estimate.low,
                estimate.high,
                estimate.decision
            )
            .expect("String writes cannot fail");
        }
        for (policy, payload, decision) in &self.crossovers {
            writeln!(
                output,
                "\nCrossover: {policy:?} at {payload} bytes ({decision:?})."
            )
            .expect("String writes cannot fail");
        }
        output
    }
}

pub fn command(paths: &[String]) -> Result<String, String> {
    let studies = paths
        .iter()
        .map(|path| {
            let loaded = load(std::path::Path::new(path))?;
            loaded
                .complete
                .then_some(loaded.study)
                .ok_or_else(|| format!("{path}: incomplete evidence"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(analyze(&studies)
        .map_err(|error| format!("{error:?}"))?
        .markdown())
}
