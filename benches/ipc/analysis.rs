use std::collections::BTreeSet;

use super::{
    family::Family,
    model::{Condition, Meta, Status, Study, StudyError, Trial, load, schedule, validate_study},
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
    pub condition: Condition,
    pub candidate: &'static str,
    pub blocks: usize,
    pub effect: f64,
    pub low: f64,
    pub high: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Authority {
    Screening,
    Focused(Vec<Decision>),
}

pub struct Analysis {
    pub(super) specification: &'static Family,
    pub(super) meta: Meta,
    pub estimates: Vec<Estimate>,
    pub authority: Authority,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Study(StudyError),
    Family,
    Pair,
    Duplicate,
}

fn comparison(family: &Family, trial: &Trial) -> Option<(Condition, &'static str)> {
    let arm = family.arm(&trial.cell.arm)?;
    (arm.key != family.baseline.key).then_some((trial.cell.condition(), arm.key))
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

fn estimate(
    condition: Condition,
    candidate: &'static str,
    logs: &[f64],
    seed: u64,
    tail: f64,
) -> Estimate {
    let mut point = logs.to_vec();
    let effect = median(&mut point).exp();
    let mut state = seed;
    let mut samples = Vec::with_capacity(10_000);
    for _ in 0..10_000 {
        let mut sample = (0..logs.len())
            .map(|_| logs[random(&mut state) as usize % logs.len()])
            .collect::<Vec<_>>();
        samples.push(median(&mut sample).exp());
    }
    Estimate {
        condition,
        candidate,
        blocks: logs.len(),
        effect,
        low: quantile(&mut samples, tail),
        high: quantile(&mut samples, 1.0 - tail),
    }
}

fn decision(estimate: &Estimate) -> Decision {
    if estimate.low >= 0.95 && estimate.high <= 1.05 {
        Decision::Equivalent
    } else if estimate.low > 1.05 {
        Decision::Faster
    } else if estimate.high < 0.95 {
        Decision::Slower
    } else {
        Decision::Inconclusive
    }
}

fn admit(study: &Study) -> Result<(&'static Family, bool, usize), Error> {
    validate_study(study).map_err(Error::Study)?;
    let family = super::family::find(&study.meta.family)
        .filter(|family| family.revision == study.meta.family_revision)
        .ok_or(Error::Family)?;
    let focused = match study.meta.mode.as_str() {
        "screening" => false,
        "focused" => true,
        _ => return Err(Error::Family),
    };
    let (_, blocks) = family.mode(&study.meta.mode).ok_or(Error::Family)?;
    let members = (family.members)(&study.meta.mode).ok_or(Error::Family)?;
    let expected = schedule(&members, blocks, study.meta.seed);
    let mut actual = study.trials.iter().collect::<Vec<_>>();
    actual.sort_unstable_by_key(|trial| (trial.block, trial.order));
    if study.meta.blocks != blocks
        || study.meta.expected != expected.len()
        || actual.iter().zip(expected).any(|(trial, expected)| {
            trial.block != expected.block
                || trial.order != expected.order
                || trial.cell.condition() != expected.condition
                || trial.cell.arm != expected.arm.key
        })
    {
        return Err(Error::Family);
    }
    let comparisons = members
        .iter()
        .filter(|(_, arm)| arm.key != family.baseline.key)
        .count();
    (comparisons > 0)
        .then_some((family, focused, comparisons))
        .ok_or(Error::Family)
}

pub fn analyze(study: &Study) -> Result<Analysis, Error> {
    let (family, focused, comparisons) = admit(study)?;
    let candidates = study
        .trials
        .iter()
        .filter(|trial| trial.block == 0)
        .filter_map(|trial| comparison(family, trial))
        .collect::<BTreeSet<_>>();
    if candidates.len() != comparisons {
        return Err(Error::Family);
    }
    let tail = if focused {
        0.05 / (2.0 * comparisons as f64)
    } else {
        0.025
    };
    let mut estimates = Vec::with_capacity(comparisons);
    for (condition, candidate) in candidates {
        let mut logs = Vec::with_capacity(study.meta.blocks as usize);
        for block in 0..study.meta.blocks {
            let candidate = study.trials.iter().find(|trial| {
                trial.block == block && comparison(family, trial) == Some((condition, candidate))
            });
            let baseline = study.trials.iter().find(|trial| {
                trial.block == block
                    && trial.cell.condition() == condition
                    && trial.cell.arm == family.baseline.key
            });
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
        estimates.push(estimate(condition, candidate, &logs, study.meta.seed, tail));
    }
    let authority = if focused {
        Authority::Focused(estimates.iter().map(decision).collect())
    } else {
        Authority::Screening
    };
    Ok(Analysis {
        specification: family,
        meta: study.meta.clone(),
        estimates,
        authority,
    })
}

impl Analysis {
    fn markdown(&self) -> String {
        use core::fmt::Write;

        let focused = match &self.authority {
            Authority::Screening => false,
            Authority::Focused(_) => true,
        };
        let authority = if focused {
            "Focused evidence authorizes registered familywise decisions."
        } else {
            "Screening is descriptive; it authorizes no performance decision."
        };
        let meta = &self.meta;
        let family = self.specification;
        let mut output = format!(
            "## {}/v{} — {}\n\n{authority}\n\nEnvironment: `{}` / `{}`; host `{}`; rustc `{}`. \
             Evidence: `{}`. Limit: {} s. Baseline: `{}`.\n\n\
             | arm | payload | capacity | in-flight | memory | blocks | effect | interval |{}\
             \n|---|---:|---:|---:|---:|---:|---:|---:|{}",
            family.key,
            family.revision,
            meta.mode,
            meta.os,
            meta.target,
            meta.host,
            meta.rustc,
            format_args!("{}:{}#0..{}", meta.revision, meta.schedule, meta.blocks),
            family.mode(&meta.mode).unwrap().0.as_secs(),
            family.baseline.key,
            if focused { " decision |" } else { "" },
            if focused { "---|" } else { "" },
        );
        for (index, estimate) in self.estimates.iter().enumerate() {
            write!(
                output,
                "\n| {} | {} | {} | {} | {} | {} | {:.6} | [{:.6}, {:.6}] |",
                estimate.candidate,
                estimate.condition.payload,
                estimate.condition.capacity,
                estimate.condition.in_flight,
                estimate.condition.memory,
                estimate.blocks,
                estimate.effect,
                estimate.low,
                estimate.high,
            )
            .expect("String writes cannot fail");
            if let Authority::Focused(decisions) = &self.authority {
                write!(output, " {:?} |", decisions[index]).expect("String writes cannot fail");
            }
        }
        output.push_str("\n\n");
        output
    }
}

pub(super) fn analyses(studies: &[Study]) -> Result<Vec<Analysis>, Error> {
    let mut analyses = studies.iter().map(analyze).collect::<Result<Vec<_>, _>>()?;
    analyses.sort_by_key(|analysis| analysis.specification.key);
    if analyses
        .windows(2)
        .any(|pair| pair[0].specification.key == pair[1].specification.key)
    {
        return Err(Error::Duplicate);
    }
    Ok(analyses)
}

pub fn report(studies: &[Study]) -> Result<String, Error> {
    let analyses = analyses(studies)?;
    let mut output = String::from("# IPC study\n\n");
    analyses
        .iter()
        .for_each(|analysis| output.push_str(&analysis.markdown()));
    Ok(output)
}

pub(super) fn complete(path: &str) -> Result<Study, String> {
    let loaded = load(std::path::Path::new(path))?;
    loaded
        .complete
        .then_some(loaded.study)
        .ok_or_else(|| format!("{path}: incomplete evidence"))
}

pub fn command(paths: &[String]) -> Result<String, String> {
    let studies = paths
        .iter()
        .map(|path| complete(path))
        .collect::<Result<Vec<_>, _>>()?;
    report(&studies).map_err(|error| format!("{error:?}"))
}
