#![cfg(all(feature = "os", feature = "tokio"))]

#[allow(dead_code)]
#[path = "../examples/index.rs"]
mod index;

use std::{fs, time::SystemTime};

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
