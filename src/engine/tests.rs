use super::*;
mod navigation;
mod observed_checks;
mod partial_review;
mod workspace_views;
use crate::{
    engine::api::{Command, Direction},
    store::test_support::TestData,
};

fn session() -> (TestData, Engine, i64, i64) {
    let data = TestData::new();
    let mut store = data.store();
    let project = data.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project_id = store.load_project(&project).unwrap().id;
    let first = store
        .create_mod(project_id, "Add native controls")
        .unwrap()
        .id;
    let second = store.create_mod(project_id, "Other codemod").unwrap().id;
    store.select_mod(project_id, first).unwrap();
    let engine = Engine::load(project, store).unwrap();
    (data, engine, first, second)
}

#[test]
fn desktop_preparation_network_dialog_preserves_draft_and_applies_explicit_decision() {
    for allow in [true, false] {
        let (data, mut store, m, root, workers) = crate::network::tests::fixture();
        let source = &m.execution.as_ref().unwrap().tasks[0].source;
        let policy = crate::sandbox::network(&root).unwrap();
        let request = crate::network::Request::parse(serde_json::json!({"domains":["models.example.org"],"reason":"Preparation: Download verified fixtures"})).unwrap();
        let error = crate::network::require_preparation(
            &store, &root, m.id, workers[0], source, request, &policy,
        )
        .unwrap_err();
        store
            .finish_task(
                m.id,
                source,
                "paused",
                &format!("Environment preflight failed: {error}"),
                &[],
            )
            .unwrap();
        let mut engine = Engine::load(data.0.join("project"), store).unwrap();
        engine.refresh_network_requests().unwrap();
        engine
            .command(Command::SetDraft {
                mod_id: m.id,
                text: "Keep this draft".into(),
            })
            .unwrap();
        let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
        assert!(
            snapshot["status"]
                .as_str()
                .unwrap()
                .starts_with("Needs network access")
        );
        assert!(
            snapshot["actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|action| action["id"] == "network"
                    && action["label"] == "Review network access")
        );
        engine
            .command(Command::Action {
                mod_id: Some(m.id),
                action: "network".into(),
                task_id: None,
            })
            .unwrap();
        let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
        assert_eq!(snapshot["dialog"]["kind"], "network");
        assert_eq!(
            snapshot["dialog"]["domains"],
            serde_json::json!(["models.example.org"])
        );
        let request_id = snapshot["dialog"]["request_id"].as_i64().unwrap();
        assert!(crate::network::grants(&root).unwrap().is_empty());
        engine
            .command(Command::NetworkDecision {
                mod_id: m.id,
                request_id,
                allow,
            })
            .unwrap();
        assert_eq!(engine.mods[0].draft, "Keep this draft");
        assert_eq!(
            engine.store.network_requests(m.id).unwrap()[0].status,
            if allow { "approved" } else { "denied" }
        );
        assert_eq!(
            crate::network::grants(&root)
                .unwrap()
                .contains_key("models.example.org"),
            allow
        );
    }
}

