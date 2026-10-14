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
        serde_json::json!(["conversation", "changes", "checks", "findings", "agents"])
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
