use super::*;

#[test]
#[ignore = "replays saved preparation snapshots from SPROWT_SNAPSHOT_REPLAY_DIR"]
fn saved_preparation_snapshots_ignore_only_generated_metadata() {
    let root = PathBuf::from(
        std::env::var_os("SPROWT_SNAPSHOT_REPLAY_DIR").expect("SPROWT_SNAPSHOT_REPLAY_DIR"),
    );
    let before: Snapshot =
        serde_json::from_slice(&fs::read(root.join("before.json")).unwrap()).unwrap();
    let mut after: Snapshot =
        serde_json::from_slice(&fs::read(root.join("after.json")).unwrap()).unwrap();
    let baseline = source_snapshot(&root, before.clone()).unwrap();
    assert_eq!(source_snapshot(&root, after.clone()).unwrap(), baseline);
    assert_eq!(
        baseline
            .iter()
            .find(|f| f.0 == Path::new(".pytest_cache/v/cache/nodeids"))
            .expect("retained test cache"),
        before
            .iter()
            .find(|f| f.0 == Path::new(".pytest_cache/v/cache/nodeids"))
            .expect("starting test cache")
    );
    let file = after
        .iter_mut()
        .find(|f| source_path(&root, &f.0) && !generated_metadata(&f.0))
        .expect("real source file");
    file.1.extend_from_slice(b"unexpected setup edit");
    assert_ne!(source_snapshot(&root, after).unwrap(), baseline);
}