#[test]
fn desktop_commands_preserve_drafts_queue_order_and_saved_steering_on_reopen() {
    let (data, mut engine, first, second) = session();
    engine
        .command(Command::Submit {
            mod_id: first,
            text: "First instruction".into(),
        })
        .unwrap();
    engine
        .command(Command::Submit {
            mod_id: first,
            text: "Second instruction".into(),
        })
        .unwrap();
    engine
        .command(Command::SetDraft {
            mod_id: first,
            text: "Unsent draft".into(),
        })
        .unwrap();
    let ids = engine.mods[0]
        .queue
        .iter()
        .map(|m| m.id)
        .collect::<Vec<_>>();
    engine
        .command(Command::EditQueue {
            mod_id: first,
            message_id: ids[0],
            text: "Revised first".into(),
        })
        .unwrap();
    engine
        .command(Command::MoveQueue {
            mod_id: first,
            message_id: ids[1],
            direction: Direction::Up,
        })
        .unwrap();
    assert_eq!(
        engine.mods[0]
            .queue
            .iter()
            .map(|m| m.body.as_str())
            .collect::<Vec<_>>(),
        ["Second instruction", "Revised first"]
    );
    engine
        .command(Command::SteerQueue {
            mod_id: first,
            message_ids: ids,
        })
        .unwrap();
    assert_eq!(
        engine.mods[0].steering,
        ["Second instruction", "Revised first"]
    );
    assert_eq!(engine.mods[0].draft, "Unsent draft");
    assert!(
        engine.mods[0]
            .messages
            .iter()
            .all(|m| m.body != "Second instruction")
    );
    engine
        .command(Command::SelectMod { mod_id: second })
        .unwrap();
    engine
        .command(Command::SetDraft {
            mod_id: second,
            text: "Other draft".into(),
        })
        .unwrap();
    engine
        .command(Command::SelectMod { mod_id: first })
        .unwrap();
    let project = engine.project.clone();
    drop(engine);
    let reopened = Engine::load(project, data.store()).unwrap();
    assert_eq!(reopened.current_mod().unwrap().draft, "Unsent draft");
    assert_eq!(
        reopened.current_mod().unwrap().steering,
        ["Second instruction", "Revised first"]
    );
    assert_eq!(reopened.mods[1].draft, "Other draft");
    assert!(reopened.workers.is_empty() && reopened.auto_plans.is_empty());
}

#[test]
fn stale_commands_and_closed_submissions_preserve_saved_work() {
    let (_data, mut engine, first, second) = session();
    engine
        .command(Command::SetDraft {
            mod_id: first,
            text: "Saved draft".into(),
        })
        .unwrap();
    assert!(
        engine
            .command(Command::Submit {
                mod_id: second,
                text: "Wrong session".into()
            })
            .unwrap_err()
            .contains("selected codemod changed")
    );
    assert!(
        engine
            .command(Command::Action {
                mod_id: Some(first),
                action: "publish".into(),
                task_id: None
            })
            .unwrap_err()
            .contains("no longer available")
    );
    engine.mods[0].closed = true;
    assert_eq!(
        engine
            .command(Command::Submit {
                mod_id: first,
                text: "Forbidden replacement".into()
            })
            .unwrap_err(),
        "Wait for this operation to finish."
    );
    assert_eq!(engine.mods[0].draft, "Saved draft");
    assert!(engine.mods[0].queue.is_empty());
    let saved = engine.store.load_project(&engine.project).unwrap();
    assert_eq!(saved.mods[0].draft, "Saved draft");
}

#[test]
fn deletion_requires_the_open_matching_confirmation_and_can_be_cancelled() {
    let (_data, mut engine, first, second) = session();
    engine
        .command(Command::SetDraft {
            mod_id: first,
            text: "Keep this".into(),
        })
        .unwrap();
    assert!(
        engine
            .command(Command::Confirm {
                mod_id: Some(first),
                kind: "delete".into()
            })
            .is_err()
    );
    engine
        .command(Command::Action {
            mod_id: Some(first),
            action: "delete".into(),
            task_id: None,
        })
        .unwrap();
    assert_eq!(engine.snapshot().dialog.unwrap().kind, "delete");
    assert!(
        engine
            .command(Command::Confirm {
                mod_id: Some(second),
                kind: "delete".into()
            })
            .is_err()
    );
    engine.command(Command::Cancel {}).unwrap();
    assert_eq!(engine.mods.len(), 2);
    assert_eq!(engine.composer, "Keep this");
    engine
        .command(Command::Action {
            mod_id: Some(first),
            action: "delete".into(),
            task_id: None,
        })
        .unwrap();
    engine
        .command(Command::Confirm {
            mod_id: Some(first),
            kind: "delete".into(),
        })
        .unwrap();
    assert_eq!(
        engine.mods.iter().map(|m| m.id).collect::<Vec<_>>(),
        [second]
    );
}

#[test]
fn new_project_preview_does_not_initialize_git_until_confirmed() {
    let data = TestData::new();
    let project = data.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("hello.txt"), "Keep this source").unwrap();
    let mut engine = Engine::load(project.clone(), data.store()).unwrap();
    engine
        .command(Command::NewMod {
            description: "Add controls".into(),
        })
        .unwrap();
    assert_eq!(engine.snapshot().dialog.unwrap().kind, "project_setup");
    assert!(!project.join(".git").exists());
    assert!(engine.mods.is_empty());
    engine.command(Command::Cancel {}).unwrap();
    assert_eq!(
        std::fs::read_to_string(project.join("hello.txt")).unwrap(),
        "Keep this source"
    );
    assert_eq!(engine.composer, "Add controls");
}

