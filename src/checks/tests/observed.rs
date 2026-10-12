use super::*;

pub(crate) fn save_observed_fixture(root: &Path, source: &str) -> workspace::Snapshot {
    let id = task_worktree::task_id(source).unwrap();
    fs::write(root.join("work/a.txt"), "saved work").unwrap();
    let snapshot = workspace::source_state(&root.join("work")).unwrap();
    fs::create_dir_all(root.join("task-drafts")).unwrap();
    fs::write(
        root.join(format!("task-drafts/{id}.json")),
        serde_json::to_vec(&snapshot).unwrap(),
    )
    .unwrap();
    let scripts = vec![Script {
        name: "check.sh".into(),
        content: "exit 1".into(),
    }];
    let results = [0, 0, 1]
        .into_iter()
        .enumerate()
        .map(|(index, exit)| CheckResult {
            task: None,
            check: "A exists".into(),
            command: vec!["/bin/sh".into(), "-c".into(), format!("exit {exit}")],
            exit_code: Some(exit),
            output: format!("Observed command {index}: exit {exit}"),
            timeout_seconds: Some(30),
            duration_ms: Some(12),
            missing_runtime: None,
        })
        .collect();
    let receipt = Receipt {
        source: source.into(),
        fingerprint: workspace::fingerprint(&snapshot).unwrap(),
        scripts_fingerprint: workspace::fingerprint(&script_snapshot(&scripts)).unwrap(),
        passed: false,
        results,
    };
    save(
        root,
        id,
        &Bundle {
            scripts,
            receipt: Some(receipt),
        },
    )
    .unwrap();
    snapshot
}
