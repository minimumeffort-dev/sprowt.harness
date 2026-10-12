use super::*;

#[test]
fn blocked_partial_review_terminal_caption_keeps_reason_and_fix_action() {
    let (data, store, _) = crate::review::tests::partial::partial_fixture();
    let mut app = App::load(data.0.join("project"), false, store).unwrap();
    app.view = View::Findings(0);
    let text = rows(&screen(&mut app, 160, 70)).join("\n");
    let readable = text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        text.contains("Review incomplete") && text.contains("Reject empty names"),
        "{text}"
    );
    assert!(
        text.contains("required follow-up inspection could not finish")
            && readable.contains("fix issues"),
        "{text}"
    );
    assert_eq!(app.action_dock().primary, Some(Action::FixIssues));
    let review = app
        .timeline_items()
        .into_iter()
        .find(|item| item.key == Key::Review)
        .unwrap();
    assert_eq!(review.title, "Review incomplete · 1 known issue");
    assert_eq!(review.action, Some(Action::FixIssues));
    assert!(!app.engine.can_publish());
}

#[test]
fn observed_task_check_receipt_reaches_terminal_evidence_without_completion() {
    let (data, mut store, m, root, _) = crate::network::tests::fixture();
    let run = &m.execution.as_ref().unwrap().tasks[0];
    let id = run.id;
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
    let mut app = App::load(data.0.join("project"), false, store).unwrap();
    app.view = View::Task(id, 0);
    let text = rows(&screen(&mut app, 160, 70)).join("\n");
    assert!(
        text.contains("Observed task checks")
            && text.contains("2 passed")
            && text.contains("1 failed"),
        "{text}"
    );
    assert!(
        text.contains("Observed command 2: exit 1") && text.contains("Dependency is still blocked"),
        "{text}"
    );
    assert_eq!(app.inspect_task(id).unwrap().state, "Blocked");
    assert!(
        app.engine.mods[0].execution.as_ref().unwrap().tasks[0]
            .checks
            .is_empty()
    );
}