#[test]
fn repository_setup_has_distinct_entry_and_confirmation_without_a_side_effect() {
    let (_data, mut engine, first, _) = session();
    engine.view = View::Repository(false);
    assert_eq!(
        engine.snapshot().dialog.unwrap().confirm_label.as_deref(),
        Some("Continue")
    );
    engine
        .command(Command::Repository {
            mod_id: first,
            slug: "example/project".into(),
            create: true,
        })
        .unwrap();
    let dialog = engine.snapshot().dialog.unwrap();
    assert_eq!(dialog.kind, "repository_confirmation");
    assert_eq!(dialog.confirm_label.as_deref(), Some("Create repository"));
    assert!(engine.git_jobs.is_empty());
    engine.command(Command::Cancel {}).unwrap();
    assert!(engine.git_jobs.is_empty());
}

#[test]
fn ownership_rejects_duplicate_controllers_and_releases_on_exit() {
    let data = TestData::new();
    let database = data.0.join("state.db");
    let project = data.0.join("project");
    let first = ownership::acquire(&database, &project).unwrap();
    assert!(
        ownership::acquire(&database, &project)
            .unwrap_err()
            .to_string()
            .contains("already open")
    );
    let other = ownership::acquire(&database, &data.0.join("other-project")).unwrap();
    drop(first);
    let reopened = ownership::acquire(&database, &project).unwrap();
    drop((other, reopened));
}

