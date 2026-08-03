use super::{
    family::Family,
    fixture,
    mechanism::{self, FixtureSchema},
    model::{Condition, schedule},
    pilot, study,
    system::{self, Evidence},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Interval {
    pub point: f64,
    pub low: f64,
    pub high: f64,
}

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
    pub candidate: String,
    pub case: String,
    pub role: system::Role,
    pub blocks: usize,
    pub effect: f64,
    pub low: f64,
    pub high: f64,
    pub delta: f64,
    pub candidate_rate: f64,
    pub baseline_rate: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Authority {
    Descriptive,
    SystemEffect,
    Attributed,
    Inconclusive,
}

pub struct Analysis {
    pub(super) family: &'static Family,
    pub(super) evidence: String,
    pub(super) context: study::Context,
    pub(super) specification: system::Specification,
    pub(super) budget_ms: u64,
    pub estimates: Vec<Estimate>,
    pub authority: Authority,
    pub decisions: Vec<Decision>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Family,
    Pair,
    Duplicate,
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

fn interval(
    values: &[f64],
    seed: u64,
    tail: f64,
    transform: impl Copy + Fn(f64) -> f64,
) -> Interval {
    let mut point = values.to_vec();
    let point = transform(median(&mut point));
    let mut state = seed;
    let mut samples = Vec::with_capacity(10_000);
    for _ in 0..10_000 {
        let mut sample = (0..values.len())
            .map(|_| values[study::random(&mut state) as usize % values.len()])
            .collect::<Vec<_>>();
        samples.push(transform(median(&mut sample)));
    }
    Interval {
        point,
        low: quantile(&mut samples, tail),
        high: quantile(&mut samples, 1.0 - tail),
    }
}

fn estimate(
    condition: Condition,
    candidate: String,
    contrast: &system::Contrast,
    logs: &[f64],
    rates: (&[f64], &[f64]),
    seed: u64,
    tail: f64,
) -> Estimate {
    let interval = interval(logs, seed, tail, f64::exp);
    let mut candidate_rate = rates.0.to_vec();
    let mut baseline_rate = rates.1.to_vec();
    Estimate {
        condition,
        candidate,
        case: contrast.candidate.clone(),
        role: contrast.role,
        blocks: logs.len(),
        effect: interval.point,
        low: interval.low,
        high: interval.high,
        delta: contrast.delta,
        candidate_rate: median(&mut candidate_rate),
        baseline_rate: median(&mut baseline_rate),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MechanismEstimate {
    pub target: String,
    pub target_admitted: Option<bool>,
    pub case: String,
    pub paired_ns: Vec<f64>,
    pub gross_ns: Vec<f64>,
    pub control_ns: Vec<f64>,
    pub iqr: [f64; 2],
    pub interval: Interval,
    pub decision: Decision,
}

pub struct MechanismAnalysis {
    pub schema: String,
    pub evidence: String,
    pub context: study::Context,
    pub delta_ns: f64,
    pub system_delta: f64,
    pub estimates: Vec<MechanismEstimate>,
}

pub struct Attribution {
    pub authority: Authority,
    pub system: String,
    pub mechanism: Option<String>,
    pub intervention: Option<String>,
}

pub struct Source {
    pub evidence: String,
    pub digest: String,
    pub bytes: u64,
}

pub struct MechanismSection {
    pub analysis: MechanismAnalysis,
    pub attribution: Option<Attribution>,
}

pub struct CalibrationSection {
    pub evidence: String,
    pub context: study::Context,
    pub cases: usize,
    pub budget_ms: u64,
}

pub struct Report {
    pub calibrations: Vec<CalibrationSection>,
    pub systems: Vec<Analysis>,
    pub mechanisms: Vec<MechanismSection>,
    pub sources: Vec<Source>,
}

pub fn analyze_mechanism<F: FixtureSchema>(
    evidence: &study::Study<mechanism::Mechanism<F>>,
    systems: &[Evidence],
) -> Result<MechanismAnalysis, Error> {
    let header = &evidence.header;
    let tail = header.specification.alpha / (2.0 * header.cases.len() as f64);
    let mut estimates = Vec::with_capacity(header.cases.len());
    for case in 0..header.cases.len() {
        let rows = evidence
            .observations
            .iter()
            .filter(|row| row.case as usize == case)
            .collect::<Vec<_>>();
        if rows.len() != header.specification.policy.pairs as usize {
            return Err(Error::Pair);
        }
        let gross_ns = rows
            .iter()
            .map(|row| row.measure.gross_ns as f64 / row.measure.operations as f64)
            .collect::<Vec<_>>();
        let control_ns = rows
            .iter()
            .map(|row| row.measure.control_ns as f64 / row.measure.operations as f64)
            .collect::<Vec<_>>();
        let paired_ns = gross_ns
            .iter()
            .zip(&control_ns)
            .map(|(gross, control)| gross - control)
            .collect::<Vec<_>>();
        let interval = interval(&paired_ns, header.run.seed, tail, core::convert::identity);
        let mut ordered = paired_ns.clone();
        let iqr = [quantile(&mut ordered, 0.25), quantile(&mut ordered, 0.75)];
        let delta = header.specification.delta_ns;
        let target = &header.specification.targets[case];
        let mut matches = systems.iter().flat_map(|system| {
            system
                .header
                .cases
                .iter()
                .filter(|candidate| study::case_id::<system::System>(candidate) == *target)
        });
        let target_admitted = matches.next().map(|first| {
            F::matches(&header.specification.parameters, &header.cases[case], first)
                && matches.all(|candidate| {
                    F::matches(
                        &header.specification.parameters,
                        &header.cases[case],
                        candidate,
                    )
                })
        });
        let decision = if interval.low >= -delta && interval.high <= delta {
            Decision::Equivalent
        } else if interval.high < -delta {
            Decision::Faster
        } else if interval.low > delta {
            Decision::Slower
        } else {
            Decision::Inconclusive
        };
        estimates.push(MechanismEstimate {
            target: target.clone(),
            target_admitted,
            case: serde_json::to_string(&header.cases[case]).map_err(|_| Error::Pair)?,
            paired_ns,
            gross_ns,
            control_ns,
            iqr,
            interval,
            decision,
        });
    }
    Ok(MechanismAnalysis {
        schema: F::KEY.into(),
        evidence: study::study_id::<mechanism::Mechanism<F>>(header),
        context: header.context.clone(),
        delta_ns: header.specification.delta_ns,
        system_delta: header.specification.system_delta,
        estimates,
    })
}

fn path_used(evidence: &Evidence, target: &str, schema: &str) -> bool {
    let Some(case) = evidence
        .header
        .cases
        .iter()
        .position(|case| study::case_id::<system::System>(case) == target)
    else {
        return false;
    };
    evidence
        .observations
        .iter()
        .filter(|row| row.case as usize == case)
        .any(|row| match schema {
            "mechanism.queue.reserve-publish" => row.measure.path.send_attempts > 0,
            "mechanism.queue.claim-recycle" => row.measure.path.recv_attempts > 0,
            "mechanism.pool.allocate-release" => {
                row.measure.path.send_attempts > 0 && row.measure.observed.allocator.is_some()
            }
            "mechanism.talc.allocate-release" => {
                row.measure.path.send_attempts > 0
                    && row.measure.observed.allocator.as_deref() == Some("talc")
            }
            "mechanism.notify" => row.measure.path.waits > 0,
            "mechanism.signal-wait" => row.measure.path.waits > 0 && row.measure.path.wakes > 0,
            _ => false,
        })
}

fn guardrails(analysis: &Analysis) -> bool {
    analysis
        .estimates
        .iter()
        .zip(&analysis.decisions)
        .filter(|(estimate, _)| estimate.role == system::Role::Guardrail)
        .all(|(_, decision)| matches!(decision, Decision::Equivalent | Decision::Faster))
}

fn same_cases(left: &Evidence, right: &Evidence) -> bool {
    let ids = |evidence: &Evidence| {
        evidence
            .header
            .cases
            .iter()
            .map(study::case_id::<system::System>)
            .collect::<std::collections::BTreeSet<_>>()
    };
    ids(left) == ids(right)
}

pub fn attribute(
    system: &Evidence,
    mechanism: Option<&MechanismAnalysis>,
    intervention: Option<&Evidence>,
) -> Result<Attribution, Error> {
    let analysis = analyze(system)?;
    let mut result = Attribution {
        authority: analysis.authority,
        system: analysis.evidence.clone(),
        mechanism: mechanism.map(|value| value.evidence.clone()),
        intervention: None,
    };
    if analysis.authority != Authority::SystemEffect {
        return Ok(result);
    }
    let (Some(mechanism), Some(intervention)) = (mechanism, intervention) else {
        return Ok(result);
    };
    let after = analyze(intervention)?;
    result.intervention = Some(after.evidence.clone());
    let contexts_match =
        study::context_id(&system.header.context) == study::context_id(&mechanism.context);
    let intervention_is_distinct = result.system != after.evidence;
    let same_protocol = system.header.specification.family
        == intervention.header.specification.family
        && system.header.specification.family_revision
            == intervention.header.specification.family_revision
        && system.header.specification.mode == intervention.header.specification.mode
        && system.header.context.compiler == intervention.header.context.compiler
        && system.header.context.host == intervention.header.context.host
        && same_cases(system, intervention);
    let linked = mechanism.estimates.iter().all(|mechanism_estimate| {
        let before = analysis
            .estimates
            .iter()
            .zip(&analysis.decisions)
            .find(|(estimate, _)| estimate.case == mechanism_estimate.target);
        let after = after
            .estimates
            .iter()
            .find(|estimate| estimate.case == mechanism_estimate.target);
        let Some(((before, decision), after)) = before.zip(after) else {
            return false;
        };
        let expected = if mechanism.system_delta > 0.0 {
            after.low > before.high * (1.0 + mechanism.system_delta)
        } else {
            after.high < before.low * (1.0 + mechanism.system_delta)
        };
        before.role == system::Role::Primary
            && *decision == mechanism_estimate.decision
            && mechanism_estimate.target_admitted == Some(true)
            && matches!(decision, Decision::Faster | Decision::Slower)
            && path_used(system, &mechanism_estimate.target, &mechanism.schema)
            && expected
    });
    result.authority = if contexts_match
        && intervention_is_distinct
        && same_protocol
        && guardrails(&analysis)
        && guardrails(&after)
        && linked
    {
        Authority::Attributed
    } else {
        Authority::Inconclusive
    };
    Ok(result)
}

fn decision(estimate: &Estimate) -> Decision {
    let low = 1.0 - estimate.delta;
    let high = 1.0 + estimate.delta;
    if estimate.low >= low && estimate.high <= high {
        Decision::Equivalent
    } else if estimate.low > high {
        Decision::Faster
    } else if estimate.high < low {
        Decision::Slower
    } else {
        Decision::Inconclusive
    }
}

fn admit(evidence: &Evidence) -> Result<(&'static Family, bool), Error> {
    let header = &evidence.header;
    let spec = &header.specification;
    let family = super::family::find(&spec.family)
        .filter(|family| family.revision == spec.family_revision)
        .ok_or(Error::Family)?;
    let focused = match spec.mode.as_str() {
        "smoke" | "screening" => false,
        "focused" => true,
        _ => return Err(Error::Family),
    };
    let (_, blocks) = family.mode(&spec.mode).ok_or(Error::Family)?;
    let members = (family.members)(&spec.mode).ok_or(Error::Family)?;
    let expected = schedule(&members, blocks, header.run.seed);
    if spec.blocks != blocks
        || header.run.schedule.len() != expected.len()
        || header
            .run
            .schedule
            .iter()
            .zip(expected)
            .any(|(unit, expected)| {
                let Some(case) = header.cases.get(unit.case as usize) else {
                    return true;
                };
                unit.unit.block != expected.block
                    || unit.unit.order != expected.order
                    || case.workload != expected.cell()
                    || case.resources != expected.arm.resources()
            })
        || spec.contrasts.is_empty()
    {
        return Err(Error::Family);
    }
    Ok((family, focused))
}

pub fn analyze(evidence: &Evidence) -> Result<Analysis, Error> {
    let (family, focused) = admit(evidence)?;
    let header = &evidence.header;
    let comparisons = header.specification.contrasts.len();
    let tail = if focused {
        header.specification.alpha / (2.0 * comparisons as f64)
    } else {
        header.specification.alpha / 2.0
    };
    let mut estimates = Vec::with_capacity(comparisons);
    for contrast in &header.specification.contrasts {
        let index = |id: &str| {
            header
                .cases
                .iter()
                .position(|case| study::case_id::<system::System>(case) == id)
        };
        let candidate = index(&contrast.candidate).ok_or(Error::Pair)? as u32;
        let baseline = index(&contrast.baseline).ok_or(Error::Pair)? as u32;
        let condition = header.cases[candidate as usize].workload.condition();
        if header.cases[baseline as usize].workload.condition() != condition {
            return Err(Error::Pair);
        }
        let mut logs = Vec::with_capacity(header.specification.blocks as usize);
        let mut candidate_rates = Vec::with_capacity(header.specification.blocks as usize);
        let mut baseline_rates = Vec::with_capacity(header.specification.blocks as usize);
        for block in 0..header.specification.blocks {
            let find = |case| {
                evidence
                    .observations
                    .iter()
                    .find(|row| row.unit.block == block && row.case == case)
            };
            let (candidate, baseline) = (find(candidate), find(baseline));
            let (Some(candidate), Some(baseline)) = (candidate, baseline) else {
                return Err(Error::Pair);
            };
            let rate = |row: &study::Observation<system::Measure>| {
                row.measure.validated as f64 * 1e9 / row.measure.elapsed_ns as f64
            };
            let (candidate_rate, baseline_rate) = (rate(candidate), rate(baseline));
            logs.push((candidate_rate / baseline_rate).ln());
            candidate_rates.push(candidate_rate);
            baseline_rates.push(baseline_rate);
        }
        estimates.push(estimate(
            condition,
            header.cases[candidate as usize].workload.arm.clone(),
            contrast,
            &logs,
            (&candidate_rates, &baseline_rates),
            header.run.seed,
            tail,
        ));
    }
    let decisions = estimates.iter().map(decision).collect::<Vec<_>>();
    let authority = if !focused {
        Authority::Descriptive
    } else if estimates
        .iter()
        .zip(&decisions)
        .filter(|(estimate, _)| estimate.role == system::Role::Primary)
        .any(|(_, decision)| matches!(decision, Decision::Faster | Decision::Slower))
    {
        Authority::SystemEffect
    } else {
        Authority::Inconclusive
    };
    Ok(Analysis {
        family,
        evidence: study::study_id::<system::System>(header),
        context: header.context.clone(),
        specification: header.specification.clone(),
        budget_ms: header.run.budget_ms,
        estimates,
        authority,
        decisions,
    })
}

impl Analysis {
    pub(super) fn markdown(&self) -> String {
        use core::fmt::Write;
        let focused = self.specification.mode == "focused";
        let authority = match self.authority {
            Authority::Descriptive => "Descriptive evidence authorizes no performance decision.",
            Authority::SystemEffect => "A registered System effect is present but unattributed.",
            Authority::Attributed => "The registered causal chain is attributed.",
            Authority::Inconclusive => "The registered evidence is inconclusive.",
        };
        let mut output = format!(
            "## {}/v{} — {}\n\n{authority}\n\nEnvironment: `{}` / `{}`; host `{}`; rustc `{}`. Evidence: `{}`. Limit: {} ms. Baseline: `{}`.\n\n| arm | payload | capacity | in-flight | memory | blocks | candidate op/s | baseline op/s | candidate MiB/s | baseline MiB/s | effect | interval |{}\n|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|{}",
            self.family.key,
            self.family.revision,
            self.specification.mode,
            self.context.host.os,
            self.context.compiler.target,
            self.context.host.description,
            self.context.compiler.rustc,
            self.evidence,
            self.budget_ms,
            self.family.baseline.key,
            if focused { " decision |" } else { "" },
            if focused { "---|" } else { "" },
        );
        for (index, estimate) in self.estimates.iter().enumerate() {
            let mib = estimate.condition.payload as f64 / (1024.0 * 1024.0);
            write!(
                output,
                "\n| {} | {} | {} | {} | {} | {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.6} | [{:.6}, {:.6}] |",
                estimate.candidate,
                size(estimate.condition.payload),
                estimate.condition.capacity,
                estimate.condition.in_flight,
                size(estimate.condition.memory),
                estimate.blocks,
                estimate.candidate_rate,
                estimate.baseline_rate,
                estimate.candidate_rate * mib,
                estimate.baseline_rate * mib,
                estimate.effect,
                estimate.low,
                estimate.high,
            )
            .expect("String writes cannot fail");
            if focused {
                write!(output, " {:?} |", self.decisions[index])
                    .expect("String writes cannot fail");
            }
        }
        output.push_str("\n\n");
        output
    }
}

impl MechanismAnalysis {
    pub(super) fn markdown(&self, authority: Authority) -> String {
        use core::fmt::Write;
        let mut output = format!(
            "## {}\n\nAuthority: `{:?}`. Evidence: `{}`. Practical band: ±{:.3} ns/op. Expected System change: {:+.3}.\n\n| fixture case | target System case | target contract | pairs | median difference | interval | interquartile range | decision |\n|---|---|---|---:|---:|---:|---:|---|",
            self.schema, authority, self.evidence, self.delta_ns, self.system_delta,
        );
        for estimate in &self.estimates {
            writeln!(
                output,
                "\n| `{}` | `{}` | {} | {} | {:.3} ns/op | [{:.3}, {:.3}] ns/op | [{:.3}, {:.3}] ns/op | {:?} |",
                estimate.case,
                estimate.target,
                match estimate.target_admitted {
                    Some(true) => "admitted",
                    Some(false) => "rejected",
                    None => "unavailable",
                },
                estimate.paired_ns.len(),
                estimate.interval.point,
                estimate.interval.low,
                estimate.interval.high,
                estimate.iqr[0],
                estimate.iqr[1],
                estimate.decision,
            )
            .expect("String writes cannot fail");
        }
        output.push('\n');
        output
    }
}

pub(super) fn size(value: u64) -> String {
    if value == 0 {
        "empty".into()
    } else if value.is_multiple_of(1 << 20) {
        format!("{} MiB", value >> 20)
    } else if value.is_multiple_of(1 << 10) {
        format!("{} KiB", value >> 10)
    } else {
        format!("{value} B")
    }
}

#[cfg(feature = "plot")]
pub(super) fn system_label(analysis: &Analysis, index: usize) -> String {
    let estimate = &analysis.estimates[index];
    let condition = &estimate.condition;
    let mut label = format!(
        "{} · {} payload · queue capacity {} · {} messages in flight · {} shared memory",
        estimate.candidate,
        size(condition.payload),
        condition.capacity,
        condition.in_flight,
        size(condition.memory),
    );
    if analysis.specification.mode == "focused" {
        label += &format!(" · {:?}", analysis.decisions[index]);
    }
    label
}

pub(super) fn analyses(studies: &[Evidence]) -> Result<Vec<Analysis>, Error> {
    let mut analyses = studies.iter().map(analyze).collect::<Result<Vec<_>, _>>()?;
    analyses.sort_by(|left, right| {
        left.family
            .key
            .cmp(right.family.key)
            .then_with(|| left.evidence.cmp(&right.evidence))
    });
    if analyses
        .windows(2)
        .any(|pair| pair[0].evidence == pair[1].evidence)
    {
        return Err(Error::Duplicate);
    }
    Ok(analyses)
}

pub fn report(paths: &[String]) -> Result<Report, String> {
    struct Input {
        path: String,
        schema: study::SchemaId,
        encoded: String,
        digest: String,
        bytes: u64,
    }
    let inputs = paths
        .iter()
        .map(|path| {
            let bytes = std::fs::read(path).map_err(|error| format!("{path}: {error}"))?;
            let encoded = String::from_utf8(bytes).map_err(|error| error.to_string())?;
            Ok(Input {
                path: path.clone(),
                schema: study::schema_str(&encoded).map_err(|error| format!("{error:?}"))?,
                digest: blake3::hash(encoded.as_bytes()).to_hex().to_string(),
                bytes: encoded.len() as u64,
                encoded,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut calibrations = Vec::new();
    let mut systems = Vec::new();
    let mut mechanisms = Vec::new();
    let mut sources = Vec::new();
    for input in &inputs {
        if input.schema.name == <system::System as study::Schema>::NAME {
            let evidence = study::load_str::<system::System>(&input.encoded)
                .map_err(|error| format!("{}: {error:?}", input.path))?;
            sources.push(Source {
                evidence: study::study_id::<system::System>(&evidence.header),
                digest: input.digest.clone(),
                bytes: input.bytes,
            });
            systems.push(evidence);
        } else if input.schema.name == <pilot::Calibration as study::Schema>::NAME {
            let evidence = study::load_str::<pilot::Calibration>(&input.encoded)
                .map_err(|error| format!("{}: {error:?}", input.path))?;
            let id = study::study_id::<pilot::Calibration>(&evidence.header);
            sources.push(Source {
                evidence: id.clone(),
                digest: input.digest.clone(),
                bytes: input.bytes,
            });
            calibrations.push(CalibrationSection {
                evidence: id,
                context: evidence.header.context.clone(),
                cases: evidence.header.cases.len(),
                budget_ms: evidence.header.run.budget_ms,
            });
        }
    }
    for input in inputs.iter().filter(|input| {
        input.schema.name != <system::System as study::Schema>::NAME
            && input.schema.name != <pilot::Calibration as study::Schema>::NAME
    }) {
        let analysis = fixture::analyze_str(&input.schema, &input.encoded, &systems)?;
        sources.push(Source {
            evidence: analysis.evidence.clone(),
            digest: input.digest.clone(),
            bytes: input.bytes,
        });
        mechanisms.push(analysis);
    }
    let analyses = analyses(&systems).map_err(|error| format!("{error:?}"))?;
    mechanisms.sort_by(|left, right| {
        left.schema
            .cmp(&right.schema)
            .then_with(|| left.evidence.cmp(&right.evidence))
    });
    if mechanisms
        .windows(2)
        .any(|pair| pair[0].evidence == pair[1].evidence)
    {
        return Err("duplicate Mechanism evidence".into());
    }
    let mut sections = Vec::with_capacity(mechanisms.len());
    for mechanism in mechanisms {
        let originals = systems
            .iter()
            .filter(|system| {
                study::context_id(&system.header.context) == study::context_id(&mechanism.context)
                    && mechanism.estimates.iter().all(|estimate| {
                        system
                            .header
                            .cases
                            .iter()
                            .any(|case| study::case_id::<system::System>(case) == estimate.target)
                    })
            })
            .collect::<Vec<_>>();
        if originals.len() > 1 {
            return Err(format!(
                "duplicate original System evidence for {}",
                mechanism.schema
            ));
        }
        let attribution = if let Some(original) = originals.first() {
            let original = *original;
            let interventions = systems
                .iter()
                .filter(|candidate| !core::ptr::eq(*candidate, original))
                .filter(|candidate| same_cases(original, candidate))
                .collect::<Vec<_>>();
            if interventions.len() > 1 {
                return Err(format!(
                    "duplicate intervention evidence for {}",
                    mechanism.schema
                ));
            }
            Some(
                attribute(original, Some(&mechanism), interventions.first().copied())
                    .map_err(|error| format!("{error:?}"))?,
            )
        } else {
            None
        };
        sections.push(MechanismSection {
            analysis: mechanism,
            attribution,
        });
    }
    sources.sort_by(|left, right| left.evidence.cmp(&right.evidence));
    if sources
        .windows(2)
        .any(|pair| pair[0].evidence == pair[1].evidence)
    {
        return Err("duplicate artifact evidence".into());
    }
    calibrations.sort_by(|left, right| left.evidence.cmp(&right.evidence));
    Ok(Report {
        calibrations,
        systems: analyses,
        mechanisms: sections,
        sources,
    })
}

impl Report {
    pub fn markdown(&self) -> String {
        use core::fmt::Write;
        let mut output = String::from(
            "# IPC study\n\n## Admitted source artifacts\n\n| evidence | bytes | BLAKE3 |\n|---|---:|---|\n",
        );
        for source in &self.sources {
            writeln!(
                output,
                "| `{}` | {} | `{}` |",
                source.evidence, source.bytes, source.digest
            )
            .expect("String writes cannot fail");
        }
        output.push('\n');
        for calibration in &self.calibrations {
            writeln!(
                output,
                "## Calibration evidence\n\nDescriptive calibration for {} registered cases on `{}` / `{}`. Evidence: `{}`. Limit: {} ms.\n",
                calibration.cases,
                calibration.context.host.os,
                calibration.context.compiler.target,
                calibration.evidence,
                calibration.budget_ms,
            )
            .expect("String writes cannot fail");
        }
        self.systems
            .iter()
            .for_each(|analysis| output.push_str(&analysis.markdown()));
        for section in &self.mechanisms {
            let authority = section
                .attribution
                .as_ref()
                .map_or(Authority::Descriptive, |value| value.authority);
            if let Some(link) = &section.attribution {
                writeln!(
                    output,
                    "Causal link: System `{}`; Mechanism `{}`; intervention `{}`; authority `{:?}`.\n",
                    link.system,
                    link.mechanism.as_deref().unwrap_or("missing"),
                    link.intervention.as_deref().unwrap_or("missing"),
                    link.authority,
                )
                .expect("String writes cannot fail");
            }
            output.push_str(&section.analysis.markdown(authority));
        }
        output
    }
}

pub fn command(paths: &[String]) -> Result<String, String> {
    report(paths).map(|report| report.markdown())
}
