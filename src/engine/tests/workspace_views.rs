use super::*;

#[test]
fn conversation_and_saved_history_partition_by_message_identity() {
    let (_data, mut engine, _, _) = session();
    let message = crate::store::Message {
        source: None,
        task: None,
        item_id: None,
        role: "user".into(),
        body: "Repeat this request".into(),
        model: None,
        effort: None,
    };
    let m = &mut engine.mods[engine.active.unwrap()];
    m.messages = vec![
        message.clone(),
        message.clone(),
        crate::store::Message {
            role: "codex:4".into(),
            body: "Saved executor details".into(),
            ..message
        },
    ];
    let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
    assert_eq!(
        snapshot["workspace_views"],
        serde_json::json!(["conversation"])
    );
    let current = snapshot["active_mod"]["messages"].as_array().unwrap();
    let saved = snapshot["active_mod"]["history"].as_array().unwrap();
    assert_eq!(
        current
            .iter()
            .map(|m| m["body"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["Repeat this request", "Repeat this request"]
    );
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0]["body"], "Saved executor details");
    assert_eq!(saved[0]["author"], "codex · executor · w4");
}

#[test]
fn empty_codemod_exposes_only_conversation() {
    let (_data, engine, _, _) = session();
    let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
    assert_eq!(
        snapshot["workspace_views"],
        serde_json::json!(["conversation"])
    );
}

#[test]
fn saved_source_checks_findings_and_workers_determine_available_tabs() {
    let (data, store, _, _) = crate::review::tests::fixture();
    let mut engine = Engine::load(data.0.join("project"), store).unwrap();
    let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
    assert_eq!(
        snapshot["workspace_views"],
        serde_json::json!(["conversation", "changes", "checks"])
    );
    engine.mods[0].agent_review = Some(crate::review::State {
        status: "findings".into(),
        report: Some(serde_json::from_str(&crate::review::tests::finding()).unwrap()),
        ..Default::default()
    });
    let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
    assert_eq!(
        snapshot["workspace_views"],
        serde_json::json!(["conversation", "changes", "checks", "findings"])
    );
    assert_eq!(
        snapshot["active_mod"]["findings"][0]["title"],
        "Reject empty names"
    );
    engine.mods[0].agent_review = None;
    let execution = engine.mods[0].execution.as_mut().unwrap();
    std::fs::write(execution.workspace.join("review.patch"), "").unwrap();
    execution.checks.clear();
    for task in &mut execution.tasks {
        task.checks.clear();
    }
    assert_eq!(
        serde_json::to_value(engine.snapshot()).unwrap()["workspace_views"],
        serde_json::json!(["conversation"])
    );
    let record = engine.store.worker(engine.mods[0].id).unwrap();
    let mut worker = Worker::start(
        &engine.project,
        &engine.mods[0],
        record,
        Role::Planner,
        None,
    )
    .unwrap();
    worker.status = Status::Failed;
    worker.enabled = false;
    worker.error = Some("Provider unavailable".into());
    engine.workers.insert(worker.id, worker);
    let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
    assert_eq!(
        snapshot["workspace_views"],
        serde_json::json!(["conversation", "agents"])
    );
    assert_eq!(
        snapshot["active_mod"]["worker_details"][0]["activity"],
        "Provider unavailable"
    );
}
