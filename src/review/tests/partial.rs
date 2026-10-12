use super::*;

#[test]
fn incomplete_blocked_review_keeps_reason_after_restart_and_blocks_publication() {
    let (data, mut store, mut m, _plan) = fixture();
    let source = start(&mut store, &mut m);
    let reason = "Browser socket restrictions prevented the remaining inspection.";
    let report = json!({
        "complete": false,
        "status": "blocked",
        "summary": reason,
        "findings": [],
        "assessments": []
    });
    store
        .finish_review(&m, &source, &report.to_string())
        .unwrap();
    drop(store);
    let mut store = data.store();
    let state = store.review_state(m.id).unwrap().unwrap();
    assert_eq!(state.status, "blocked");
    assert_eq!(state.report.as_ref().unwrap().summary, reason);
    assert!(!state.report.as_ref().unwrap().complete);
    assert!(state.current(&m));
    assert!(!state.clear(&m));
    assert!(!store.review_fixes(&m).unwrap());
}

#[test]
fn incomplete_reviews_cannot_claim_clean_or_actionable_findings() {
    for text in [
        json!({"status":"clean", "summary":"Inspection stopped before checking errors.", "findings":[]}).to_string(),
        finding(),
    ] {
        let (_data, mut store, mut m, plan) = fixture();
        let source = start(&mut store, &mut m);
        let mut report: Value = serde_json::from_str(&text).unwrap();
        report["complete"] = json!(false);
        assert!(Report::parse(&report.to_string(), &plan).is_err());
        store.finish_review(&m, &source, &report.to_string()).unwrap();
        let state = store.review_state(m.id).unwrap().unwrap();
        assert_eq!(state.status, "blocked");
        assert!(state.current(&m));
        assert!(!state.clear(&m));
        assert!(!store.review_fixes(&m).unwrap());
    }
}

pub(crate) fn report() -> Value {
    let mut report: Value = serde_json::from_str(&finding()).unwrap();
    report["complete"] = json!(false);
    report["status"] = json!("blocked");
    report["summary"] = json!(
        "Source inspection found a confirmed defect; required follow-up inspection could not finish."
    );
    report
}

pub(crate) fn partial_fixture() -> (TestData, Store, CodeMod) {
    let (data, mut store, mut m, _) = fixture();
    let source = start(&mut store, &mut m);
    store
        .finish_review(&m, &source, &report().to_string())
        .unwrap();
    m.agent_review = store.review_state(m.id).unwrap();
    (data, store, m)
}

#[test]
fn blocked_partial_review_keeps_findings_reason_and_scoped_repairs_after_restart() {
    let (data, store, m) = partial_fixture();
    drop(store);
    let mut store = data.store();
    let state = store.review_state(m.id).unwrap().unwrap();
    assert_eq!(state.status, "blocked");
    assert_eq!(
        state.report.as_ref().unwrap().summary,
        report()["summary"].as_str().unwrap()
    );
    assert!(!state.report.as_ref().unwrap().complete);
    assert_eq!(state.issues.len(), 1);
    assert_eq!(state.issues[0].status, "confirmed");
    assert!(state.repairable() && !state.clear(&m));
    assert!(store.review_fixes(&m).unwrap());
    let execution = store.execution(m.id).unwrap().unwrap();
    assert!(execution.tasks.iter().all(|run| run.status == "pending"));
    assert!(
        execution.tasks[0]
            .review_feedback
            .as_ref()
            .unwrap()
            .contains("Reject empty names")
    );
    assert_eq!(store.review_state(m.id).unwrap().unwrap().status, "fixing");
    assert!(
        !store
            .review_state(m.id)
            .unwrap()
            .unwrap()
            .report
            .as_ref()
            .unwrap()
            .complete
    );
}

#[test]
fn blocked_partial_review_adds_known_findings_without_closing_omitted_issues() {
    let (_data, mut store, mut m, _) = fixture();
    let first = start(&mut store, &mut m);
    store.finish_review(&m, &first, &finding()).unwrap();
    m.agent_review = store.review_state(m.id).unwrap();
    let source = start(&mut store, &mut m);
    let mut partial = report();
    partial["findings"][0]["owner"] = json!("docs");
    partial["findings"][0]["file"] = json!("README.md");
    partial["findings"][0]["title"] = json!("Describe the missing-input error");
    store
        .finish_review(&m, &source, &partial.to_string())
        .unwrap();
    let state = store.review_state(m.id).unwrap().unwrap();
    assert_eq!(state.issues.len(), 2);
    assert!(state.issues.iter().all(|issue| issue.status == "confirmed"));
    assert_eq!(state.issues[0].finding.title, "Reject empty names");
    assert_eq!(
        state.issues[1].finding.title,
        "Describe the missing-input error"
    );
    assert_eq!(
        state.report.as_ref().unwrap().summary,
        partial["summary"].as_str().unwrap()
    );
    assert!(!state.clear(&m));
}
