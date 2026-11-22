//! The record store as a contract: a header and its rows survive encode and decode,
//! and every way of breaking that — a truncated prefix, a duplicate row, a
//! post-terminal byte, a reordered measure — is refused rather than repaired.

use crate::study;
use crate::tests::mechanism_specs::{ExampleSchema, ExampleSpec, ForeignSchema};

fn example_header() -> study::Header<ExampleSpec, u64> {
    let schedule = [0, 1]
        .map(|order| study::Scheduled {
            unit: study::Unit { block: 0, order },
            case: order,
        })
        .into();
    study::Header::new::<ExampleSchema>(
        study::Context::default(),
        ExampleSpec,
        vec![11, 22],
        study::Run {
            schedule,
            ..study::Run::default()
        },
    )
}

fn example_artifact(path: &std::path::Path) -> String {
    let mut recorder = study::Recorder::<ExampleSchema>::create(path, example_header()).unwrap();
    for (order, measure) in [11, 22].into_iter().enumerate() {
        recorder
            .observe(study::Observation {
                unit: study::Unit {
                    block: 0,
                    order: order as u32,
                },
                case: order as u32,
                measure,
            })
            .unwrap();
    }
    recorder.complete().unwrap()
}

#[test]
fn typed_study_round_trips_and_schema_selects_the_decoder() {
    let root = std::env::temp_dir().join(format!("evering-typed-study-{}", fastrand::u64(..)));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("evidence.jsonl");
    let digest = example_artifact(&path);
    let loaded = study::load::<ExampleSchema>(&path).unwrap();

    assert_eq!(loaded.header, example_header());
    assert_eq!(loaded.observations.len(), 2);
    assert_eq!(loaded.complete.content, digest);
    assert_eq!(
        study::load::<ForeignSchema>(&path).err(),
        Some(study::Error::Schema)
    );
    let mut another_run = example_header();
    another_run.run.seed = 99;
    assert_eq!(
        study::study_id::<ExampleSchema>(&loaded.header),
        study::study_id::<ExampleSchema>(&another_run)
    );
    another_run.context.source.revision = "different source".into();
    assert_ne!(
        study::study_id::<ExampleSchema>(&loaded.header),
        study::study_id::<ExampleSchema>(&another_run)
    );
    assert_ne!(
        study::case_id::<ExampleSchema>(&11),
        study::case_id::<ExampleSchema>(&22)
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn typed_study_rejects_every_unsealed_prefix_and_abort() {
    let root = std::env::temp_dir().join(format!("evering-typed-cuts-{}", fastrand::u64(..)));
    std::fs::create_dir(&root).unwrap();
    let complete = root.join("complete.jsonl");
    example_artifact(&complete);
    let bytes = std::fs::read(&complete).unwrap();
    for cut in 0..bytes.len() {
        let path = root.join(format!("cut-{cut}"));
        std::fs::write(&path, &bytes[..cut]).unwrap();
        assert!(
            study::load::<ExampleSchema>(&path).is_err(),
            "accepted cut {cut}"
        );
    }
    let aborted = root.join("aborted.jsonl");
    let recorder = study::Recorder::<ExampleSchema>::create(&aborted, example_header()).unwrap();
    recorder.abort(None, "stopped\nnow").unwrap();
    assert_eq!(
        study::load::<ExampleSchema>(&aborted).err(),
        Some(study::Error::Aborted)
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn typed_study_rejects_corruption_duplicate_rows_and_post_terminal_data() {
    let root = std::env::temp_dir().join(format!("evering-typed-corrupt-{}", fastrand::u64(..)));
    std::fs::create_dir(&root).unwrap();
    let complete = root.join("complete.jsonl");
    example_artifact(&complete);
    let text = std::fs::read_to_string(&complete).unwrap();
    let lines: Vec<_> = text.lines().collect();

    let corrupt = root.join("corrupt.jsonl");
    std::fs::write(
        &corrupt,
        text.replacen("\"measure\":11", "\"measure\":12", 1),
    )
    .unwrap();
    assert!(study::load::<ExampleSchema>(&corrupt).is_err());

    let revision = root.join("revision.jsonl");
    std::fs::write(
        &revision,
        text.replacen("\"revision\":1", "\"revision\":2", 1),
    )
    .unwrap();
    assert_eq!(
        study::load::<ExampleSchema>(&revision).err(),
        Some(study::Error::Schema)
    );

    let duplicate = root.join("duplicate.jsonl");
    std::fs::write(
        &duplicate,
        format!("{}\n{}\n{}\n{}\n", lines[0], lines[1], lines[1], lines[3]),
    )
    .unwrap();
    assert!(study::load::<ExampleSchema>(&duplicate).is_err());

    let trailing = root.join("trailing.jsonl");
    std::fs::write(&trailing, format!("{text}{}\n", lines[1])).unwrap();
    assert_eq!(
        study::load::<ExampleSchema>(&trailing).err(),
        Some(study::Error::Syntax)
    );

    let aborted = root.join("aborted.jsonl");
    study::Recorder::<ExampleSchema>::create(&aborted, example_header())
        .unwrap()
        .abort(None, "stopped")
        .unwrap();
    let forged = root.join("forged-complete.jsonl");
    std::fs::write(
        &forged,
        std::fs::read_to_string(aborted)
            .unwrap()
            .replace("\"kind\":\"abort\"", "\"kind\":\"complete\""),
    )
    .unwrap();
    assert!(study::load::<ExampleSchema>(&forged).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn typed_study_rejects_reorder_invalid_measure_and_overwrite() {
    let root = std::env::temp_dir().join(format!("evering-typed-order-{}", fastrand::u64(..)));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("evidence.jsonl");
    let mut recorder = study::Recorder::<ExampleSchema>::create(&path, example_header()).unwrap();
    assert_eq!(
        recorder.observe(study::Observation {
            unit: study::Unit { block: 0, order: 1 },
            case: 1,
            measure: 22,
        }),
        Err(study::Error::Schedule)
    );
    assert_eq!(
        recorder.observe(study::Observation {
            unit: study::Unit { block: 0, order: 0 },
            case: 0,
            measure: 99,
        }),
        Err(study::Error::Invalid)
    );
    assert!(study::Recorder::<ExampleSchema>::create(&path, example_header()).is_err());
    recorder.abort(None, "test complete").unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
