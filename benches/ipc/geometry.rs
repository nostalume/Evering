use std::collections::{BTreeMap, BTreeSet};

pub type Id = u64;
pub type Owner = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Reserve { id: Id, owner: Owner, bytes: u64 },
    Release { id: Id },
    Transfer { id: Id, to: Owner },
    Crash { owner: Owner },
    Reap { owner: Owner },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trace {
    pub events: Vec<Event>,
}

impl Trace {
    pub fn synthetic(events: impl IntoIterator<Item = Event>) -> Self {
        Self {
            events: events.into_iter().collect(),
        }
    }

    fn projected(events: impl IntoIterator<Item = Event>) -> Self {
        Self {
            events: events.into_iter().collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    BlockTooLarge,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Reserved,
    Rejected(Rejection),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Divergence {
    pub event: usize,
    pub static_model: Outcome,
    pub compact_bound: Outcome,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub accepted: u64,
    pub rejected: u64,
    pub rejections: Vec<(usize, Rejection)>,
    pub requested_bytes: u64,
    pub peak_live_bytes: u64,
    pub peak_physical_bytes: u64,
    pub peak_fragmentation: u64,
    pub spills: u64,
    pub recovery_work: u64,
    pub reclaimed_bytes: u64,
    pub stranded_bytes: u64,
    pub metadata_bytes: u64,
    pub largest: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comparison {
    pub static_model: Report,
    pub compact_bound: Report,
    pub first_divergence: Option<Divergence>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Allocation {
    id: Id,
    owner: Owner,
    requested: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Class {
    bytes: u64,
    slots: Vec<Option<Allocation>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Geometry {
    classes: Vec<Class>,
}

impl Geometry {
    pub fn balanced(extent: u64, min: u64, max: u64) -> Result<Self, &'static str> {
        let min = min
            .max(64)
            .checked_next_power_of_two()
            .ok_or("range overflow")?;
        let max = max
            .max(min)
            .checked_next_power_of_two()
            .ok_or("range overflow")?;
        let mut sizes = std::iter::successors(Some(min), |size| size.checked_mul(2))
            .take_while(|size| *size <= max)
            .collect::<Vec<_>>();
        while let Some(last) = sizes.len().checked_sub(1) {
            let minimum = 64_u64
                .checked_shl((last / 2) as u32)
                .ok_or("range overflow")?;
            if Self::weighted_bytes(&sizes, minimum).is_some_and(|used| used <= extent) {
                let mut low = minimum;
                let mut high = extent / 65 + 1;
                while low + 1 < high {
                    let middle = low + (high - low) / 2;
                    if Self::weighted_bytes(&sizes, middle).is_some_and(|used| used <= extent) {
                        low = middle;
                    } else {
                        high = middle;
                    }
                }
                let classes = sizes
                    .iter()
                    .enumerate()
                    .map(|(index, size)| (*size, (low >> (index / 2)) as usize))
                    .collect::<Vec<_>>();
                return Self::from_classes(extent, &classes);
            }
            sizes.pop();
        }
        Err("extent too small")
    }

    pub fn from_classes(extent: u64, classes: &[(u64, usize)]) -> Result<Self, &'static str> {
        if classes.is_empty()
            || classes
                .iter()
                .any(|&(bytes, slots)| bytes < 64 || !bytes.is_power_of_two() || slots == 0)
            || classes.windows(2).any(|pair| pair[0].0 >= pair[1].0)
        {
            return Err("invalid classes");
        }
        let used = classes.iter().try_fold(0_u64, |used, &(bytes, slots)| {
            used.checked_add(bytes.checked_mul(slots as u64)?)?
                .checked_add(slots as u64)
        });
        if used.is_none_or(|used| used > extent) {
            return Err("classes exceed extent");
        }
        Ok(Self {
            classes: classes
                .iter()
                .map(|&(bytes, slots)| Class {
                    bytes,
                    slots: vec![None; slots],
                })
                .collect(),
        })
    }

    pub fn replay(&self, trace: &Trace) -> Report {
        let mut model = self.clone();
        let mut report = Report {
            metadata_bytes: model
                .classes
                .iter()
                .map(|class| class.slots.len() as u64)
                .sum(),
            ..Report::default()
        };
        let mut locations = BTreeMap::new();
        let mut dead = BTreeSet::new();
        for (at, event) in trace.events.iter().copied().enumerate() {
            match event {
                Event::Reserve { id, owner, bytes } => {
                    report.requested_bytes += bytes;
                    let preferred = model.classes.partition_point(|class| class.bytes < bytes);
                    let Some((class, slot)) = model.claim(preferred, id, owner, bytes) else {
                        let rejection = if preferred == model.classes.len() {
                            Rejection::BlockTooLarge
                        } else {
                            Rejection::Unavailable
                        };
                        report.rejected += 1;
                        report.rejections.push((at, rejection));
                        model.sample(&mut report);
                        continue;
                    };
                    report.accepted += 1;
                    report.spills += u64::from(class != preferred);
                    locations.insert(id, (class, slot));
                }
                Event::Release { id } => model.release(id, &mut locations),
                Event::Transfer { id, to } => {
                    if let Some(&(class, slot)) = locations.get(&id) {
                        model.classes[class].slots[slot].as_mut().unwrap().owner = to;
                    }
                }
                Event::Crash { owner } => {
                    dead.insert(owner);
                }
                Event::Reap { owner } => {
                    if dead.contains(&owner) {
                        for class in &mut model.classes {
                            for slot in &mut class.slots {
                                report.recovery_work += 1;
                                if slot.is_some_and(|allocation| allocation.owner == owner) {
                                    let allocation = slot.take().unwrap();
                                    locations.remove(&allocation.id);
                                    report.reclaimed_bytes += class.bytes;
                                }
                            }
                        }
                    }
                }
            }
            model.sample(&mut report);
        }
        report.stranded_bytes = model
            .classes
            .iter()
            .flat_map(|class| class.slots.iter().map(move |slot| (class.bytes, slot)))
            .filter(|(_, slot)| slot.is_some_and(|allocation| dead.contains(&allocation.owner)))
            .map(|(bytes, _)| bytes)
            .sum();
        report
    }

    pub fn compare_compacting(&self, trace: &Trace) -> Comparison {
        let static_model = self.replay(trace);
        let compact_bound = self.compact_replay(trace);
        let static_outcomes = outcomes(trace, &static_model);
        let compact_outcomes = outcomes(trace, &compact_bound);
        let first_divergence = static_outcomes.into_iter().zip(compact_outcomes).find_map(
            |((event, static_model), (_, compact_bound))| {
                (static_model != compact_bound).then_some(Divergence {
                    event,
                    static_model,
                    compact_bound,
                })
            },
        );
        Comparison {
            static_model,
            compact_bound,
            first_divergence,
        }
    }

    fn compact_replay(&self, trace: &Trace) -> Report {
        let capacity: u64 = self
            .classes
            .iter()
            .map(|class| class.bytes * class.slots.len() as u64)
            .sum();
        let max = self.classes.last().unwrap().bytes;
        let mut allocations = BTreeMap::new();
        let mut dead = BTreeSet::new();
        let mut report = Report {
            metadata_bytes: self
                .classes
                .iter()
                .map(|class| class.slots.len() as u64)
                .sum(),
            ..Report::default()
        };
        for (at, event) in trace.events.iter().copied().enumerate() {
            match event {
                Event::Reserve { id, owner, bytes } => {
                    report.requested_bytes += bytes;
                    let physical = bytes.max(64).checked_next_power_of_two();
                    let used: u64 = allocations.values().map(|&(_, _, physical)| physical).sum();
                    let rejection = if physical.is_none_or(|bytes| bytes > max) {
                        Some(Rejection::BlockTooLarge)
                    } else if physical.unwrap() > capacity - used {
                        Some(Rejection::Unavailable)
                    } else {
                        None
                    };
                    if let Some(rejection) = rejection {
                        report.rejected += 1;
                        report.rejections.push((at, rejection));
                    } else {
                        report.accepted += 1;
                        allocations.insert(id, (owner, bytes, physical.unwrap()));
                    }
                }
                Event::Release { id } => {
                    allocations.remove(&id);
                }
                Event::Transfer { id, to } => {
                    if let Some(allocation) = allocations.get_mut(&id) {
                        allocation.0 = to;
                    }
                }
                Event::Crash { owner } => {
                    dead.insert(owner);
                }
                Event::Reap { owner } if dead.contains(&owner) => {
                    report.recovery_work += allocations.len() as u64;
                    allocations.retain(|_, allocation| {
                        if allocation.0 == owner {
                            report.reclaimed_bytes += allocation.2;
                            false
                        } else {
                            true
                        }
                    });
                }
                Event::Reap { .. } => {}
            }
            let live = allocations.values().map(|value| value.1).sum::<u64>();
            let physical = allocations.values().map(|value| value.2).sum::<u64>();
            report.peak_live_bytes = report.peak_live_bytes.max(live);
            report.peak_physical_bytes = report.peak_physical_bytes.max(physical);
            report.peak_fragmentation = report.peak_fragmentation.max(physical - live);
            report
                .largest
                .push(if capacity - physical >= max { max } else { 0 });
        }
        report.stranded_bytes = allocations
            .values()
            .filter(|allocation| dead.contains(&allocation.0))
            .map(|allocation| allocation.2)
            .sum();
        report
    }

    fn weighted_bytes(sizes: &[u64], base: u64) -> Option<u64> {
        sizes
            .iter()
            .enumerate()
            .try_fold(0_u64, |used, (index, size)| {
                let slots = base >> (index / 2);
                used.checked_add(size.checked_mul(slots)?)?
                    .checked_add(slots)
            })
    }

    fn claim(
        &mut self,
        from: usize,
        id: Id,
        owner: Owner,
        requested: u64,
    ) -> Option<(usize, usize)> {
        self.classes
            .iter_mut()
            .enumerate()
            .skip(from)
            .find_map(|(class, value)| {
                value
                    .slots
                    .iter_mut()
                    .enumerate()
                    .find_map(|(slot, state)| {
                        state.is_none().then(|| {
                            *state = Some(Allocation {
                                id,
                                owner,
                                requested,
                            });
                            (class, slot)
                        })
                    })
            })
    }

    fn release(&mut self, id: Id, locations: &mut BTreeMap<Id, (usize, usize)>) {
        if let Some((class, slot)) = locations.remove(&id) {
            self.classes[class].slots[slot] = None;
        }
    }

    fn sample(&self, report: &mut Report) {
        let (live, physical) = self
            .classes
            .iter()
            .flat_map(|class| class.slots.iter().map(move |slot| (class.bytes, slot)))
            .filter_map(|(bytes, slot)| slot.map(|allocation| (allocation.requested, bytes)))
            .fold((0, 0), |(live, physical), value| {
                (live + value.0, physical + value.1)
            });
        report.peak_live_bytes = report.peak_live_bytes.max(live);
        report.peak_physical_bytes = report.peak_physical_bytes.max(physical);
        report.peak_fragmentation = report.peak_fragmentation.max(physical - live);
        report.largest.push(
            self.classes
                .iter()
                .rev()
                .find(|class| class.slots.iter().any(Option::is_none))
                .map_or(0, |class| class.bytes),
        );
    }
}

fn outcomes(trace: &Trace, report: &Report) -> Vec<(usize, Outcome)> {
    let rejected = report
        .rejections
        .iter()
        .copied()
        .collect::<BTreeMap<_, _>>();
    trace
        .events
        .iter()
        .enumerate()
        .filter(|(_, value)| matches!(value, Event::Reserve { .. }))
        .map(|(event, _)| {
            (
                event,
                rejected
                    .get(&event)
                    .copied()
                    .map_or(Outcome::Reserved, Outcome::Rejected),
            )
        })
        .collect()
}

pub fn evidence_report(studies: &[super::system::Evidence]) -> Result<String, &'static str> {
    let mut cases = BTreeSet::new();
    for study in studies {
        super::analysis::analyze(study).map_err(|_| "system evidence not admitted")?;
        for row in &study.observations {
            let Some(observed) =
                Some(&row.measure.observed).filter(|value| value.allocator.is_some())
            else {
                continue;
            };
            let extent = observed.extent.ok_or("allocator extent missing")?;
            let bytes = observed.payload.max(64);
            if observed.window == 0 || observed.window > 4096 {
                return Err("unsupported projected window");
            }
            cases.insert((bytes, extent, observed.window));
        }
    }
    let mut output = String::from(
        "geometry-v1\nsource\tpayload\textent\twindow\tstatic_ok\tstatic_too_large\tstatic_unavailable\treplacement_ok\tcompact_ok\trequested_bytes\tpeak_live_bytes\tpeak_physical_bytes\tpeak_fragmentation\tspills\tmin_largest\trecovery_work\tmetadata_bytes\tfirst_divergence\tbuddy_gate\n",
    );
    for (bytes, extent, window) in cases {
        let trace = projected_window(bytes, window);
        let static_geometry = Geometry::balanced(extent, 64, 64 * 1024)?;
        let replacement = Geometry::balanced(extent, bytes, bytes)?;
        let comparison = static_geometry.compare_compacting(&trace);
        let replacement = replacement.replay(&trace);
        let count = |reason| {
            comparison
                .static_model
                .rejections
                .iter()
                .filter(|(_, value)| *value == reason)
                .count()
        };
        let divergence = comparison
            .first_divergence
            .map_or_else(|| "none".into(), |value| value.event.to_string());
        output.push_str(&format!(
            "projected\t{bytes}\t{extent}\t{window}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{divergence}\tfalse\n",
            comparison.static_model.accepted,
            count(Rejection::BlockTooLarge),
            count(Rejection::Unavailable),
            replacement.accepted,
            comparison.compact_bound.accepted,
            comparison.static_model.requested_bytes,
            comparison.static_model.peak_live_bytes,
            comparison.static_model.peak_physical_bytes,
            comparison.static_model.peak_fragmentation,
            comparison.static_model.spills,
            comparison.static_model.largest.iter().copied().min().unwrap_or(0),
            comparison.static_model.recovery_work,
            comparison.static_model.metadata_bytes,
        ));
    }
    Ok(output)
}

fn projected_window(bytes: u64, window: u64) -> Trace {
    let mut events = Vec::with_capacity(window as usize * 6);
    for round in 0..2 {
        for id in 0..window {
            events.push(Event::Reserve {
                id: round * window + id,
                owner: 1,
                bytes,
            });
        }
        for id in 0..window {
            let id = round * window + id;
            events.push(Event::Transfer { id, to: 2 });
            events.push(Event::Release { id });
        }
    }
    Trace::projected(events)
}
