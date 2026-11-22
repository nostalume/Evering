//! The bytes an example defines for itself, checked where they need no peer: the bootstrap a coordinator
//! hands a worker across an `exec`, the job and outcome payloads that ride the channel, and the streaming
//! walker that produces an outcome. Each is reached through the source file the example itself compiles, so
//! the copy under test and the copy shipped cannot drift.
//!
//! The legs that cross a process are not here. A spawning leg names its child's role as an option on
//! `current_exe()`, and the test harness refuses an unknown option before any role body could answer for it,
//! so the child has to be the example itself — which is what the CI rows exec. What the identity bytes below
//! *mean* is `portable_contracts`' claim; what is claimed here is the framing around them.

#[allow(dead_code)]
#[path = "../examples/index.rs"]
mod index;

use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use evering::{ChannelId, Encoded, PoolId, Port, layout::RegionId, process::Bootstrap};

const REGION: RegionId = RegionId::new(0x4556_4552_4945_3031, 1);
const WORKER: usize = 3;

fn port(channel: u32, role: u8) -> Port<Encoded> {
    Port::from_parts(ChannelId::new(REGION, channel, 2, 3, 4), role, 41).unwrap()
}

fn boot() -> Bootstrap {
    index::encode_boot(
        WORKER,
        PoolId::new(REGION, 13, 17, 19),
        &port(23, 0),
        &port(29, 1),
    )
}

#[test]
fn indexes_files_with_bounded_streaming_state() {
    let root = std::env::temp_dir().join(format!(
        "evering-index-{}-{:?}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let text = root.join("text.txt");
    fs::write(&text, b"alpha\nbeta\nlast").unwrap();

    let indexed = index::inspect(&text);
    assert_eq!((indexed.bytes, indexed.lines), (15, 3));
    assert_eq!(indexed.checksum, 0x458b_7a7b_21c1_b15d);
    assert!(indexed.error.is_none());

    let missing = index::inspect(&root.join("missing"));
    assert!(missing.error.as_deref().unwrap().contains("missing"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn bootstrap_frames_the_identities_a_worker_is_named_by() {
    let pool = PoolId::new(REGION, 13, 17, 19);
    let (jobs, results) = (port(23, 0), port(29, 1));
    let boot = index::encode_boot(WORKER, pool, &jobs, &results);
    assert_eq!(
        boot.as_ref().len(),
        24 + PoolId::BYTE_LEN + 2 * Port::<Encoded>::BYTE_LEN
    );

    let (opened, opened_jobs, opened_results) = index::decode_boot(&boot, WORKER).unwrap();
    assert_eq!(opened, pool);
    for (got, want) in [(&opened_jobs, &jobs), (&opened_results, &results)] {
        assert_eq!(got.id(), want.id());
        assert_eq!(
            (got.role(), got.generation()),
            (want.role(), want.generation())
        );
    }
}

#[test]
fn bootstrap_promises_the_worker_it_was_addressed_to() {
    let frame = boot().as_ref().to_vec();
    let refused = |bytes: Vec<u8>, worker: usize| {
        index::decode_boot(&Bootstrap::new(bytes).unwrap(), worker).is_err()
    };
    // Every case below mutates this frame, so the unmutated one is asserted accepted first: otherwise a
    // wrong offset in the test itself would look exactly like the identity check working.
    assert!(!refused(frame.clone(), WORKER));

    // The magic owns the framing version, so a peer that disagrees about it says so before it reads a part.
    let mut other_version = frame.clone();
    let magic = u64::from_le_bytes(frame[..8].try_into().unwrap());
    other_version[..8].copy_from_slice(&(magic + 1).to_le_bytes());
    assert!(refused(other_version, WORKER));

    // The slot is not decoration: a frame addressed to this worker must not answer for another.
    assert!(refused(frame.clone(), WORKER + 1));

    // The extent is checked, not trusted, because a wrong size admits a mapping of the wrong bytes.
    let mut other_extent = frame.clone();
    other_extent[16] ^= 1;
    assert!(refused(other_extent, WORKER));

    // One byte either side of the two ports the frame promises is a framing failure, not a short read.
    let mut short = frame.clone();
    short.pop();
    assert!(refused(short, WORKER));
    let mut long = frame.clone();
    long.push(0);
    assert!(refused(long, WORKER));
}

#[test]
fn job_bytes_number_the_file_then_name_it() {
    let body = index::job(7, Path::new("src/lib.rs")).unwrap();
    assert_eq!(&body[..8], &7u64.to_le_bytes());
    assert_eq!(&body[8..], b"src/lib.rs");
    assert_eq!(
        index::read_job(&body).unwrap(),
        (7, PathBuf::from("src/lib.rs"))
    );

    assert_eq!(index::read_job(&[]).err().as_deref(), Some("short job"));
    let mut not_utf8 = 1u64.to_le_bytes().to_vec();
    not_utf8.push(0xff);
    assert!(index::read_job(&not_utf8).is_err());
}

#[test]
fn outcome_bytes_carry_the_index_or_its_failure() {
    let done = index::Index {
        bytes: 15,
        lines: 3,
        checksum: 0x458b_7a7b_21c1_b15d,
        error: None,
    };
    let body = index::encode_outcome(9, &done);
    assert_eq!(body.len(), 33);
    assert_eq!(&body[..8], &9u64.to_le_bytes());
    assert_eq!(body[32], 0);
    assert_eq!(index::read_outcome(&body).unwrap(), (9, done));

    let failed = index::Index {
        error: Some("no such file".into()),
        ..index::Index::default()
    };
    let body = index::encode_outcome(2, &failed);
    assert_eq!(body[32], 1);
    assert_eq!(&body[33..], b"no such file");
    assert_eq!(index::read_outcome(&body).unwrap(), (2, failed));

    assert_eq!(
        index::read_outcome(&body[..32]).err().as_deref(),
        Some("short outcome")
    );
    let mut not_utf8 = vec![0; 32];
    not_utf8.push(1);
    not_utf8.push(0xff);
    assert!(index::read_outcome(&not_utf8).is_err());
}
