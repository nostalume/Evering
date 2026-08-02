use super::model::{Cell, Journal, Scheduled};

pub fn progress(kind: &str, done: usize, total: usize, cell: &Cell) -> String {
    format!(
        "{kind} {done}/{total}: {} payload={} capacity={} in-flight={}",
        cell.arm, cell.payload, cell.capacity, cell.in_flight
    )
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Identity {
    pub algorithm: u32,
    pub family: String,
    pub family_revision: u32,
    pub revision: String,
    pub diff: String,
    pub target: String,
    pub rustc: String,
    pub environment: String,
    pub seed: u64,
    pub warmup: u64,
    pub timeout_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Observation {
    pub count: u64,
    pub elapsed_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Row {
    pub cell: Cell,
    pub count: u64,
    pub observations: Vec<Observation>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Manifest {
    pub identity: Identity,
    pub rows: Vec<Row>,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
enum Line {
    Header { identity: Identity, expected: usize },
    Row(Row),
    Abort { cell: Cell, reason: String },
    End { rows: usize, digest: String },
}

pub fn record(
    path: &std::path::Path,
    identity: Identity,
    expected: usize,
    rows: impl IntoIterator<Item = (Cell, Result<Row, String>)>,
) -> Result<String, String> {
    let mut journal = Journal::create(path)?;
    journal.append(&Line::Header {
        identity: identity.clone(),
        expected,
    })?;
    let mut manifest = Manifest {
        identity,
        rows: Vec::with_capacity(expected),
    };
    for (cell, result) in rows {
        let row = match result {
            Ok(row) => row,
            Err(error) => {
                journal.append(&Line::Abort {
                    cell,
                    reason: error.replace(['\t', '\n', '\r'], " "),
                })?;
                return Err(error);
            }
        };
        validate(&row, manifest.identity.warmup)?;
        if manifest.rows.len() == expected
            || manifest.rows.iter().any(|value| value.cell == row.cell)
        {
            return Err("duplicate or excess pilot row".into());
        }
        journal.append(&Line::Row(row.clone()))?;
        manifest.rows.push(row);
    }
    if manifest.rows.len() != expected {
        return Err("incomplete pilot manifest".into());
    }
    let digest = journal.digest();
    journal.seal(&Line::End {
        rows: manifest.rows.len(),
        digest: digest.clone(),
    })?;
    Ok(digest)
}

pub fn load(path: &std::path::Path) -> Result<(Manifest, String), String> {
    let input = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    if !input.ends_with('\n') {
        return Err("truncated pilot manifest".into());
    }
    let mut header = None;
    let mut rows = Vec::new();
    let mut end = None;
    let mut digest = blake3::Hasher::new();
    for raw in input.split_inclusive('\n') {
        let line: Line = serde_json::from_str(raw.strip_suffix('\n').unwrap())
            .map_err(|error| error.to_string())?;
        match line {
            Line::Header { identity, expected } if header.is_none() && rows.is_empty() => {
                header = Some((identity, expected));
                digest.update(raw.as_bytes());
            }
            Line::Row(row) if header.is_some() && end.is_none() => {
                rows.push(row);
                digest.update(raw.as_bytes());
            }
            Line::End { rows, digest } if header.is_some() && end.is_none() => {
                end = Some((rows, digest));
            }
            Line::Abort { .. } => return Err("aborted pilot manifest".into()),
            _ => return Err("invalid pilot manifest".into()),
        }
    }
    let (identity, expected) = header.ok_or("missing pilot header")?;
    let (sealed_rows, sealed_digest) = end.ok_or("incomplete pilot manifest")?;
    if rows.len() != expected
        || rows.len() != sealed_rows
        || sealed_digest != digest.finalize().to_hex().as_str()
    {
        return Err("pilot digest mismatch".into());
    }
    rows.iter()
        .try_for_each(|row| validate(row, identity.warmup))?;
    Ok((Manifest { identity, rows }, sealed_digest))
}

pub fn admit(
    manifest: &Manifest,
    identity: &Identity,
    scheduled: &[Scheduled],
) -> Result<Vec<u64>, String> {
    use std::collections::HashSet;

    if identity.algorithm != 3 || &manifest.identity != identity {
        return Err("foreign pilot identity".into());
    }
    let expected: HashSet<_> = scheduled.iter().map(|entry| entry.cell()).collect();
    let mut present = HashSet::new();
    for row in &manifest.rows {
        if !present.insert(row.cell.clone()) || !expected.contains(&row.cell) {
            return Err("duplicate or foreign pilot row".into());
        }
        validate(row, identity.warmup)?;
    }
    if present != expected {
        return Err("incomplete pilot manifest".into());
    }
    scheduled
        .iter()
        .map(|entry| {
            let cell = entry.cell();
            manifest
                .rows
                .iter()
                .find(|row| row.cell == cell)
                .map(|row| row.count)
                .ok_or_else(|| "missing pilot row".into())
        })
        .collect()
}

fn rounded(value: u64, window: u64) -> Result<u64, String> {
    value
        .checked_add(window - 1)
        .map(|value| value / window * window)
        .ok_or_else(|| "pilot count overflow".into())
}

fn scaled(count: u64, elapsed_ns: u64, target_ns: u64, window: u64) -> Result<u64, String> {
    let target = u128::from(count)
        .checked_mul(u128::from(target_ns))
        .ok_or("pilot scale overflow")?
        .div_ceil(u128::from(elapsed_ns));
    let target = u64::try_from(target).map_err(|_| "pilot count overflow")?;
    rounded(target, window)
        .and_then(|value| Ok(value.max(window.checked_mul(32).ok_or("pilot count overflow")?)))
}

fn validate(row: &Row, warmup: u64) -> Result<(), String> {
    let window = row.cell.capacity.min(row.cell.in_flight);
    let minimum = window.checked_mul(32).ok_or("pilot count overflow")?;
    if window == 0
        || row.count < minimum
        || !row.count.is_multiple_of(window)
        || row
            .count
            .checked_add(warmup)
            .is_none_or(|last| last == u64::MAX)
    {
        return Err("invalid pilot count".into());
    }
    if !(1..=8).contains(&row.observations.len()) {
        return Err("invalid pilot ramp".into());
    }
    let mut count = rounded(64_u64.max(minimum), window)?;
    for (index, value) in row.observations.iter().enumerate() {
        if value.count != count || value.elapsed_ns == 0 {
            return Err("invalid pilot ramp".into());
        }
        if index + 1 < row.observations.len() {
            if value.elapsed_ns >= 50_000_000 {
                return Err("continued measurable pilot ramp".into());
            }
            count = scaled(count, value.elapsed_ns, 50_000_000, window)?;
        } else if value.elapsed_ns < 50_000_000 {
            return Err("pilot ramp remained unmeasurable".into());
        }
    }
    let last = row.observations.last().unwrap();
    (row.count == scaled(last.count, last.elapsed_ns, 500_000_000, window)?)
        .then_some(())
        .ok_or_else(|| "invalid frozen pilot count".into())
}

pub fn calibrate(
    cell: Cell,
    warmup: u64,
    mut run: impl FnMut(u64) -> Result<u64, String>,
) -> Result<Row, String> {
    let window = cell.capacity.min(cell.in_flight);
    if window == 0 {
        return Err("zero pilot window".into());
    }
    let mut count = rounded(
        64_u64.max(window.checked_mul(32).ok_or("pilot count overflow")?),
        window,
    )?;
    let mut observations = Vec::new();
    for _ in 0..8 {
        let elapsed_ns = run(count)?;
        if elapsed_ns == 0 {
            return Err("zero pilot duration".into());
        }
        observations.push(Observation { count, elapsed_ns });
        if elapsed_ns >= 50_000_000 {
            count = scaled(count, elapsed_ns, 500_000_000, window)?;
            if count
                .checked_add(warmup)
                .is_some_and(|last| last < u64::MAX)
            {
                return Ok(Row {
                    cell,
                    count,
                    observations,
                });
            }
            return Err("pilot operation identity overflow".into());
        }
        count = scaled(count, elapsed_ns, 50_000_000, window)?;
    }
    Err("pilot never became measurable".into())
}
