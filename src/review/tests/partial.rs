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

#[test]
fn blocked_partial_review_cannot_repair_changed_source_queued_edits_or_exhausted_budget() {
    for cause in ["source", "queue", "budget"] {
        let (_data, mut store, mut m) = partial_fixture();
        match cause {
            "source" => fs::write(
                m.execution
                    .as_ref()
                    .unwrap()
                    .workspace
                    .join("work/greet.sh"),
                "changed\n",
            )
            .unwrap(),
            "queue" => {
                store.enqueue(m.id, "Change the greeting").unwrap();
            }
            _ => {
                store
                    .0
                    .execute("UPDATE reviews SET rounds=2 WHERE mod_id=?1", [m.id])
                    .unwrap();
            }
        }
        m.agent_review = store.review_state(m.id).unwrap();
        assert!(!store.review_fixes(&m).unwrap(), "{cause}");
        assert!(
            store.execution(m.id).unwrap().unwrap().complete(),
            "{cause}"
        );
        assert!(
            !store.review_state(m.id).unwrap().unwrap().clear(&m),
            "{cause}"
        );
    }
}

#[test]
fn blocked_review_without_findings_keeps_reason_and_open_ledger_without_enabling_repairs() {
    let (_data, mut store, mut m) = partial_fixture();
    let source = start(&mut store, &mut m);
    let mut blocked = report();
    blocked["findings"] = json!([]);
    blocked["summary"] = json!("Required source inspection is unavailable.");
    store
        .finish_review(&m, &source, &blocked.to_string())
        .unwrap();
    let state = store.review_state(m.id).unwrap().unwrap();
    assert_eq!(
        state.report.as_ref().unwrap().summary,
        "Required source inspection is unavailable."
    );
    assert_eq!(state.issues[0].status, "confirmed");
    assert!(!state.repairable() && !state.clear(&m));
    assert!(!store.review_fixes(&m).unwrap());
}

#[test]
fn blocked_partial_review_cannot_bypass_no_progress_after_a_repair_pass() {
    let (_data, mut store, mut m) = partial_fixture();
    assert!(store.review_fixes(&m).unwrap());
    let execution = store.execution(m.id).unwrap().unwrap();
    let plan = store.planning(m.id).unwrap().unwrap().plan.unwrap();
    let mut checks = Vec::new();
    for (run, task) in execution.tasks.iter().zip(&plan.tasks) {
        let results: Vec<_> = task
            .checks
            .iter()
            .map(|check| CheckResult {
                duration_ms: None,
                timeout_seconds: None,
                missing_runtime: None,
                task: Some(run.id),
                check: check.clone(),
                command: vec!["/bin/true".into()],
                exit_code: Some(0),
                output: "Passed".into(),
            })
            .collect();
        store
            .finish_task(m.id, &run.source, "done", "Checks pass", &results)
            .unwrap();
        checks.extend(results);
    }
    let fingerprint = workspace::review(&execution.workspace).unwrap().fingerprint;
    store
        .execution_checks(m.id, "review", &checks, Some(&fingerprint))
        .unwrap();
    m.planning = store.planning(m.id).unwrap();
    m.execution = store.execution(m.id).unwrap();
    let source = start(&mut store, &mut m);
    store
        .finish_review(&m, &source, &report().to_string())
        .unwrap();
    let state = store.review_state(m.id).unwrap().unwrap();
    assert_eq!(state.status, "blocked");
    assert_eq!(state.rounds, 1);
    assert!(
        state
            .halt
            .as_deref()
            .is_some_and(|reason| reason.starts_with("No finding was resolved"))
    );
    assert!(!state.repairable() && !state.clear(&m));
    assert!(!store.review_fixes(&m).unwrap());
}

#[test]
fn blocked_partial_review_repairs_only_findings_confirmed_on_its_version() {
    let (_data, mut store, mut m) = partial_fixture();
    let execution = m.execution.as_ref().unwrap();
    fs::write(
        execution.workspace.join("work/greet.sh"),
        "#!/bin/sh\n[ -n \"${1-}\" ] || exit 1\nprintf 'hello %s\\n' \"$1\"\n",
    )
    .unwrap();
    let fingerprint = workspace::review(&execution.workspace).unwrap().fingerprint;
    store
        .execution_checks(m.id, "review", &execution.checks, Some(&fingerprint))
        .unwrap();
    m.execution = store.execution(m.id).unwrap();
    let source = start(&mut store, &mut m);
    let mut partial = report();
    partial["findings"][0]["owner"] = json!("docs");
    partial["findings"][0]["file"] = json!("README.md");
    partial["findings"][0]["title"] = json!("Describe the missing-input error");
    store
        .finish_review(&m, &source, &partial.to_string())
        .unwrap();
    m.agent_review = store.review_state(m.id).unwrap();
    assert!(store.review_fixes(&m).unwrap());
    let execution = store.execution(m.id).unwrap().unwrap();
    assert_eq!(execution.tasks[0].status, "done");
    assert_eq!(execution.tasks[1].status, "pending");
    let issues = store.review_state(m.id).unwrap().unwrap().issues;
    assert_eq!(issues[0].status, "confirmed");
    assert_eq!(issues[0].attempts, 0);
    assert_eq!(issues[1].status, "fixing");
}

#[test]
fn blocked_partial_review_invalid_ledger_cannot_authorize_old_findings() {
    let (_data, mut store, mut m) = partial_fixture();
    let source = start(&mut store, &mut m);
    let mut partial = report();
    partial["findings"][0]["id"] = json!(999);
    store
        .finish_review(&m, &source, &partial.to_string())
        .unwrap();
    let state = store.review_state(m.id).unwrap().unwrap();
    assert_eq!(state.status, "blocked");
    assert_eq!(
        state.report.as_ref().unwrap().summary,
        "Review referenced an unknown finding ID."
    );
    assert!(!state.repairable());
    assert!(!store.review_fixes(&m).unwrap());
    assert!(store.execution(m.id).unwrap().unwrap().complete());
}
