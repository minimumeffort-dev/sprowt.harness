use super::*;

pub(super) fn source_changed_app(legacy: bool) -> (TestData, App, PathBuf, i64) {
    let (data, app, root, id) = setup_failed_app();
    let reason = if legacy {
        "Setup or baseline changed source. Edits are retained; correct the recipe before retrying."
    } else {
        "Setup or baseline changed source: package-lock.json, source.txt.\nEdits are retained. Revise the setup recipe before retrying."
    };
    let execution = app.mods[0].execution.as_ref().unwrap();
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
    app.store
        .0
        .execute(
            "UPDATE task_runs SET summary=?2 WHERE id=?1",
            rusqlite::params![id, format!("Environment preflight failed: {reason}")],
        )
        .unwrap();
    let project = app.project.clone();
    drop(app);
    let app = App::load(project, false, data.store()).unwrap();
    (data, app, root, id)
}

#[test]
fn workbench_source_changing_preparation_recommends_correction_after_restart() {
    let (_data, mut app, root, id) = source_changed_app(true);
    let mod_id = app.mods[0].id;
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
    let before = app.store.execution(mod_id).unwrap().unwrap().tasks[0]
        .source
        .clone();
    key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::NONE);
    assert!(matches!(app.view, View::Task(task, 0) if task == id));
    assert_eq!(
        app.store.execution(mod_id).unwrap().unwrap().tasks[0].source,
        before
    );
    assert!(root.exists());
    key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE);
    assert!(app.auto_plans.contains(&mod_id));
    assert!(app.mods[0].messages.iter().any(|message| {
        message
            .body
            .contains("Correct the setup recipe that changed source")
    }));
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn workbench_source_change_paths_reach_full_evidence_and_correction() {
    let (_data, mut app, _root, id) = source_changed_app(false);
    app.view = View::Task(id, 0);
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
    let message = app.mods[0]
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
    let mod_id = app.mods[0].id;
    let project = app.project.clone();
    let old_source = &app.mods[0].execution.as_ref().unwrap().tasks[0].source;
    std::fs::remove_file(
        root.join("environments")
            .join(format!("{id}-{old_source}.json")),
    )
    .unwrap();
    app.store.0.execute("UPDATE task_runs SET source=?2,summary=?3 WHERE id=?1", rusqlite::params![id,crate::store::task_source(id, 3),"Environment preflight failed: Setup failed: Download browser\nGuest command timed out after 180s."]).unwrap();
    drop(app);
    let app = App::load(project, false, data.store()).unwrap();
    assert_eq!(app.state_summary().primary, Some(Action::RetryTask(id)));
    assert!(
        !app.action_dock()
            .actions
            .iter()
            .any(|a| a.action == Action::ReviseRepair)
    );
    assert_eq!(app.mods[0].id, mod_id);
}

#[test]
fn workbench_source_change_correction_hint_waits_for_busy_peer() {
    let (_data, mut app, _, id) = source_changed_app(false);
    let record = app.store.worker(app.mods[0].id).unwrap();
    let mut worker =
        Worker::start(&app.project, &app.mods[0], record, Role::Planner, None).unwrap();
    worker.role = Role::Executor;
    worker.status = Status::Running;
    worker.bind_test_task(crate::store::task_source(id + 1, 1));
    app.workers.insert(worker.id, worker);
    app.view = View::Task(id, 0);
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
