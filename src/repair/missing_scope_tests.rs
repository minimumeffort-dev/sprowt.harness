use super::*;
use crate::{store::task_source, workspace};
use std::{fs, path::Path};

pub(crate) const MISSED_FILE: &str = "browser/integration.spec.mjs";

pub(crate) fn fixture() -> (
    crate::store::test_support::TestData,
    Store,
    i64,
    Plan,
    Vec<i64>,
    Vec<i64>,
    Repair,
) {
    let (data, store, mod_id, plan, ids, workers) = tests::fixture();
    let repair = Repair {
        task: "integration".into(),
        files: vec![MISSED_FILE.into()],
        check: "Retry works".into(),
        command: vec!["/usr/bin/node".into(), MISSED_FILE.into()],
        evidence: "browser/integration.spec.mjs expects version 1; the accepted contract exports version 2.".into(),
    };
    save_receipt(&store, mod_id, ids[2], &task_source(ids[2], 1), &repair);
    (data, store, mod_id, plan, ids, workers, repair)
}

fn save_receipt(store: &Store, mod_id: i64, id: i64, source: &str, repair: &Repair) {
    let root = store.execution(mod_id).unwrap().unwrap().workspace;
    fs::create_dir_all(root.join("checks")).unwrap();
    fs::create_dir_all(root.join("task-drafts")).unwrap();
    let base = vec![(
        Path::new(MISSED_FILE).to_owned(),
        b"expect(backup.version).toBe(1)".to_vec(),
        0o644,
    )];
    let mut draft = base.clone();
    draft.push((
        "tests/pdf.spec.mjs".into(),
        b"completed verification work".to_vec(),
        0o644,
    ));
    fs::write(
        root.join(format!("task-drafts/{id}-base.json")),
        serde_json::to_vec(&base).unwrap(),
    )
    .unwrap();
    fs::write(
        root.join(format!("task-drafts/{id}.json")),
        serde_json::to_vec(&draft).unwrap(),
    )
    .unwrap();
    let receipt = json!({"scripts":[],"receipt":{
        "source":source,"fingerprint":workspace::fingerprint(&draft).unwrap(),
        "scripts_fingerprint":workspace::fingerprint(&Vec::new()).unwrap(),"passed":false,
        "results":[{"check":repair.check,"command":repair.command,"exit_code":1,"output":repair.evidence}]
    }});
    fs::write(
        root.join(format!("checks/{id}.json")),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
}

#[test]
fn missing_assignment_resumes_only_reporter_and_preserves_completed_work() {
    let (data, mut store, mod_id, mut plan, ids, workers, repair) = fixture();
    let source = task_source(ids[2], 1);
    let root = store.execution(mod_id).unwrap().unwrap().workspace;
    let draft = fs::read(root.join(format!("task-drafts/{}.json", ids[2]))).unwrap();
    let peer_checks =
        json!([{"check":"Loads","command":["/bin/true"],"exit_code":0,"output":"passed"}]);
    store
        .0
        .execute(
            "UPDATE task_runs SET checks=?2,summary='Completed source preserved' WHERE id=?1",
            params![ids[0], peer_checks.to_string()],
        )
        .unwrap();
    let result = store.request_repair(mod_id, &source, &repair).unwrap();
    assert_eq!(
        result,
        Ok(()),
        "A verified missing assignment should resume without replanning"
    );
    assert_eq!(
        store.request_repair(mod_id, &source, &repair).unwrap(),
        Ok(())
    );
    drop(store);
    let mut store = data.store();
    assert!(store.resume_repairs(mod_id).unwrap());
    assert!(!store.resume_repairs(mod_id).unwrap());
    let execution = store.execution(mod_id).unwrap().unwrap();
    assert_eq!(
        execution
            .tasks
            .iter()
            .map(|r| r.status.as_str())
            .collect::<Vec<_>>(),
        ["done", "done", "pending", "pending"]
    );
    assert_eq!(execution.tasks[0].source, task_source(ids[0], 1));
    assert_eq!(execution.tasks[0].worker, Some(workers[0]));
    assert_eq!(execution.tasks[0].summary, "Completed source preserved");
    assert_eq!(
        serde_json::to_value(&execution.tasks[0].checks).unwrap(),
        peer_checks
    );
    assert_eq!(execution.tasks[2].source, task_source(ids[2], 2));
    assert_eq!(
        fs::read(root.join(format!("task-drafts/{}.json", ids[2]))).unwrap(),
        draft
    );
    plan.tasks[2].files.push(MISSED_FILE.into());
    let amended = store.planning(mod_id).unwrap().unwrap().plan.unwrap();
    assert_eq!(
        serde_json::to_value(&amended).unwrap(),
        serde_json::to_value(&plan).unwrap()
    );
    let retry = store
        .task_input(mod_id, workers[1], &amended)
        .unwrap()
        .unwrap();
    assert_eq!(retry.source, task_source(ids[2], 2));
    assert!(
        retry.texts[0].contains(MISSED_FILE) && retry.texts[0].contains("Preserve saved edits")
    );
    let count: i64 = store
        .0
        .query_row(
            "SELECT repair_count FROM executions WHERE mod_id=?1",
            [mod_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn missing_assignment_rejects_owned_broad_excluded_and_nonexistent_paths() {
    for files in [
        vec!["app/runtime.js"],
        vec!["tests/new.mjs"],
        vec!["app"],
        vec!["browser"],
        vec!["browser/new.mjs"],
        vec!["../integration.spec.mjs"],
        vec!["/tmp/integration.spec.mjs"],
        vec!["browser/./integration.spec.mjs"],
        vec!["browser//integration.spec.mjs"],
        vec!["browser/*.mjs"],
        vec![".env"],
        vec!["node_modules/check.mjs"],
        vec![".pytest_cache/check"],
        vec![MISSED_FILE, MISSED_FILE],
    ] {
        let (_data, mut store, mod_id, plan, ids, _, mut repair) = fixture();
        repair.files = files.iter().map(|file| (*file).into()).collect();
        assert!(
            store
                .request_repair(mod_id, &task_source(ids[2], 1), &repair)
                .unwrap()
                .is_err(),
            "{files:?}"
        );
        assert_unchanged(&store, mod_id, &plan, &ids);
    }
}

fn assert_unchanged(store: &Store, mod_id: i64, plan: &Plan, ids: &[i64]) {
    assert_eq!(
        serde_json::to_value(store.planning(mod_id).unwrap().unwrap().plan.unwrap()).unwrap(),
        serde_json::to_value(plan).unwrap()
    );
    let execution = store.execution(mod_id).unwrap().unwrap();
    assert_eq!(execution.tasks[2].status, "running");
    assert_eq!(execution.tasks[2].source, task_source(ids[2], 1));
    let count: i64 = store
        .0
        .query_row(
            "SELECT repair_count FROM executions WHERE mod_id=?1",
            [mod_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn missing_assignment_requires_current_failed_observed_command() {
    for field in [
        "missing",
        "source",
        "fingerprint",
        "scripts",
        "passed",
        "timeout",
        "runtime",
        "command",
        "check",
    ] {
        let (_data, mut store, mod_id, plan, ids, _, repair) = fixture();
        let root = store.execution(mod_id).unwrap().unwrap().workspace;
        let path = root.join(format!("checks/{}.json", ids[2]));
        let mut bundle: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        match field {
            "missing" => bundle["receipt"] = Value::Null,
            "source" => bundle["receipt"]["source"] = json!(task_source(ids[2], 2)),
            "fingerprint" => bundle["receipt"]["fingerprint"] = json!("stale"),
            "scripts" => bundle["scripts"] = json!([{"name":"new.sh","content":"exit 1"}]),
            "passed" => bundle["receipt"]["results"][0]["exit_code"] = json!(0),
            "timeout" => bundle["receipt"]["results"][0]["exit_code"] = Value::Null,
            "runtime" => {
                bundle["receipt"]["results"][0]["missing_runtime"] = json!("/usr/bin/node")
            }
            "command" => bundle["receipt"]["results"][0]["command"] = json!(["/bin/false"]),
            "check" => bundle["receipt"]["results"][0]["check"] = json!("Different check"),
            _ => unreachable!(),
        }
        fs::write(path, serde_json::to_vec(&bundle).unwrap()).unwrap();
        assert!(
            store
                .request_repair(mod_id, &task_source(ids[2], 1), &repair)
                .unwrap()
                .unwrap_err()
                .contains("fresh failed"),
            "{field}"
        );
        assert_unchanged(&store, mod_id, &plan, &ids);
    }
}

#[test]
fn missing_assignment_does_not_authorize_prior_out_of_scope_changes() {
    let (_data, mut store, mod_id, plan, ids, _, repair) = fixture();
    let root = store.execution(mod_id).unwrap().unwrap().workspace;
    let path = root.join(format!("task-drafts/{}-base.json", ids[2]));
    let mut base: workspace::Snapshot = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    base[0].1 = b"original unmodified test".to_vec();
    fs::write(path, serde_json::to_vec(&base).unwrap()).unwrap();
    assert!(
        store
            .request_repair(mod_id, &task_source(ids[2], 1), &repair)
            .unwrap()
            .unwrap_err()
            .contains("unchanged regular source file")
    );
    assert_unchanged(&store, mod_id, &plan, &ids);
}

#[test]
fn missing_assignment_rejects_nonregular_snapshot_entries() {
    let (_data, mut store, mod_id, plan, ids, _, repair) = fixture();
    let root = store.execution(mod_id).unwrap().unwrap().workspace;
    for suffix in ["-base", ""] {
        let path = root.join(format!("task-drafts/{}{suffix}.json", ids[2]));
        let mut snapshot: workspace::Snapshot =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        snapshot[0].2 = 0o120777;
        fs::write(path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
        if suffix.is_empty() {
            let path = root.join(format!("checks/{}.json", ids[2]));
            let mut bundle: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            bundle["receipt"]["fingerprint"] = json!(workspace::fingerprint(&snapshot).unwrap());
            fs::write(path, serde_json::to_vec(&bundle).unwrap()).unwrap();
        }
    }
    assert!(
        store
            .request_repair(mod_id, &task_source(ids[2], 1), &repair)
            .unwrap()
            .unwrap_err()
            .contains("unchanged regular source file")
    );
    assert_unchanged(&store, mod_id, &plan, &ids);
}

#[test]
fn missing_assignment_waits_for_active_peers_and_pause_survives_replay() {
    let (_data, mut store, mod_id, _, ids, _, repair) = fixture();
    let source = task_source(ids[2], 1);
    assert_eq!(
        store.request_repair(mod_id, &source, &repair).unwrap(),
        Ok(())
    );
    store
        .0
        .execute(
            "UPDATE task_runs SET status='checking' WHERE id=?1",
            [ids[0]],
        )
        .unwrap();
    assert!(!store.resume_repairs(mod_id).unwrap());
    store.pause_repairs(mod_id).unwrap();
    assert_eq!(
        store.request_repair(mod_id, &source, &repair).unwrap(),
        Ok(())
    );
    let mut different = repair.clone();
    different.evidence = "Different request".into();
    assert!(
        store
            .request_repair(mod_id, &source, &different)
            .unwrap()
            .is_err()
    );
    assert_eq!(
        store.execution(mod_id).unwrap().unwrap().tasks[2].status,
        "repair_paused"
    );
    store
        .finish_task(
            mod_id,
            &task_source(ids[0], 1),
            "done",
            "Peer finished",
            &[],
        )
        .unwrap();
    store.retry_tasks(mod_id).unwrap();
    assert!(store.resume_repairs(mod_id).unwrap());
    assert!(store.request_repair(mod_id, &source, &repair).is_err());
    assert_eq!(
        store.execution(mod_id).unwrap().unwrap().tasks[2].source,
        task_source(ids[2], 2)
    );
}

#[test]
fn missing_assignment_cannot_spend_exhausted_repair_budget_or_steal_a_new_claim() {
    let (_data, mut store, mod_id, plan, ids, _, repair) = fixture();
    store
        .0
        .execute(
            "UPDATE executions SET repair_count=2 WHERE mod_id=?1",
            [mod_id],
        )
        .unwrap();
    assert!(
        store
            .request_repair(mod_id, &task_source(ids[2], 1), &repair)
            .unwrap()
            .unwrap_err()
            .contains("2 attempts")
    );
    assert_eq!(
        serde_json::to_value(store.planning(mod_id).unwrap().unwrap().plan.unwrap()).unwrap(),
        serde_json::to_value(&plan).unwrap()
    );
    store
        .0
        .execute(
            "UPDATE executions SET repair_count=0 WHERE mod_id=?1",
            [mod_id],
        )
        .unwrap();
    assert_eq!(
        store
            .request_repair(mod_id, &task_source(ids[2], 1), &repair)
            .unwrap(),
        Ok(())
    );
    let mut second = repair.clone();
    second.task = "docs".into();
    second.check = "Docs match".into();
    store
        .0
        .execute(
            "UPDATE task_runs SET status='running' WHERE id=?1",
            [ids[3]],
        )
        .unwrap();
    save_receipt(&store, mod_id, ids[3], &task_source(ids[3], 1), &second);
    assert!(
        store
            .request_repair(mod_id, &task_source(ids[3], 1), &second)
            .unwrap()
            .unwrap_err()
            .contains("overlaps")
    );
    let amended = store.planning(mod_id).unwrap().unwrap().plan.unwrap();
    assert_eq!(amended.tasks[2].files, ["tests", MISSED_FILE]);
    assert_eq!(amended.tasks[3].files, ["README.md"]);
    let count: i64 = store
        .0
        .query_row(
            "SELECT repair_count FROM executions WHERE mod_id=?1",
            [mod_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}
