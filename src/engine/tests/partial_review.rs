use super::*;

#[test]
fn blocked_partial_review_native_status_and_actions_preserve_reason_and_findings() {
    let (data, store, m) = crate::review::tests::partial::partial_fixture();
    let mut engine = Engine::load(data.0.join("project"), store).unwrap();
    engine
        .command(Command::SetDraft {
            mod_id: m.id,
            text: "Keep this draft".into(),
        })
        .unwrap();
    let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
    assert_eq!(snapshot["status"], "Review incomplete · 1 known issue");
    assert_eq!(
        snapshot["error"],
        crate::review::tests::partial::report()["summary"]
    );
    assert_eq!(
        snapshot["active_mod"]["findings"].as_array().unwrap().len(),
        1
    );
    let actions = snapshot["actions"].as_array().unwrap();
    assert_eq!(actions[0]["id"], "fix_issues");
    assert!(actions.iter().any(|action| action["id"] == "fix_issues"));
    assert!(!actions.iter().any(|action| action["id"] == "publish"));
    engine
        .command(Command::Action {
            mod_id: Some(m.id),
            action: "fix_issues".into(),
            task_id: None,
        })
        .unwrap();
    assert_eq!(
        engine.mods[0].agent_review.as_ref().unwrap().status,
        "fixing"
    );
    assert_eq!(engine.mods[0].draft, "Keep this draft");
    assert!(!engine.can_publish());
}

#[test]
fn blocked_partial_review_inspection_title_keeps_partial_caption_and_reason() {
    let (data, store, m) = crate::review::tests::partial::partial_fixture();
    let mut engine = Engine::load(data.0.join("project"), store).unwrap();
    engine
        .command(Command::Action {
            mod_id: Some(m.id),
            action: "findings".into(),
            task_id: None,
        })
        .unwrap();
    let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
    assert_eq!(
        snapshot["dialog"]["title"],
        "Review incomplete · 1 known issue"
    );
    assert_eq!(
        snapshot["dialog"]["body"],
        crate::review::tests::partial::report()["summary"]
    );
}
