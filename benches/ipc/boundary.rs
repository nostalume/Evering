use std::{fs, time::Instant};

/// Provenance and resolution of [`Accounting::runtime_us`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum Source {
    /// `/proc/self/schedstat` run time, which the kernel reports in nanoseconds.
    Nanoseconds,
    /// `/proc/self/stat` user plus system ticks at the 100 Hz user clock, so a
    /// delta is only resolved to whole 10 ms ticks.
    Ticks,
}

/// Coordinator-process cost attributed to one timed phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Accounting {
    pub source: Source,
    pub runtime_us: u64,
    pub voluntary_switches: u64,
    pub involuntary_switches: u64,
}

/// Worker-process cost, carried with the width of the bracket it was measured
/// over. The bracket travels with the figure because a process-wide delta is
/// only comparable to another window once its own width is known, and the
/// worker's bracket necessarily runs past the coordinator's timed window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Witness {
    pub cost: Accounting,
    pub bracket_ns: u64,
}

/// A process-wide counter reading. The fields are exposed because a reading is
/// not evidence on its own: only [`Sample::since`] turns a pair into one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub runtime: (u64, Source),
    pub voluntary: u64,
    pub involuntary: u64,
}

// Parsed from procfs text rather than read through libc, so the same fixtures
// run on hosts that expose no /proc at all.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub mod text {
    use super::{Accounting, Source, Witness};

    const TICK_US: u64 = 10_000;

    pub fn schedstat(text: &str) -> Option<(u64, Source)> {
        let nanoseconds: u64 = text.split_whitespace().next()?.parse().ok()?;
        Some((nanoseconds / 1_000, Source::Nanoseconds))
    }

    pub fn stat(text: &str) -> Option<(u64, Source)> {
        // The command name is parenthesised and may itself contain `) `, so the
        // numeric fields are only addressable after the final separator.
        let fields: Vec<&str> = text.rsplit_once(") ")?.1.split_whitespace().collect();
        let utime: u64 = fields.get(11)?.parse().ok()?;
        let stime: u64 = fields.get(12)?.parse().ok()?;
        Some((
            utime.checked_add(stime)?.checked_mul(TICK_US)?,
            Source::Ticks,
        ))
    }

    pub fn switches(text: &str) -> Option<(u64, u64)> {
        let count = |key: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(key))
                .and_then(|value| value.trim().parse::<u64>().ok())
        };
        // `nonvoluntary_ctxt_switches:` does not begin with the voluntary key.
        Some((
            count("voluntary_ctxt_switches:")?,
            count("nonvoluntary_ctxt_switches:")?,
        ))
    }

    pub fn encode(witness: &Witness) -> String {
        let source = match witness.cost.source {
            Source::Nanoseconds => "nanoseconds",
            Source::Ticks => "ticks",
        };
        format!(
            "source={source} runtime_us={} voluntary={} involuntary={} bracket_ns={}",
            witness.cost.runtime_us,
            witness.cost.voluntary_switches,
            witness.cost.involuntary_switches,
            witness.bracket_ns,
        )
    }

    pub fn decode(text: &str) -> Option<Witness> {
        let mut tokens = text.split_whitespace();
        // The trailer is one fixed-shape line, so any other token count is not a
        // witness at all: a half-written or concatenated file must read as
        // absence rather than as the first figure it happens to resemble.
        let (Some(source), Some(runtime), Some(voluntary), Some(involuntary), Some(bracket), None) = (
            tokens.next(),
            tokens.next(),
            tokens.next(),
            tokens.next(),
            tokens.next(),
            tokens.next(),
        ) else {
            return None;
        };
        let value = |token: &str, key: &str| token.strip_prefix(key)?.parse::<u64>().ok();
        let source = match source.strip_prefix("source=")? {
            "nanoseconds" => Source::Nanoseconds,
            "ticks" => Source::Ticks,
            _ => return None,
        };
        Some(Witness {
            cost: Accounting {
                source,
                runtime_us: value(runtime, "runtime_us=")?,
                voluntary_switches: value(voluntary, "voluntary=")?,
                involuntary_switches: value(involuntary, "involuntary=")?,
            },
            bracket_ns: value(bracket, "bracket_ns=")?,
        })
    }
}

