use super::*;

#[test]
fn observed_task_check_receipt_reaches_native_evidence_without_finishing_task() {
    let (data, mut store, m, root, _) = crate::network::tests::fixture();
    let run = &m.execution.as_ref().unwrap().tasks[0];
    crate::checks::tests::save_observed_fixture(&root, &run.source);
    store
        .finish_task(
            m.id,
            &run.source,
            "blocked",
            "Dependency is still blocked",
            &[],
        )
        .unwrap();
    let engine = Engine::load(data.0.join("project"), store).unwrap();
    let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
    let task = &snapshot["active_mod"]["plan"]["tasks"][0];
    assert_eq!(task["checks_label"], "Observed task checks");
    assert_eq!(task["completed"], false);
    assert_eq!(task["error"], "Dependency is still blocked");
    assert_eq!(
        task["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|result| result["status"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["passed", "passed", "failed"]
    );
    assert!(
        task["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|result| result["kind"] == "observed_check")
    );
    assert_eq!(snapshot["active_mod"]["checks"], task["results"]);
    assert_eq!(
        task["results"][2]["output"],
        "/bin/sh -c exit 1\n\nObserved command 2: exit 1"
    );
    let execution = engine.store.execution(m.id).unwrap().unwrap();
    assert_eq!(execution.tasks[0].status, "blocked");
    assert!(execution.tasks[0].checks.is_empty() && execution.checks.is_empty());
    assert!(!engine.version_ready() && !engine.can_publish());
}

#[test]
fn observed_task_check_receipt_rejects_replaced_attempt_scripts_and_changed_draft() {
    for change in ["attempt", "scripts", "draft"] {
        let (data, mut store, m, root, _) = crate::network::tests::fixture();
        let run = &m.execution.as_ref().unwrap().tasks[0];
        let mut draft = crate::checks::tests::save_observed_fixture(&root, &run.source);
        store
            .finish_task(m.id, &run.source, "blocked", "Still blocked", &[])
            .unwrap();
        if change == "draft" {
            draft[0].1.extend_from_slice(b"unverified edit");
            std::fs::write(
                root.join(format!("task-drafts/{}.json", run.id)),
                serde_json::to_vec(&draft).unwrap(),
            )
            .unwrap();
        } else {
            let path = root.join(format!("checks/{}.json", run.id));
            let mut bundle: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            if change == "attempt" {
                bundle["receipt"]["source"] =
                    serde_json::json!(crate::store::task_source(run.id, 2));
            } else {
                bundle["scripts"][0]["content"] = serde_json::json!("exit 0");
            }
            std::fs::write(path, serde_json::to_vec(&bundle).unwrap()).unwrap();
        }
        let engine = Engine::load(data.0.join("project"), store).unwrap();
        let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
        assert!(
            snapshot["active_mod"]["plan"]["tasks"][0]["results"]
                .as_array()
                .unwrap()
                .is_empty(),
            "{change}: {snapshot}"
        );
    }
}
