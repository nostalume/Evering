use std::{fs, time::Instant};

/// Provenance and resolution of [`Accounting::runtime_us`], read from the task's own
/// entry under whichever of the two procfs roots [`sample`] or [`sample_here`] was
/// asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum Source {
    /// `schedstat` run time, which the kernel reports in nanoseconds.
    Nanoseconds,
    /// `stat` user plus system ticks at the 100 Hz user clock, so a
    /// delta is only resolved to whole 10 ms ticks.
    Ticks,
}

/// Cost attributed to one timed phase, from the counters of the task that ran it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Accounting {
    pub source: Source,
    pub runtime_us: u64,
    pub voluntary_switches: u64,
    pub involuntary_switches: u64,
}

/// Worker-process cost, carried with the width of the bracket it was measured
/// over. The bracket travels with the figure because a task's own delta is
/// only comparable to another window once its own width is known, and the
/// worker's bracket necessarily runs past the coordinator's timed window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Witness {
    pub cost: Accounting,
    pub bracket_ns: u64,
}

/// One task's counter reading: `status` for the two switch counts and `schedstat`,
/// falling back to `stat`, for the run time. The fields are exposed because a reading
/// is not evidence on its own: only [`Sample::since`] turns a pair into one, and a pair
/// taken from two tasks would compare two unrelated counters.
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

/// One task's counters, read from the given procfs root.
#[cfg(target_os = "linux")]
fn reading(at: &str) -> Option<Sample> {
    let status = fs::read_to_string(format!("{at}/status")).ok()?;
    let (voluntary, involuntary) = text::switches(&status)?;
    let schedstat = fs::read_to_string(format!("{at}/schedstat")).ok();
    let runtime = match schedstat {
        Some(schedstat) => text::schedstat(&schedstat),
        None => text::stat(&fs::read_to_string(format!("{at}/stat")).ok()?),
    }?;
    Some(Sample {
        runtime,
        voluntary,
        involuntary,
    })
}

/// The thread-group leader's counters, which is what a single-threaded host owes a
/// trial: `sample()` is the whole process whenever the timed phase ran on the thread
/// that `/proc/self` names.
#[cfg(target_os = "linux")]
pub fn sample() -> Option<Sample> {
    reading("/proc/self")
}

/// The counters of the calling task. `/proc/thread-self` is the kernel's spelling of
/// "this task", so a bracket taken here never reports another thread's scheduling.
#[cfg(target_os = "linux")]
pub fn sample_here() -> Option<Sample> {
    reading("/proc/thread-self")
}

#[cfg(not(target_os = "linux"))]
pub fn sample() -> Option<Sample> {
    None
}

#[cfg(not(target_os = "linux"))]
pub fn sample_here() -> Option<Sample> {
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

/// One edge of a phase, marked so a trace of a whole run can be cut there.
///
/// The marker is a `stat` of a name that is never created, so it costs exactly one
/// kernel entry and that entry carries the name as its own argument. A counting pass
/// over a shared stream can therefore keep only the entries between a phase's opening
/// and closing markers, which is what separates the timed work from the process
/// bootstrap — the connect retry spin and the host's own shell probe — that the same
/// stream otherwise records inside every window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    MeasuredOpen,
    MeasuredClose,
    ServedOpen,
    ServedClose,
}

impl Marker {
    fn name(self) -> &'static str {
        match self {
            Self::MeasuredOpen => "measured-open",
            Self::MeasuredClose => "measured-close",
            Self::ServedOpen => "served-open",
            Self::ServedClose => "served-close",
        }
    }
}

// Resolved once per process: a trial that marks hundreds of phases must not re-read the
// environment to learn that it marks none.
static MARKER_DIRECTORY: std::sync::OnceLock<Option<std::path::PathBuf>> =
    std::sync::OnceLock::new();

/// `EVERING_MARKER_DIRECTORY` names the directory whose absent entries the markers
/// stand for, and marking exists only while it is set: a registered or timed run
/// leaves it unset and pays no entry for a boundary it never reports.
///
/// Nothing is created, so a marker leaves no trace on the filesystem and cannot collide
/// with another process's file; the path's existence would change the entry's result but
/// never its count, which is all the counting pass reads.
pub fn mark(marker: Marker) {
    let Some(directory) = MARKER_DIRECTORY
        .get_or_init(|| std::env::var_os("EVERING_MARKER_DIRECTORY").map(std::path::PathBuf::from))
        .as_deref()
    else {
        return;
    };
    let _ = fs::metadata(directory.join(marker.name()));
}

/// One bracketing pair taken on the task that serves the phase, around the phase it
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
        (trailer != ABSENT).then(|| {
            let started = Instant::now();
            let before = sample_here();
            mark(Marker::ServedOpen);
            Self { started, before }
        })
    }

    /// The close is marked before the counters are read, so a marked window holds the
    /// phase and neither bracket's own reads: the accounting cost a witness reports is
    /// the reading a marker deliberately keeps out of the count it accompanies.
    pub fn witness(self) -> Option<Witness> {
        mark(Marker::ServedClose);
        let cost = self.before?.since(sample_here()?)?;
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
    /// which keeps the extra file off hosts that could only ever report absence. The
    /// probe is the reading [`Phase`] will take, so a host that has `/proc/self` but no
    /// `/proc/thread-self` claims nothing rather than a bracket it cannot account for.
    /// `create_new` makes a path another user pre-created a hard setup failure
    /// instead of an overwrite.
    pub fn claim() -> Result<Option<Self>, String> {
        if sample_here().is_none() {
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