#[cfg(target_os = "linux")]
pub fn sample() -> Option<Sample> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let (voluntary, involuntary) = text::switches(&status)?;
    let schedstat = fs::read_to_string("/proc/self/schedstat").ok();
    let runtime = match schedstat {
        Some(schedstat) => text::schedstat(&schedstat),
        None => text::stat(&fs::read_to_string("/proc/self/stat").ok()?),
    }?;
    Some(Sample {
        runtime,
        voluntary,
        involuntary,
    })
}

#[cfg(not(target_os = "linux"))]
pub fn sample() -> Option<Sample> {
    None
}

impl Sample {
    /// The counters only rise, and a pair from different sources would report a
    /// delta whose resolution is unknown, so either condition discards a window.
    pub fn since(self, later: Self) -> Option<Accounting> {
        if self.runtime.1 != later.runtime.1 {
            return None;
        }
        Some(Accounting {
            source: later.runtime.1,
            runtime_us: later.runtime.0.checked_sub(self.runtime.0)?,
            voluntary_switches: later.voluntary.checked_sub(self.voluntary)?,
            involuntary_switches: later.involuntary.checked_sub(self.involuntary)?,
        })
    }
}

/// The argv placeholder for a host that claimed no trailer.
pub const ABSENT: &str = "-";

/// One bracketing pair taken inside the worker process around the phase it
/// served. The start is the probe frame that closes setup, so warmup stays out
/// of the figure and no subtraction is needed for work the coordinator timed.
pub struct Phase {
    started: Instant,
    before: Option<Sample>,
}

impl Phase {
    /// `None` when no accounting was requested, so a phase that cannot be
    /// reported costs the worker no reads at all.
    pub fn begin(trailer: &str) -> Option<Self> {
        (trailer != ABSENT).then(|| Self {
            started: Instant::now(),
            before: sample(),
        })
    }

    pub fn witness(self) -> Option<Witness> {
        let cost = self.before?.since(sample()?)?;
        Some(Witness {
            cost,
            bracket_ns: self.started.elapsed().as_nanos().max(1) as u64,
        })
    }

    /// A host that cannot observe the boundary leaves the trailer empty rather
    /// than writing a figure it did not measure, and the coordinator reads
    /// absence. Failing to write at all is a worker error: the trial asked for a
    /// figure it could not deliver.
    pub fn record(self, trailer: &str) -> Result<(), String> {
        let Some(witness) = self.witness() else {
            return Ok(());
        };
        fs::write(trailer, text::encode(&witness)).map_err(|error| error.to_string())
    }
}

/// The channel a worker uses to hand its own accounting back: a file the
/// coordinator creates before spawning, the worker fills as its last act, and
/// the coordinator reads only after the child is reaped. No part of it sits
/// inside a timed window, and it is the same channel for every worker entry
/// point, so no transfer protocol grows a new message to carry instrumentation.
pub struct Trailer {
    path: std::path::PathBuf,
    argument: String,
}

static NEXT_TRAILER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl Trailer {
    /// A trailer exists exactly when this host can observe the boundary at all,
    /// which keeps the extra file off hosts that could only ever report absence.
    /// `create_new` makes a path another user pre-created a hard setup failure
    /// instead of an overwrite.
    pub fn claim() -> Result<Option<Self>, String> {
        if sample().is_none() {
            return Ok(None);
        }
        let sequence = NEXT_TRAILER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "evering-boundary-{}-{sequence}.txt",
            std::process::id()
        ));
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| error.to_string())?;
        Ok(Some(Self {
            argument: path.to_string_lossy().into_owned(),
            path,
        }))
    }

    pub fn argument(&self) -> &str {
        &self.argument
    }

    /// An empty, corrupt or missing trailer reads as absence: a child killed at
    /// the deadline exits having written nothing, which is not the same
    /// statement as a worker that consumed no run time.
    pub fn read(&self) -> Option<Witness> {
        fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| text::decode(&text))
    }
}

impl Drop for Trailer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