pub(super) fn setup_failed_app() -> (TestData, App, PathBuf, i64) {
    let (data, mut app, root) = execution_app();
    let m = &app.engine.mods[0];
    let id = m.execution.as_ref().unwrap().tasks[0].id;
    let source = crate::store::task_source(id, 2);
    let recipe = crate::environment::Recipe {
        resources: None,
        reusable_paths: vec![],
        inputs: vec![],
        system_packages: vec![],
        setup: vec![crate::environment::Command {
            network_domains: Vec::new(),
            name: "Install browser runtime".into(),
            argv: vec![
                "/usr/bin/npm".into(),
                "exec".into(),
                "playwright".into(),
                "install".into(),
            ],
            timeout_seconds: 180,
        }],
        capabilities: vec![crate::environment::Command {
            network_domains: Vec::new(),
            name: "Browser runtime available".into(),
            argv: vec!["/usr/bin/node".into(), "--version".into()],
            timeout_seconds: 5,
        }],
        baseline: vec![],
    };
    let mut plan = m.planning.as_ref().unwrap().plan.clone().unwrap();
    plan.environment = Some(recipe.clone());
    app.engine
        .store
        .save_plan(m.id, &m.planning.as_ref().unwrap().source, &plan)
        .unwrap();
    app.engine.store.0.execute("UPDATE task_runs SET status='paused',attempt=2,source=?2,summary=?3,checks='[]' WHERE id=?1", rusqlite::params![id,source,"Environment preflight failed: Setup failed: Install browser runtime\nGuest command timed out after 180s."]).unwrap();
    app.engine
        .store
        .0
        .execute(
            "UPDATE executions SET status='running',checks='[]',fingerprint=NULL WHERE mod_id=?1",
            [m.id],
        )
        .unwrap();
    let output = format!(
        "Guest command timed out after 180s.\n{}\nRAW_PREPARATION_TAIL: headless-shell download reached 30%",
        (1..=12)
            .map(|i| format!("browser download progress {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let receipt = crate::environment::Receipt {
        recipe,
        source_changes: Vec::new(),
        error: Some(
            "Setup failed: Install browser runtime\nGuest command timed out after 180s.".into(),
        ),
        environment: "fixture-environment".into(),
        source: "fixture-source-fingerprint".into(),
        runtime_home: "/home/worker".into(),
        cache: crate::environment::cache::Use::Miss,
        cache_saved: None,
        setup: vec![crate::execution::CheckResult {
            task: Some(id),
            check: "Install browser runtime".into(),
            command: vec![
                "/usr/bin/npm".into(),
                "exec".into(),
                "playwright".into(),
                "install".into(),
            ],
            timeout_seconds: Some(180),
            duration_ms: Some(180000),
            exit_code: Some(-1),
            missing_runtime: None,
            output,
        }],
        capabilities: vec![],
        baseline: vec![],
    };
    std::fs::create_dir_all(root.join("environments")).unwrap();
    let bytes = serde_json::to_vec(&receipt).unwrap();
    for name in [format!("{id}.json"), format!("{id}-{source}.json")] {
        std::fs::write(root.join("environments").join(name), &bytes).unwrap();
    }
    let project = app.engine.project.clone();
    drop(app);
    let app = App::load(project, false, data.store()).unwrap();
    (data, app, root, id)
}

pub(super) fn network_preparation_app() -> (TestData, App, PathBuf, i64) {
    let (data, mut app, root, id) = setup_failed_app();
    let mod_id = app.engine.mods[0].id;
    let source = app.inspect_task(id).unwrap().run.source.clone();
    let worker = app
        .engine
        .store
        .worker_provider(mod_id, Role::Executor, 0, "codex")
        .unwrap();
    app.engine
        .store
        .0
        .execute(
            "UPDATE task_runs SET worker_id=?2,status='sending' WHERE id=?1",
            rusqlite::params![id, worker.id],
        )
        .unwrap();
    let policy = crate::sandbox::network(&root).unwrap();
    let request = crate::network::Request::parse(serde_json::json!({"domains":["huggingface.co","us.aws.cdn.hf.co"],"reason":"Preparation: Download verified model fixtures"})).unwrap();
    let error = crate::network::require_preparation(
        &app.engine.store,
        &root,
        mod_id,
        worker.id,
        &source,
        request,
        &policy,
    )
    .unwrap_err();
    app.engine
        .store
        .finish_task(
            mod_id,
            &source,
            "paused",
            &format!("Environment preflight failed: {error}"),
            &[],
        )
        .unwrap();
    let mut receipt = crate::environment::load(&root, id).unwrap().unwrap();
    receipt.error = Some(error.to_string());
    receipt.setup.clear();
    for name in [format!("{id}.json"), format!("{id}-{source}.json")] {
        std::fs::write(
            root.join("environments").join(name),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
    }
    let project = app.engine.project.clone();
    drop(app);
    let mut app = App::load(project, false, data.store()).unwrap();
    app.refresh_network_requests().unwrap();
    (data, app, root, id)
}

#[test]
fn workbench_preparation_network_gate_offers_approval_before_editing() {
    let (_data, mut app, root, id) = network_preparation_app();
    assert_eq!(app.action_dock().primary, Some(Action::Network));
    assert!(!app.can_retry_task(id));
    let text = rows(&screen(&mut app, 160, 60))
        .join("\n")
        .replace('\u{a0}', " ");
    assert!(
        text.contains("Needs network access") && text.contains("Review network access"),
        "{text}"
    );
    key(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
    let dialog = rows(&screen(&mut app, 80, 36)).join("\n");
    assert!(
        dialog.contains("huggingface.co") && dialog.contains("us.aws.cdn.hf.co"),
        "{dialog}"
    );
    assert_eq!(app.input.lines(), ["keep this draft"]);
    assert!(crate::network::grants(&root).unwrap().is_empty());
}

pub(super) fn source_changed_app(legacy: bool) -> (TestData, App, PathBuf, i64) {
    let (data, app, root, id) = setup_failed_app();
    let reason = if legacy {
        "Setup or baseline changed source. Edits are retained; correct the recipe before retrying."
    } else {
        "Setup or baseline changed source: package-lock.json, source.txt.\nEdits are retained. Revise the setup recipe before retrying."
    };
    let execution = app.engine.mods[0].execution.as_ref().unwrap();
    let source = &execution.tasks[0].source;
    let mut receipt = crate::environment::load(&root, id).unwrap().unwrap();
    receipt.error = Some(reason.into());
    if !legacy {
        receipt.source_changes = vec!["package-lock.json".into(), "source.txt".into()];
    }
    receipt.setup[0].exit_code = Some(0);
    receipt.setup[0].duration_ms = Some(2000);
    receipt.setup[0].output = "Browser installed".into();
    for name in [format!("{id}.json"), format!("{id}-{source}.json")] {
        std::fs::write(
            root.join("environments").join(name),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
    }
    app.engine
        .store
        .0
        .execute(
            "UPDATE task_runs SET summary=?2 WHERE id=?1",
            rusqlite::params![id, format!("Environment preflight failed: {reason}")],
        )
        .unwrap();
    let project = app.engine.project.clone();
    drop(app);
    let app = App::load(project, false, data.store()).unwrap();
    (data, app, root, id)
}

#[test]
fn workbench_source_changing_preparation_recommends_correction_after_restart() {
    let (_data, mut app, root, id) = source_changed_app(true);
    let mod_id = app.engine.mods[0].id;
    assert_eq!(app.state_summary().primary, Some(Action::ReviseRepair));
    let dock = app.action_dock();
    assert!(
        dock.actions
            .iter()
            .any(|a| a.action == Action::ReviseRepair && a.label == "Revise setup recipe")
    );
    assert!(
        dock.actions
            .iter()
            .any(|a| a.action == Action::RetryTask(id))
    );
    assert_eq!(app.input.lines(), ["keep this draft"]);
    let before = app.engine.store.execution(mod_id).unwrap().unwrap().tasks[0]
        .source
        .clone();
    key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::NONE);
    assert!(matches!(app.engine.view, View::Task(task, 0) if task == id));
    assert_eq!(
        app.engine.store.execution(mod_id).unwrap().unwrap().tasks[0].source,
        before
    );
    assert!(root.exists());
    key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE);
    assert!(app.engine.auto_plans.contains(&mod_id));
    assert!(app.engine.mods[0].messages.iter().any(|message| {
        message
            .body
            .contains("Correct the setup recipe that changed source")
    }));
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn workbench_source_change_paths_reach_full_evidence_and_correction() {
    let (_data, mut app, _root, id) = source_changed_app(false);
    app.engine.view = View::Task(id, 0);
    key(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
    let text = rows(&screen(&mut app, 160, 70)).join("\n");
    assert!(
        text.contains("Changed source paths")
            && text.contains("package-lock.json")
            && text.contains("source.txt"),
        "{text}"
    );
    key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE);
    let message = app.engine.mods[0]
        .messages
        .iter()
        .find(|message| {
            message
                .body
                .contains("Correct the setup recipe that changed source")
        })
        .unwrap();
    assert!(
        message.body.contains("source_changes")
            && message.body.contains("package-lock.json")
            && message.body.contains("source.txt")
    );
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn workbench_earlier_source_change_does_not_replace_current_download_retry() {
    let (data, app, root, id) = source_changed_app(false);
    let mod_id = app.engine.mods[0].id;
    let project = app.engine.project.clone();
    let old_source = &app.engine.mods[0].execution.as_ref().unwrap().tasks[0].source;
    std::fs::remove_file(
        root.join("environments")
            .join(format!("{id}-{old_source}.json")),
    )
    .unwrap();
    app.engine.store.0.execute("UPDATE task_runs SET source=?2,summary=?3 WHERE id=?1", rusqlite::params![id,crate::store::task_source(id, 3),"Environment preflight failed: Setup failed: Download browser\nGuest command timed out after 180s."]).unwrap();
    drop(app);
    let app = App::load(project, false, data.store()).unwrap();
    assert_eq!(app.state_summary().primary, Some(Action::RetryTask(id)));
    assert!(
        !app.action_dock()
            .actions
            .iter()
            .any(|a| a.action == Action::ReviseRepair)
    );
    assert_eq!(app.engine.mods[0].id, mod_id);
}

#[test]
fn workbench_source_change_correction_hint_waits_for_busy_peer() {
    let (_data, mut app, _, id) = source_changed_app(false);
    let record = app.engine.store.worker(app.engine.mods[0].id).unwrap();
    let mut worker = Worker::start(
        &app.engine.project,
        &app.engine.mods[0],
        record,
        Role::Planner,
        None,
    )
    .unwrap();
    worker.role = Role::Executor;
    worker.status = Status::Running;
    worker.bind_test_task(crate::store::task_source(id + 1, 1));
    app.engine.workers.insert(worker.id, worker);
    app.engine.view = View::Task(id, 0);
    let text = rows(&screen(&mut app, 160, 70)).join("\n");
    assert!(
        text.contains("package-lock.json") && text.contains("source.txt"),
        "{text}"
    );
    assert!(
        !text.contains("ctrl+g v") && !text.contains("revise setup recipe"),
        "{text}"
    );
    assert!(
        !app.action_dock()
            .actions
            .iter()
            .any(|action| action.action == Action::ReviseRepair)
    );
}