#[test]
fn desktop_contract_matches_the_native_fixture() {
    let (_data, mut engine, first, _) = session();
    engine.project = PathBuf::from("/example/project");
    let plan = crate::plan::Plan::parse_generated(r#"{"title":"Add native controls","summary":"Share the engine with native controls","tasks":[{"id":"core","title":"Extract the engine","outcome":"Both clients use the same lifecycle rules","files":["src/engine.rs"],"depends_on":[],"worker":"codex","checks":["CLI regression suite passes"]},{"id":"native","title":"Add native controls","outcome":"Open a project and manage codemods","files":["macos/Sources"],"depends_on":["core"],"worker":"codex","checks":["Draft survives reopening"]}]}"#).unwrap();
    let source = engine.mods[0].planning.as_ref().unwrap().source.clone();
    engine.store.save_plan(first, &source, &plan).unwrap();
    engine.mods[0].name = plan.title.clone().unwrap();
    engine.mods[0].planning = engine.store.planning(first).unwrap();
    engine
        .command(Command::Submit {
            mod_id: first,
            text: "Keep keyboard shortcuts".into(),
        })
        .unwrap();
    engine
        .command(Command::SetDraft {
            mod_id: first,
            text: "Do not lose this draft".into(),
        })
        .unwrap();
    engine.mods[0].messages.push(crate::store::Message {
        source: None,
        task: None,
        item_id: None,
        role: "planner".into(),
        body: "I will split this into two tasks.".into(),
        model: Some("gpt-6.1-sol".into()),
        effort: Some("high".into()),
    });
    engine.mods[0]
        .steering
        .push("Preserve the existing keyboard controls".into());
    engine
        .store
        .create_execution(first, &PathBuf::from("/example/workspace"), &plan)
        .unwrap();
    let mut execution = engine.store.execution(first).unwrap().unwrap();
    execution.status = "blocked".into();
    execution.tasks[0].status = "done".into();
    execution.tasks[1].status = "paused".into();
    execution.tasks[1].summary = "Preserve the native composer draft.".into();
    execution.checks = vec![crate::execution::CheckResult {
        timeout_seconds: Some(60),
        duration_ms: Some(50),
        task: Some(execution.tasks[1].id),
        check: "CLI regression suite passes".into(),
        command: vec!["cargo".into(), "test".into()],
        exit_code: Some(1),
        output: "Draft did not survive reopening".into(),
        missing_runtime: None,
    }];
    let run_id = execution.tasks[1].id;
    let source = execution.tasks[1].source.clone();
    engine.mods[0].execution = Some(execution);
    let record = engine.store.worker(first).unwrap();
    let mut worker = Worker::start(
        &engine.project,
        &engine.mods[0],
        record,
        Role::Planner,
        None,
    )
    .unwrap();
    worker.status = Status::Ready;
    worker.enabled = false;
    worker.model = Some("gpt-6.1-sol".into());
    worker.effort = Some("high".into());
    engine.workers.insert(worker.id, worker);
    engine.mods[0].messages.push(crate::store::Message {
        source: Some(source),
        task: Some(run_id),
        item_id: None,
        role: "codex:executor".into(),
        body: "Native controls are ready; draft persistence needs repair.".into(),
        model: Some("gpt-6.1-sol".into()),
        effort: Some("high".into()),
    });
    engine.mods[0].agent_review = Some(crate::review::State {
        status: "findings".into(),
        issues: vec![crate::review::Issue {
            id: 1,
            status: "confirmed".into(),
            attempts: 0,
            regression: "Draft survives reopening".into(),
            history: Vec::new(),
            finding: crate::review::Finding {
                id: Some(1),
                triage: None,
                owner: "core".into(),
                file: "src/engine.rs".into(),
                line: 42,
                priority: "P1".into(),
                title: "Keep the unsent draft".into(),
                evidence: "Reopening discarded the composer text.".into(),
                fix: "Persist the draft before switching sessions.".into(),
            },
        }],
        ..Default::default()
    });
    engine.targets.insert(
        engine.mods[0].id,
        crate::mod_sync::Target {
            branch: "main".into(),
            head: "target-head".into(),
            pr_state: Some("OPEN".into()),
            pr_feedback: vec![crate::pr_feedback::Feedback {
                id: "thread:30".into(),
                title: "src/engine.rs:42 · rev".into(),
                detail: "This drops the draft.\nhttps://example.test/t30".into(),
                status: "unresolved thread".into(),
                actionable: true,
                file: Some("src/engine.rs".into()),
                line: Some(42),
            }],
            pr_feedback_error: None,
        },
    );
    engine.view = View::DeleteMod(0);
    let actual = serde_json::to_value(engine.response(Some(7), None)).unwrap();
    assert_eq!(
        actual["snapshot"]["active_mod"]["messages"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        actual["snapshot"]["active_mod"]["history"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/desktop.json");
    if std::env::var_os("SPROWT_DESKTOP_FIXTURE").is_some() {
        std::fs::write(
            &fixture,
            serde_json::to_string_pretty(&actual).unwrap() + "\n",
        )
        .unwrap();
    }
    let expected: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&fixture).unwrap()).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn desktop_fix_pr_feedback_rejects_unknown_and_empty_selections() {
    let (_data, mut engine, first, _) = session();
    engine.targets.insert(
        first,
        crate::mod_sync::Target {
            branch: "main".into(),
            head: "target-head".into(),
            pr_state: Some("OPEN".into()),
            pr_feedback: vec![crate::pr_feedback::Feedback {
                id: "thread:30".into(),
                title: "src/engine.rs:42 · rev".into(),
                detail: "This drops the draft.".into(),
                status: "unresolved thread".into(),
                actionable: true,
                file: Some("src/engine.rs".into()),
                line: Some(42),
            }],
            pr_feedback_error: None,
        },
    );
    let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
    assert_eq!(
        snapshot["active_mod"]["pr_feedback"][0]["imported"],
        serde_json::json!(false)
    );
    let empty = engine
        .command(Command::FixPrFeedback {
            mod_id: first,
            feedback_ids: vec![],
        })
        .unwrap_err();
    assert_eq!(empty, "Select PR feedback first.");
    // No clean review exists in this session, and unknown IDs never resolve
    // against the displayed feedback, so neither selection imports anything.
    for ids in [
        vec!["thread:404".to_string()],
        vec!["thread:30".to_string()],
    ] {
        let error = engine
            .command(Command::FixPrFeedback {
                mod_id: first,
                feedback_ids: ids,
            })
            .unwrap_err();
        assert!(
            error.contains("no longer available to send to review"),
            "{error}"
        );
    }
    assert!(engine.mods[0].agent_review.is_none());
}

#[test]
fn desktop_inspections_do_not_block_commands_but_confirmations_do() {
    let (_data, mut engine, _first, second) = session();
    engine.view = View::Failure(0);
    assert!(engine.dialog().is_some_and(|d| d.is_inspection()));
    engine
        .command(Command::SetDraft {
            mod_id: engine.mods[0].id,
            text: "Typed while inspecting".into(),
        })
        .unwrap();
    assert_eq!(engine.mods[0].draft, "Typed while inspecting");
    engine
        .command(Command::SelectMod { mod_id: second })
        .unwrap();
    assert_eq!(engine.mods[engine.active.unwrap()].id, second);
    assert!(matches!(engine.view, View::Chat));
    engine
        .command(Command::SelectMod {
            mod_id: engine.mods[0].id,
        })
        .unwrap();
    assert_eq!(engine.composer, "Typed while inspecting");
    engine.view = View::DeleteMod(0);
    let error = engine
        .command(Command::SelectMod {
            mod_id: engine.mods[0].id,
        })
        .unwrap_err();
    assert_eq!(error, "Finish or cancel the open dialog first.");
}

#[test]
fn ownership_excludes_a_separate_process() {
    const PROBE: &str = "SPROWT_OWNERSHIP_PROBE";
    if let Some(directory) = std::env::var_os(PROBE) {
        let root = PathBuf::from(directory);
        let error = ownership::acquire(&root.join("state.db"), &root.join("project")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        return;
    }
    let data = TestData::new();
    let _owner = ownership::acquire(&data.0.join("state.db"), &data.0.join("project")).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "engine::tests::ownership_excludes_a_separate_process",
            "--nocapture",
        ])
        .env(PROBE, &data.0)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn snapshot_check(
    task: i64,
    output: &str,
    exit_code: Option<i64>,
) -> crate::execution::CheckResult {
    crate::execution::CheckResult {
        task: Some(task),
        check: "Shared check name".into(),
        command: vec!["/usr/bin/test".into()],
        exit_code,
        output: output.into(),
        timeout_seconds: None,
        duration_ms: None,
        missing_runtime: None,
    }
}

#[test]
fn desktop_task_evidence_preserves_attempt_identity_and_final_check_ownership() {
    let (data, mut store, m, _, _) = crate::network::tests::fixture();
    let first = &m.execution.as_ref().unwrap().tasks[0];
    let id = first.id;
    let source = first.source.clone();
    store
        .finish_task(m.id, &source, "paused", "Repair the implementation", &[])
        .unwrap();
    for (attempt, body, model) in [
        (source.as_str(), "Current attempt update", "current-model"),
        ("earlier-attempt", "Old completion report", "old-model"),
    ] {
        store
            .save_message(
                m.id,
                &crate::store::Message {
                    source: Some(attempt.into()),
                    task: Some(id),
                    item_id: Some(format!("result:{attempt}")),
                    role: "codex:executor".into(),
                    body: body.into(),
                    model: Some(model.into()),
                    effort: Some("high".into()),
                },
            )
            .unwrap();
    }
    let mut engine = Engine::load(data.0.join("project"), store).unwrap();
    let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
    let task = &snapshot["active_mod"]["plan"]["tasks"][0];
    assert_eq!(task["run_id"], id);
    assert_eq!(task["source"], source);
    assert_eq!(task["completed"], false);
    assert_eq!(task["error"], "Repair the implementation");
    assert_eq!(task["latest"]["body"], "Current attempt update");
    assert_eq!(task["latest"]["model"], "current-model");
    assert!(task["identity"].as_str().unwrap().contains("current-model"));
    assert!(!task["identity"].as_str().unwrap().contains("old-model"));

    let execution = engine.mods[0].execution.as_mut().unwrap();
    let other = execution.tasks[1].id;
    execution.tasks[0].status = "done".into();
    execution.tasks[0].checks = vec![snapshot_check(id, "Earlier task check passed", Some(0))];
    execution.checks = vec![
        snapshot_check(other, "Other task failed", Some(1)),
        snapshot_check(id, "Current final check failed", Some(1)),
    ];
    let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
    let detail = &snapshot["active_mod"];
    let task = &detail["plan"]["tasks"][0];
    assert_eq!(task["status"], "Final checks failed");
    assert_eq!(task["completed"], true);
    assert_eq!(task["checks_label"], "Final checks");
    assert_eq!(task["results"].as_array().unwrap().len(), 1);
    assert_eq!(task["results"][0]["task_id"], "a");
    assert_eq!(task["results"][0]["kind"], "final_check");
    assert_eq!(task["results"][0]["id"], detail["checks"][1]["id"]);
    assert_eq!(
        task["results"][0]["output"],
        "/usr/bin/test\n\nCurrent final check failed"
    );
    assert_eq!(detail["checks"][0]["task_id"], "b");
    assert_ne!(detail["checks"][0]["id"], detail["checks"][1]["id"]);
    assert_eq!(detail["checks"][2]["kind"], "check");

    let execution = engine.mods[0].execution.as_mut().unwrap();
    execution.checks.clear();
    execution.tasks[0].checks.clear();
    execution.tasks[0].verification_feedback =
        vec![snapshot_check(id, "Previous failure retained", Some(1))];
    let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
    let task = &snapshot["active_mod"]["plan"]["tasks"][0];
    assert_eq!(task["checks_label"], "Previous failed checks");
    assert_eq!(task["results"][0]["kind"], "previous_check");
    assert_eq!(
        task["results"][0]["output"],
        "/usr/bin/test\n\nPrevious failure retained"
    );
}

#[test]
fn desktop_worker_summaries_are_scoped_to_their_codemod_and_actual_state() {
    let (data, mut store, m, _, _) = crate::network::tests::fixture();
    let other = store.create_mod(1, "Other saved work").unwrap().id;
    store.select_mod(1, m.id).unwrap();
    let mut engine = Engine::load(data.0.join("project"), store).unwrap();
    let record = engine.store.worker(m.id).unwrap();
    let mut worker = Worker::start(
        &engine.project,
        &engine.mods[0],
        record,
        Role::Planner,
        None,
    )
    .unwrap();
    worker.role = Role::Executor;
    worker.status = Status::Running;
    worker.model = Some("current-model".into());
    worker.effort = Some("high".into());
    let run = &mut engine.mods[0].execution.as_mut().unwrap().tasks[0];
    run.worker = Some(worker.id);
    run.status = "running".into();
    worker.bind_test_task(run.source.clone());
    engine.mods[0].execution.as_mut().unwrap().tasks[1].status = "pending".into();
    let worker_id = worker.id;
    engine.workers.insert(worker.id, worker);
    let record = engine.store.worker(other).unwrap();
    let mut worker = Worker::start(
        &engine.project,
        &engine.mods[1],
        record,
        Role::Planner,
        None,
    )
    .unwrap();
    worker.status = Status::Failed;
    let other_worker_id = worker.id;
    engine.workers.insert(worker.id, worker);
    engine.mods[1].user_paused = true;

    let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
    assert_eq!(snapshot["phase"], "building");
    assert_eq!(snapshot["mods"][0]["status"], "Working · 1 worker");
    assert_eq!(snapshot["mods"][1]["status"], "Paused by you");
    let worker = &snapshot["mods"][0]["workers"][0];
    assert_eq!(worker["id"], worker_id);
    assert_eq!(worker["role"], "executor");
    assert_eq!(worker["task_id"], "a");
    assert_eq!(worker["state"], "running");
    assert_eq!(worker["activity"], format!("codex w{worker_id} · task 1"));
    assert_eq!(
        worker["identity"],
        format!("codex · executor · w{worker_id} · current-model · high")
    );
    assert_eq!(snapshot["active_mod"]["worker_details"][0], *worker);
    assert_eq!(snapshot["mods"][1]["workers"][0]["id"], other_worker_id);
    assert_eq!(snapshot["mods"][1]["workers"][0]["state"], "failed");
    assert_eq!(
        snapshot["mods"][1]["workers"][0]["task_id"],
        serde_json::Value::Null
    );
    engine.mods[1].closed = true;
    let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
    assert_eq!(snapshot["mods"][1]["status"], "Closed · work saved");
}

#[test]
fn desktop_preparation_is_separate_from_completion_and_keeps_earlier_source_evidence() {
    let (data, store, m, root, _) = crate::network::tests::fixture();
    let run = &m.execution.as_ref().unwrap().tasks[0];
    let recipe = serde_json::from_value(serde_json::json!({
        "inputs":[],"setup":[],"capabilities":[{"name":"Runtime","argv":["/usr/bin/true"],"timeout_seconds":5}],"baseline":[]
    })).unwrap();
    let receipt = crate::environment::Receipt {
        recipe,
        error: Some("Environment preparation stopped.".into()),
        source_changes: Vec::new(),
        environment: "fixture-environment".into(),
        source: "earlier-source-fingerprint".into(),
        runtime_home: "/home/worker".into(),
        cache: crate::environment::cache::Use::Miss,
        cache_saved: None,
        setup: vec![snapshot_check(run.id, "Guest command stopped.", Some(-1))],
        capabilities: vec![snapshot_check(run.id, "Not run.", None)],
        baseline: Vec::new(),
    };
    let directory = root.join("environments");
    std::fs::create_dir_all(&directory).unwrap();
    let bytes = serde_json::to_vec(&receipt).unwrap();
    std::fs::write(directory.join(format!("{}.json", run.id)), &bytes).unwrap();
    let engine = Engine::load(data.0.join("project"), store).unwrap();
    let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
    let task = &snapshot["active_mod"]["plan"]["tasks"][0];
    assert_eq!(task["completed"], false);
    assert_eq!(task["results"], serde_json::json!([]));
    assert_eq!(task["preparation"]["current"], false);
    assert_eq!(task["preparation"]["source"], "earlier-source-fingerprint");
    assert_eq!(task["preparation"]["stopped"], true);
    assert_eq!(
        task["preparation"]["results"][0]["kind"],
        "preparation_setup"
    );
    assert_eq!(task["preparation"]["results"][0]["task_id"], "a");
    assert_eq!(task["preparation"]["results"][0]["status"], "failed");
    assert_eq!(
        task["preparation"]["results"][1]["kind"],
        "preparation_capabilities"
    );
    assert_eq!(task["preparation"]["results"][1]["status"], "skipped");
    std::fs::write(
        directory.join(format!("{}-{}.json", run.id, run.source)),
        bytes,
    )
    .unwrap();
    let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
    assert_eq!(
        snapshot["active_mod"]["plan"]["tasks"][0]["preparation"]["current"],
        true
    );
}

#[test]
fn desktop_selected_task_retry_matches_terminal_policy_and_rejects_stale_or_busy_tasks() {
    let (data, mut store, m, _, _) = crate::network::tests::fixture();
    for run in &m.execution.as_ref().unwrap().tasks {
        store
            .finish_task(m.id, &run.source, "paused", "Task interrupted", &[])
            .unwrap();
    }
    let mut engine = Engine::load(data.0.join("project"), store).unwrap();
    engine
        .command(Command::SetDraft {
            mod_id: m.id,
            text: "Keep this draft during retries".into(),
        })
        .unwrap();
    let execution = engine.mods[0].execution.as_ref().unwrap();
    let first = execution.tasks[0].id;
    let second = execution.tasks[1].id;
    let old_source = execution.tasks[1].source.clone();
    assert_eq!(engine.failed_task(), Some(first));
    assert!(engine.can_retry_task(second));
    assert!(
        !engine
            .action_dock()
            .actions
            .iter()
            .any(|a| a.action == Action::RetryTask(second))
    );
    let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
    assert_eq!(
        snapshot["active_mod"]["plan"]["tasks"][1]["actions"],
        serde_json::json!([{"id":"retry_task","label":"Retry task 2","task_id":second}])
    );
    engine
        .command(Command::Action {
            mod_id: Some(m.id),
            action: "retry_task".into(),
            task_id: Some(second),
        })
        .unwrap();
    let run = &engine.mods[0].execution.as_ref().unwrap().tasks[1];
    assert_eq!(run.status, "pending");
    assert_ne!(run.source, old_source);
    assert_eq!(engine.mods[0].draft, "Keep this draft during retries");
    assert_eq!(
        engine.store.load_project(&engine.project).unwrap().mods[0].draft,
        "Keep this draft during retries"
    );

    for id in [second, second + 100] {
        assert_eq!(
            engine
                .command(Command::Action {
                    mod_id: Some(m.id),
                    action: "retry_task".into(),
                    task_id: Some(id),
                })
                .unwrap_err(),
            "This action is no longer available. Review the updated state."
        );
    }
    let record = engine.store.worker(m.id).unwrap();
    let mut worker = Worker::start(
        &engine.project,
        &engine.mods[0],
        record,
        Role::Planner,
        None,
    )
    .unwrap();
    worker.role = Role::Executor;
    worker.status = Status::Running;
    worker.bind_test_task(
        engine.mods[0].execution.as_ref().unwrap().tasks[0]
            .source
            .clone(),
    );
    engine.workers.insert(worker.id, worker);
    assert!(!engine.can_retry_task(first));
    assert!(
        engine
            .command(Command::Action {
                mod_id: Some(m.id),
                action: "retry_task".into(),
                task_id: Some(first),
            })
            .is_err()
    );
    engine.workers.clear();
    engine.view = View::DeleteMod(0);
    assert!(
        engine
            .command(Command::Action {
                mod_id: Some(m.id),
                action: "retry_task".into(),
                task_id: Some(first),
            })
            .is_err()
    );
    engine.command(Command::Cancel {}).unwrap();
    let other = engine.store.create_mod(1, "Other context").unwrap();
    engine.mods.push(other);
    engine
        .command(Command::SelectMod {
            mod_id: engine.mods[1].id,
        })
        .unwrap();
    assert_eq!(
        engine
            .command(Command::Action {
                mod_id: Some(m.id),
                action: "retry_task".into(),
                task_id: Some(first),
            })
            .unwrap_err(),
        "The selected codemod changed. Review it before continuing."
    );
    assert_eq!(engine.mods[0].draft, "Keep this draft during retries");
}

#[test]
fn desktop_task_fix_check_is_available_only_for_the_canonical_failed_owner() {
    let (data, mut store, m, _, _) = crate::network::tests::fixture();
    let execution = m.execution.as_ref().unwrap();
    for run in &execution.tasks {
        store
            .finish_task(
                m.id,
                &run.source,
                "done",
                "Task finished",
                &[snapshot_check(run.id, "Task check passed", Some(0))],
            )
            .unwrap();
    }
    let first = execution.tasks[0].id;
    let second = execution.tasks[1].id;
    store
        .execution_checks(
            m.id,
            "blocked",
            &[snapshot_check(first, "Combined regression failed", Some(1))],
            Some("version"),
        )
        .unwrap();
    let mut engine = Engine::load(data.0.join("project"), store).unwrap();
    engine
        .command(Command::SetDraft {
            mod_id: m.id,
            text: "Keep the draft".into(),
        })
        .unwrap();
    let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
    assert_eq!(
        snapshot["active_mod"]["plan"]["tasks"][0]["actions"],
        serde_json::json!([{"id":"fix_check","label":"Fix failed check","task_id":first}])
    );
    assert_eq!(
        snapshot["active_mod"]["plan"]["tasks"][1]["actions"],
        serde_json::json!([])
    );
    assert!(
        engine
            .command(Command::Action {
                mod_id: Some(m.id),
                action: "fix_check".into(),
                task_id: Some(second)
            })
            .is_err()
    );
    engine
        .command(Command::Action {
            mod_id: Some(m.id),
            action: "fix_check".into(),
            task_id: Some(first),
        })
        .unwrap();
    let run = &engine.mods[0].execution.as_ref().unwrap().tasks[0];
    assert_eq!(run.status, "pending");
    assert!(run.check_repair);
    assert_eq!(
        run.verification_feedback[0].output,
        "Combined regression failed"
    );
    assert_eq!(engine.mods[0].draft, "Keep the draft");
    assert!(
        engine
            .command(Command::Action {
                mod_id: Some(m.id),
                action: "fix_check".into(),
                task_id: Some(first)
            })
            .is_err()
    );
}
