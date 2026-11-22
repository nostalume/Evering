//! What each stage is allowed to decide: screening holds estimates but no authority,
//! smoke is descriptive only, a report is invariant under input order, and publication
//! is complete, digest-bound and never overwrites — including the source it rejects
//! before decoding it.

use crate::analysis;
use crate::tests::evidence_attribution::{record_system_evidence, system_evidence};

#[test]
fn screening_has_estimates_but_no_decision_authority() {
    let evidence = system_evidence("screening", 1.1);
    let analysis = analysis::analyze(&evidence).unwrap();
    assert!(!analysis.estimates.is_empty());
    assert_eq!(analysis.authority, analysis::Authority::Descriptive);
}

#[test]
fn smoke_is_admitted_only_as_descriptive_evidence() {
    let evidence = system_evidence("smoke", 1.2);
    let analysis = analysis::analyze(&evidence).unwrap();
    assert!(!analysis.estimates.is_empty());
    assert_eq!(analysis.authority, analysis::Authority::Descriptive);
}

#[test]
fn report_is_input_order_invariant_and_binds_exact_source_bytes() {
    let root = std::env::temp_dir().join(format!("evering-report-{}", fastrand::u64(..)));
    std::fs::create_dir(&root).unwrap();
    let first = root.join("first.jsonl");
    let second = root.join("second.jsonl");
    record_system_evidence(&first, "focused", 1.2, "first");
    record_system_evidence(&second, "focused", 1.1, "second");
    let paths = [first, second].map(|path| path.to_string_lossy().into_owned());

    let forward = analysis::command(&paths).unwrap();
    let reverse = analysis::command(&[paths[1].clone(), paths[0].clone()]).unwrap();
    assert_eq!(forward, reverse);
    for path in &paths {
        let digest = blake3::hash(&std::fs::read(path).unwrap()).to_hex();
        assert!(forward.contains(digest.as_str()));
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(feature = "plot")]
fn publication_is_complete_digest_bound_and_non_overwriting() {
    use crate::fixture;

    let root = std::env::temp_dir().join(format!("evering-publish-{}", fastrand::u64(..)));
    std::fs::create_dir(&root).unwrap();
    let system = root.join("system.jsonl");
    record_system_evidence(&system, "focused", 1.2, "publish");
    let specification = root.join("mechanism.json");
    let mechanism = root.join("mechanism.jsonl");
    std::fs::write(
        &specification,
        serde_json::to_vec(&serde_json::json!({
            "fixture": fixture::KEYS[0],
            "targets": ["a".repeat(64)],
            "parameters": { "extent": 1 << 20, "storage": 1 << 16, "capacity": 8 },
            "cases": [{ "payload": 0 }],
            "policy": { "min_ns": 1, "max_ns": u64::MAX, "pairs": 1, "calibration_attempts": 1 },
            "seed": 7,
            "budget_ms": 5_000,
            "alpha": 0.05,
            "delta_ns": 1.0,
            "system_delta": 0.05,
        }))
        .unwrap(),
    )
    .unwrap();
    fixture::record(&specification, &mechanism).unwrap();
    let sources = [&system, &mechanism].map(|path| path.to_string_lossy().into_owned());
    let output = root.join("published");

    let files = crate::plot::command(&output, &sources).unwrap();
    assert_eq!(files.len(), 4);
    assert!(files.iter().any(|path| path.ends_with("report.md")));
    assert!(files.iter().any(|path| path.ends_with("DIGESTS.blake3")));
    let svgs = files
        .iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "svg"))
        .map(|path| std::fs::read_to_string(path).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(svgs.len(), 2);
    assert!(svgs.iter().any(|svg| svg.contains("practical band")));
    assert!(svgs.iter().any(|svg| svg.contains("paired difference")));
    let manifest = std::fs::read_to_string(output.join("DIGESTS.blake3")).unwrap();
    for path in files
        .iter()
        .filter(|path| !path.ends_with("DIGESTS.blake3"))
    {
        let digest = blake3::hash(&std::fs::read(path).unwrap()).to_hex();
        assert!(manifest.contains(&format!(
            "{}  {}",
            digest,
            path.file_name().unwrap().to_string_lossy()
        )));
    }
    let repeated = crate::plot::command(&root.join("repeated"), &sources).unwrap();
    assert!(files.iter().zip(&repeated).all(|(left, right)| {
        left.file_name() == right.file_name()
            && std::fs::read(left).unwrap() == std::fs::read(right).unwrap()
    }));
    let before = files
        .iter()
        .map(|path| (path.clone(), std::fs::read(path).unwrap()))
        .collect::<Vec<_>>();
    assert!(crate::plot::command(&output, &sources).is_err());
    assert!(
        before
            .iter()
            .all(|(path, bytes)| std::fs::read(path).unwrap() == *bytes)
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(feature = "plot")]
fn publication_rejects_source_over_the_retention_limit_before_decode() {
    let root = std::env::temp_dir().join(format!("evering-retention-{}", fastrand::u64(..)));
    std::fs::create_dir(&root).unwrap();
    let oversized = root.join("oversized.jsonl");
    std::fs::File::create(&oversized)
        .unwrap()
        .set_len(5 * 1024 * 1024 + 1)
        .unwrap();
    let error = crate::plot::command(
        &root.join("published"),
        &[oversized.to_string_lossy().into_owned()],
    )
    .unwrap_err();
    assert!(error.contains("retention limit"));
    std::fs::remove_dir_all(root).unwrap();
}
