use super::*;
mod preparation;
use crate::app::{
    controls::{Control, WorkspaceView},
    workspace_navigation::{Focus, Key},
};
use preparation::setup_failed_app;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

fn reviewed_app() -> (TestData, App) {
    let (data, mut store, mut m, _) = crate::review::tests::fixture();
    store
        .begin_review(
            m.id,
            m.execution.as_ref().unwrap().fingerprint.as_ref().unwrap(),
        )
        .unwrap();
    m.agent_review = store.review_state(m.id).unwrap();
    store
        .finish_review(
            &m,
            &m.agent_review.as_ref().unwrap().source,
            &crate::review::tests::finding(),
        )
        .unwrap();
    store.save_draft(m.id, "keep this draft").unwrap();
    let app = App::load(data.0.join("project"), false, store).unwrap();
    (data, app)
}

fn click(app: &mut App, control: Control) {
    let (area, _) = app
        .controls
        .hits
        .iter()
        .find(|(_, c)| *c == control)
        .expect("visible control");
    let (column, row) = (area.x, area.y);
    app.handle(Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }))
    .unwrap();
}

fn failed_app() -> (TestData, App, PathBuf) {
    let (data, mut app, root) = execution_app();
    let e = app.engine.mods[0].execution.as_mut().unwrap();
    e.status = "blocked".into();
    e.checks[0].exit_code = Some(1);
    e.checks[0].output = "Expected an empty name to exit nonzero; received exit 0.\nhello".into();
    e.checks[0].duration_ms = Some(82);
    e.checks[0].task = Some(e.tasks[0].id);
    (data, app, root)
}

#[test]
fn workbench_persisted_setup_failure_is_visible_without_a_live_worker() {
    for width in [80, 160] {
        let (_data, mut app, _, id) = setup_failed_app();
        assert!(app.engine.workers.is_empty());
        assert!(
            app.state_summary()
                .error
                .as_deref()
                .is_some_and(|error| error.contains("Guest command timed out after 180s"))
        );
        assert!(app.inspect_task(id).unwrap().checks.is_empty());
        assert!(
            app.engine.mods[0]
                .execution
                .as_ref()
                .unwrap()
                .checks
                .is_empty()
        );

        screen(&mut app, width, 64);
        app.activate_control(Control::Tab(WorkspaceView::Checks))
            .unwrap();
        let rendered = rows(&screen(&mut app, width, 64));
        if width == 160 {
            let dock = rendered
                .iter()
                .position(|row| row.contains("Task 1 needs attention"))
                .unwrap();
            assert!(
                rendered[dock..].iter().any(|row| {
                    row.contains("Guest command timed out after 180s.")
                        && !row.contains("Install browser runtime")
                }),
                "The dock must preserve separate cause and timeout lines"
            );
        }
        let text = rendered.join("\n");
        assert!(
            text.contains("Install browser runtime")
                && text.contains("180.0s elapsed · 180s limit"),
            "{width}:\n{text}"
        );
        assert!(text.contains("Saved preparation") && text.contains("not run"));
        assert!(!text.contains("1 failed · 0 not run"));
        click(&mut app, Control::Action(Action::Failure));
        assert!(matches!(app.engine.view, View::Task(task, 0) if task == id));
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }
}

#[test]
fn workbench_preparation_full_evidence_includes_the_output_tail() {
    let (_data, mut app, _, id) = setup_failed_app();
    app.engine.view = View::Task(id, 0);
    let compact = rows(&screen(&mut app, 160, 70)).join("\n");
    assert!(!compact.contains("RAW_PREPARATION_TAIL"));
    key(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
    let full = rows(&screen(&mut app, 160, 70)).join("\n");
    assert!(full.contains("RAW_PREPARATION_TAIL"), "{full}");
    assert!(full.contains("/usr/bin/npm") && full.contains("180.0s elapsed · 180s limit"));
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn workbench_failure_shortcut_inspects_without_retrying_the_task() {
    let (_data, mut app, _, id) = setup_failed_app();
    let source = app.inspect_task(id).unwrap().run.source.clone();
    key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::NONE);
    assert!(matches!(app.engine.view, View::Task(task, 0) if task == id));
    assert_eq!(app.inspect_task(id).unwrap().run.source, source);
    assert_eq!(app.inspect_task(id).unwrap().run.status, "paused");
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn workbench_previous_preparation_never_becomes_a_new_attempt_failure() {
    let (_data, mut app, _, id) = setup_failed_app();
    let next_source = crate::store::task_source(id, 3);
    app.engine.store.0.execute("UPDATE task_runs SET status='pending',attempt=3,source=?2,summary='',checks='[]' WHERE id=?1", rusqlite::params![id,next_source]).unwrap();
    app.engine.mods[0].execution = app.engine.store.execution(app.engine.mods[0].id).unwrap();
    assert!(app.state_summary().error.is_none());

    app.engine.view = View::Task(id, 0);
    let saved = rows(&screen(&mut app, 160, 70)).join("\n");
    assert!(saved.contains("Earlier preparation"), "{saved}");
    assert!(saved.contains("Guest command timed out after 180s"));
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn workbench_active_worker_takes_precedence_over_saved_preparation_failure() {
    let (_data, mut app, _, id) = setup_failed_app();
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
    worker.status = Status::Starting;
    worker.enabled = true;
    worker.bind_test_task(app.inspect_task(id).unwrap().run.source.clone());
    let run = &mut app.engine.mods[0].execution.as_mut().unwrap().tasks[0];
    run.worker = Some(worker.id);
    run.status = "running".into();
    app.engine.workers.insert(worker.id, worker);
    assert_eq!(app.inspect_task(id).unwrap().state, "Starting");
    assert!(app.inspect_task(id).unwrap().error.is_none());
    assert!(app.state_summary().error.is_none());
}

fn stopped_setup_app() -> (TestData, App, PathBuf, i64) {
    let (data, mut app, root, id) = setup_failed_app();
    let source = app.inspect_task(id).unwrap().run.source.clone();
    let path = root
        .join("environments")
        .join(format!("{id}-{source}.json"));
    let mut receipt: crate::environment::Receipt =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    receipt.error = Some("Environment preparation stopped.".into());
    receipt.setup[0].output = "Guest command stopped.".into();
    let bytes = serde_json::to_vec(&receipt).unwrap();
    std::fs::write(path, &bytes).unwrap();
    std::fs::write(root.join("environments").join(format!("{id}.json")), bytes).unwrap();
    app.engine
        .store
        .finish_task(
            app.engine.mods[0].id,
            &source,
            "paused",
            "Environment preflight failed: Environment preparation stopped.",
            &[],
        )
        .unwrap();
    app.engine.mods[0].execution = app.engine.store.execution(app.engine.mods[0].id).unwrap();
    (data, app, root, id)
}

#[test]
fn workbench_stopped_preparation_is_not_a_failed_environment() {
    let (_data, mut app, _, id) = stopped_setup_app();
    app.engine.mods[0].user_paused = true;
    assert!(app.state_summary().error.is_none());

    app.engine.view = View::Task(id, 0);
    let text = rows(&screen(&mut app, 160, 60)).join("\n");
    assert!(
        text.contains("Preparation stopped") && text.contains("Guest command stopped."),
        "{text}"
    );
    assert!(!text.contains("RAW_PREPARATION_TAIL"));
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn workbench_retained_worker_and_restart_keep_manual_preparation_stop_quiet() {
    for paused in [false, true] {
        let (data, mut app, _, id) = stopped_setup_app();
        let source = app.inspect_task(id).unwrap().run.source.clone();
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
        worker.status = Status::Failed;
        worker.bind_test_task(source);
        worker.error =
            Some("Environment preflight failed: Environment preparation stopped.".into());
        app.engine
            .store
            .0
            .execute(
                "UPDATE task_runs SET worker_id=?2 WHERE id=?1",
                rusqlite::params![id, worker.id],
            )
            .unwrap();
        app.engine.mods[0].execution = app.engine.store.execution(app.engine.mods[0].id).unwrap();
        app.engine
            .store
            .set_user_paused(app.engine.mods[0].id, paused)
            .unwrap();
        app.engine.mods[0].user_paused = paused;
        app.engine.workers.insert(worker.id, worker);
        assert!(
            app.inspect_task(id).unwrap().error.is_none(),
            "retained worker; paused={paused}"
        );
        assert!(app.state_summary().error.is_none());

        let project = app.engine.project.clone();
        drop(app);
        let mut app = App::load(project, false, data.store()).unwrap();
        assert!(app.engine.workers.is_empty());
        assert!(app.inspect_task(id).unwrap().error.is_none());
        assert!(app.state_summary().error.is_none());
        app.engine.view = View::Task(id, 0);
        let text = rows(&screen(&mut app, 160, 60)).join("\n");
        assert!(
            text.contains("Preparation stopped") && !text.contains("Needs attention"),
            "{text}"
        );
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }
}

#[test]
fn workbench_non_executor_errors_remain_visible_with_saved_task_stop() {
    let (_data, mut app, _, id) = stopped_setup_app();
    let record = app.engine.store.worker(app.engine.mods[0].id).unwrap();
    let mut worker = Worker::start(
        &app.engine.project,
        &app.engine.mods[0],
        record,
        Role::Planner,
        None,
    )
    .unwrap();
    worker.status = Status::Failed;
    worker.error = Some("Planner could not connect to its provider.".into());
    app.engine.workers.insert(worker.id, worker);
    assert!(app.inspect_task(id).unwrap().error.is_none());
    assert_eq!(
        app.state_summary().error.as_deref(),
        Some("Planner could not connect to its provider.")
    );
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn workbench_mouse_submit_uses_composer_even_with_evidence_focus() {
    let (_data, mut app, _) = execution_app();
    screen(&mut app, 160, 44);
    app.workspace.focus = Focus::Content;
    click(&mut app, Control::Submit);
    assert_eq!(queued(&app), ["keep this draft"]);
    assert!(app.input.is_empty());
    assert_eq!(app.workspace.focus, Focus::Composer);
}

#[test]
fn workbench_controls_clear_under_dialogs_and_use_canonical_actions() {
    let (_data, mut app) = reviewed_app();
    screen(&mut app, 160, 46);
    let old_submit = app
        .controls
        .hits
        .iter()
        .find(|(_, c)| *c == Control::Submit)
        .unwrap()
        .0;
    click(&mut app, Control::More);
    // Events arriving before the dialog draws must not activate a stale composer button.
    app.handle(Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: old_submit.x,
        row: old_submit.y,
        modifiers: KeyModifiers::NONE,
    }))
    .unwrap();
    screen(&mut app, 160, 46);
    assert!(
        app.controls
            .hits
            .iter()
            .all(|(_, c)| matches!(c, Control::MenuAction(_)))
    );
    assert!(queued(&app).is_empty());
    let expected = app.action_dock().primary.unwrap();
    assert_eq!(expected, Action::FixIssues);
    click(&mut app, Control::MenuAction(expected));
    assert!(app.engine.executing_mods.contains(&app.engine.mods[0].id));
    assert!(app.controls.hits.is_empty());
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn workbench_picker_rows_are_clickable_and_preserve_the_draft() {
    let (_data, mut app, _) = execution_app();
    screen(&mut app, 100, 42);
    click(&mut app, Control::Action(Action::Mods));
    screen(&mut app, 100, 42);
    click(&mut app, Control::PickMod(0));
    assert!(app.engine.view == View::Chat);
    screen(&mut app, 100, 42);
    click(&mut app, Control::Action(Action::Mods));
    screen(&mut app, 100, 42);
    click(&mut app, Control::PickMod(1));
    assert!(app.engine.view == View::NewMod);
    assert_eq!(app.engine.mods[0].draft, "keep this draft");
}

#[test]
fn workbench_global_shortcuts_are_not_consumed_by_pane_controls() {
    let (_data, mut app, _) = execution_app();
    for focus in [Focus::Composer, Focus::Content] {
        screen(&mut app, 160, 44);
        app.workspace.focus = focus;
        assert!(
            !app.workspace_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL))
                .unwrap()
        );
        assert!(
            !app.workspace_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::CONTROL))
                .unwrap()
        );
        assert!(!app.show_evidence);
        app.workspace.focus = focus;
        key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert!(!matches!(app.engine.view, View::Chat));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    }
}

#[test]
fn workbench_review_opens_unresolved_findings_and_keeps_selected_identity() {
    let (_data, mut app) = reviewed_app();
    let review = app.engine.mods[0].agent_review.as_mut().unwrap();
    let mut open = review.issues[0].clone();
    open.id = 2;
    open.finding.id = Some(2);
    open.finding.title = "Preserve named greetings".into();
    review.issues[0].status = "resolved".into();
    review.issues.push(open);
    screen(&mut app, 160, 50);
    app.activate_control(Control::Tab(WorkspaceView::Findings))
        .unwrap();
    screen(&mut app, 160, 50);
    assert_eq!(app.review_finding_ids(), [2, 1]);
    assert_eq!(app.workspace.inspector.finding, Some(2));
    click(&mut app, Control::Finding(1));
    screen(&mut app, 160, 50);
    assert_eq!(app.workspace.inspector.finding, Some(1));
    let review = app.engine.mods[0].agent_review.as_mut().unwrap();
    let mut new_issue = review.issues[1].clone();
    new_issue.id = 3;
    new_issue.finding.id = Some(3);
    new_issue.finding.title = "Reject whitespace names".into();
    review.issues.push(new_issue);
    screen(&mut app, 160, 50);
    assert_eq!(app.review_finding_ids(), [2, 3, 1]);
    assert_eq!(app.workspace.inspector.finding, Some(1));
    key(&mut app, KeyCode::Char('['), KeyModifiers::NONE);
    assert_eq!(app.workspace.inspector.finding, Some(3));
    key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE);
    assert_eq!(app.workspace.inspector.finding, Some(1));
    key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE);
    assert_eq!(app.workspace.inspector.finding, Some(1));
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn workbench_new_codemod_newline_remains_editable_with_a_closed_selection() {
    let (_data, mut app, _) = execution_app();
    app.engine.mods[0].closed = true;
    app.engine.view = View::NewMod;
    app.input.clear();
    app.input.insert_str("New request");
    screen(&mut app, 100, 42);
    click(&mut app, Control::Newline);
    assert_eq!(app.input.lines(), ["New request", ""]);
    assert_eq!(app.workspace.focus, Focus::Composer);
}

#[test]
fn workspace_views_keep_draft_caret_queue_and_typed_digits() {
    let (_data, mut app, _) = execution_app();
    screen(&mut app, 100, 46);
    key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    let cursor = app.input.cursor();
    let draft = app.input.lines().to_vec();
    app.engine.mods[0].queue.push(crate::store::QueuedMessage {
        id: 700,
        body: "Keep queue order".into(),
    });
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    for (index, tab) in WorkspaceView::ALL.into_iter().enumerate() {
        key(
            &mut app,
            KeyCode::Char(char::from(b'1' + index as u8)),
            KeyModifiers::NONE,
        );
        let text = rows(&screen(&mut app, 100, 46)).join("\n");
        assert_eq!(app.workspace.inspector.tab, tab);
        assert!(text.contains("Keep queue order"));
        assert_eq!(app.input.lines(), draft);
        assert_eq!(app.input.cursor(), cursor);
        assert_eq!(queued(&app), ["Keep queue order"]);
    }
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('5'), KeyModifiers::NONE);
    assert_eq!(app.input.lines(), ["keep this draf5t"]);
    assert_eq!(app.workspace.inspector.tab, WorkspaceView::Agents);
}

#[test]
fn workspace_all_five_views_fit_narrow_terminals_and_tasks_have_a_return() {
    for width in [36, 40, 80, 160] {
        let (_data, mut app, _) = execution_app();
        let id = app.engine.mods[0].execution.as_ref().unwrap().tasks[0].id;
        for tab in WorkspaceView::ALL {
            screen(&mut app, width, 50);
            click(&mut app, Control::Tab(tab));
            let text = rows(&screen(&mut app, width, 50)).join("\n");
            assert!(
                text.contains("Conversation")
                    && text.contains("Changes")
                    && text.contains("Checks")
                    && text.contains("Findings")
                    && text.contains("Agents"),
                "{width}:\n{text}"
            );
            assert!(
                !text.contains("Timeline")
                    && !text.contains("Stages")
                    && !text.contains("Workbench")
            );
            assert_eq!(app.workspace.inspector.tab, tab);
            assert_eq!(app.input.lines(), ["keep this draft"]);
        }
        click(&mut app, Control::Tab(WorkspaceView::Conversation));
        screen(&mut app, width, 50);
        click(&mut app, Control::Drill(Key::Task(id)));
        let detail = rows(&screen(&mut app, width, 70)).join("\n");
        assert!(detail.contains("Back to Conversation") && detail.contains("Print hello"));
        assert_eq!(app.workspace.selected, Some(Key::Task(id)));
        click(&mut app, Control::Back);
        screen(&mut app, width, 50);
        assert_eq!(app.workspace.selected, None);
        assert!(
            app.controls
                .hits
                .iter()
                .any(|(_, control)| *control == Control::Drill(Key::Task(id)))
        );
    }
}

#[test]
fn workspace_view_scroll_is_independent_and_resize_preserves_selection() {
    let (_data, mut app, _) = execution_app();
    let template = app.engine.mods[0]
        .planning
        .as_ref()
        .unwrap()
        .plan
        .as_ref()
        .unwrap()
        .tasks[0]
        .clone();
    for index in 1..30 {
        let mut task = template.clone();
        task.id = format!("task-{index}");
        task.title = format!("Independent task {index}");
        app.engine.mods[0]
            .planning
            .as_mut()
            .unwrap()
            .plan
            .as_mut()
            .unwrap()
            .tasks
            .push(task);
    }
    screen(&mut app, 100, 42);
    let area = app.workspace.inspector.area;
    wheel(&mut app, false, area);
    screen(&mut app, 100, 42);
    assert_eq!(app.workspace.inspector.offset, 3);
    click(&mut app, Control::Tab(WorkspaceView::Agents));
    screen(&mut app, 100, 42);
    assert_eq!(app.workspace.inspector.offset, 0);
    click(&mut app, Control::Tab(WorkspaceView::Conversation));
    screen(&mut app, 40, 42);
    assert_eq!(app.workspace.inspector.offset, 3);
    let area = app.workspace.inspector.area;
    wheel(&mut app, false, area);
    screen(&mut app, 40, 42);
    assert_eq!(app.workspace.inspector.offset, 6);
    key(&mut app, KeyCode::Char('l'), KeyModifiers::CONTROL);
    screen(&mut app, 100, 42);
    assert_eq!(app.workspace.inspector.offset, 0);
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn workspace_history_partitions_saved_messages_without_erasing_repeated_requests() {
    let (_data, mut app, _) = execution_app();
    let m = &mut app.engine.mods[0];
    let message = crate::store::Message {
        source: None,
        task: None,
        item_id: None,
        role: "user".into(),
        body: "Repeat this instruction".into(),
        model: None,
        effort: None,
    };
    m.messages = vec![
        message.clone(),
        message.clone(),
        crate::store::Message {
            role: "planner".into(),
            body: "Earlier planning notes".into(),
            ..message
        },
    ];
    let before = rows(&screen(&mut app, 160, 64)).join("\n");
    assert_eq!(before.matches("Repeat this instruction").count(), 2);
    assert!(!before.contains("Earlier planning notes"));
    click(&mut app, Control::History);
    let after = rows(&screen(&mut app, 160, 64)).join("\n");
    assert_eq!(after.matches("Repeat this instruction").count(), 2);
    assert_eq!(after.matches("Earlier planning notes").count(), 1);
    assert!(after.contains("Codex · planner"));
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn workspace_checks_are_global_until_a_task_is_explicitly_selected() {
    let (_data, mut app, _) = failed_app();
    let first = app.engine.mods[0].execution.as_ref().unwrap().tasks[0].id;
    screen(&mut app, 160, 70);
    click(&mut app, Control::Drill(Key::Task(first)));
    screen(&mut app, 160, 70);
    click(&mut app, Control::TaskChecks(first));
    let scoped = rows(&screen(&mut app, 160, 70)).join("\n");
    assert!(scoped.contains("Expected an empty name"));
    assert_eq!(app.workspace.selected, Some(Key::Task(first)));
    click(&mut app, Control::Tab(WorkspaceView::Checks));
    let global = rows(&screen(&mut app, 160, 70)).join("\n");
    assert_eq!(app.workspace.selected, None);
    assert!(global.contains("Expected an empty name"));
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn workspace_replaced_plan_discards_target_without_losing_view_or_draft() {
    let (_data, mut app, _) = execution_app();
    let id = app.engine.mods[0].execution.as_ref().unwrap().tasks[0].id;
    screen(&mut app, 100, 60);
    click(&mut app, Control::Drill(Key::Task(id)));
    app.engine.mods[0].planning.as_mut().unwrap().source = "replacement-plan".into();
    app.engine.mods[0]
        .planning
        .as_mut()
        .unwrap()
        .plan
        .as_mut()
        .unwrap()
        .tasks
        .clear();
    app.engine.mods[0].execution = None;
    let text = rows(&screen(&mut app, 100, 60)).join("\n");
    assert_eq!(app.workspace.selected, None);
    assert_eq!(app.workspace.inspector.tab, WorkspaceView::Conversation);
    assert!(!text.contains("No tasks") && !text.contains("Back to Conversation"));
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
#[ignore = "exports real terminal buffers for visual review"]
fn export_workspace_buffers() {
    let directory =
        PathBuf::from(std::env::var("SPROWT_WORKSPACE_BUFFERS").expect("output directory"));
    std::fs::create_dir_all(&directory).unwrap();
    let (_data, mut app) = reviewed_app();
    app.engine.project = PathBuf::from("/example/project");
    for (suffix, width, height) in [("wide", 160, 48), ("compact", 80, 42), ("narrow", 40, 42)] {
        for tab in WorkspaceView::ALL {
            screen(&mut app, width, height);
            app.activate_control(Control::Tab(tab)).unwrap();
            let buffer = screen(&mut app, width, height);
            let cells: Vec<_> = buffer.content.iter().enumerate().map(|(index,cell)| serde_json::json!({"x":index % width as usize,"y":index / width as usize,"symbol":cell.symbol(),"fg":format!("{:?}",cell.fg),"bg":format!("{:?}",cell.bg),"bold":cell.modifier.contains(ratatui::style::Modifier::BOLD)})).collect();
            std::fs::write(
                directory.join(format!("{}-{suffix}.json", tab.label().to_lowercase())),
                serde_json::to_vec(
                    &serde_json::json!({"width":width,"height":height,"cells":cells}),
                )
                .unwrap(),
            )
            .unwrap();
        }
    }
}

#[test]
fn workspace_findings_preserves_github_feedback_import_guards() {
    let (_data, mut app, _root) = execution_app();
    let id = app.engine.mods[0].id;
    let fingerprint = app.engine.mods[0]
        .execution
        .as_ref()
        .unwrap()
        .fingerprint
        .clone();
    let mut git = git_mod::GitMod {
        repo: app.engine.project.clone(),
        branch: "sprowt/test".into(),
        base: "base".into(),
        base_branch: Some("main".into()),
        phase: "published".into(),
        fingerprint: None,
        head: Some("head".into()),
        pr: Some("https://github.com/fixture/project/pull/1".into()),
        draft: false,
        removing: false,
        publishing: false,
        closing: false,
        discarding: false,
        continuing: false,
        published_head: Some("head".into()),
        checkpoint: Some("head".into()),
    };
    git.fingerprint = fingerprint;
    app.engine.git_states.insert(id, git);
    app.engine.targets.insert(
        id,
        crate::mod_sync::Target {
            branch: "main".into(),
            head: "base".into(),
            pr_state: Some("OPEN".into()),
            pr_feedback: vec![crate::pr_feedback::Feedback {
                id: "check:unit tests".into(),
                title: "unit tests".into(),
                detail: "This GitHub check failed on the published commit.".into(),
                status: "check failed".into(),
                actionable: true,
                file: None,
                line: None,
            }],
            pr_feedback_error: None,
        },
    );
    assert!(app.published());
    screen(&mut app, 160, 60);
    click(&mut app, Control::Tab(WorkspaceView::Findings));
    let text = rows(&screen(&mut app, 160, 60)).join("\n");
    assert!(text.contains("GitHub PR feedback") && text.contains("check failed · unit tests"));
    assert!(
        app.action_dock()
            .evidence
            .contains("GitHub PR · 1 feedback item to review")
    );
    click(&mut app, Control::Action(Action::PrFeedback));
    assert!(matches!(app.engine.view, View::PrFeedback(0)));
    let display = rows(&screen(&mut app, 100, 30)).join("\n");
    assert!(display.contains("GitHub PR feedback (1)"));
    assert!(display.contains("check failed · unit tests"));
    assert!(display.contains("mark"));
    key(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    assert!(app.pr_feedback_selection.contains("check:unit tests"));
    // This fixture has no clean review to import into, so sending keeps
    // the selection and explains the prerequisite instead of guessing.
    key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(matches!(app.engine.view, View::PrFeedback(0)));
    assert!(app.pr_feedback_selection.contains("check:unit tests"));
    assert!(
        app.engine
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("clean current review"))
    );
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(app.engine.view, View::Chat));
    assert!(app.pr_feedback_selection.is_empty());
}

#[test]
fn workspace_task_details_keep_readable_previous_findings_and_reports() {
    use ratatui::style::Color;
    let (_data, mut app, _) = execution_app();
    let m = &mut app.engine.mods[0];
    let plan = m.planning.as_mut().unwrap().plan.as_mut().unwrap();
    plan.tasks[0].title = "Implement the durable browser repository".into();
    let mut task = plan.tasks[0].clone();
    task.id = "interface".into();
    task.title = "Connect the UI to local storage".into();
    task.checks = vec![
        "Reconnect storage after the owning worker fails.".into(),
        "Preserve drafts, selection, focus and retry state while rejecting stale edits and embeddings from another tab.".into(),
        "Keyboard access and narrow layouts remain usable.".into(),
    ];
    plan.tasks.push(task.clone());
    let e = m.execution.as_mut().unwrap();
    e.status = "running".into();
    e.checks.clear();
    e.fingerprint = None;
    let run = &mut e.tasks[0];
    let id = run.id;
    run.status = "pending".into();
    run.checks.clear();
    let findings = serde_json::json!([
        {"owner":"database", "file":"app/static/todo-store.mjs", "line":178,
         "priority":"P1", "title":"Restore the change sequence after owner replacement",
         "evidence":"After reopening, the worker returns sequence 0. A surviving tab keeps sequence 4 and rejects every refreshed list as stale.",
         "fix":"Persist and restore the change sequence before accepting requests from surviving tabs."},
        {"owner":"database", "file":"app/static/database-coordinator.mjs", "line":241,
         "priority":"P2", "title":"Preserve revision tokens across database reopen",
         "evidence":"Reopening assigns fresh revision tokens to unchanged tasks. Existing mutation receipts still contain the original tokens.",
         "fix":"Store revision tokens with each row and reuse them after reopening."}
    ]);
    run.review_feedback = Some(format!(
        "Independent review findings: {findings}\nAddress only these defects within your existing scope."
    ));
    let mut done = run.clone();
    done.id += 1;
    done.task_id = task.id;
    done.source = crate::store::task_source(done.id, 1);
    done.status = "done".into();
    done.review_feedback = None;
    done.checks = task
        .checks
        .iter()
        .map(|check| crate::execution::CheckResult {
            timeout_seconds: None,
            duration_ms: None,
            missing_runtime: None,
            task: None,
            check: check.clone(),
            command: vec!["/usr/bin/true".into()],
            exit_code: Some(0),
            output: String::new(),
        })
        .collect();
    let done_id = done.id;
    let summary = "Fixed storage reconnection after owner crashes while preserving drafts and operation IDs. Healthy quota recovery retains its worker.";
    m.messages.push(crate::store::Message {
        source: Some(done.source.clone()),
        task: Some(done_id),
        item_id: Some(format!("result:{}", done.source)),
        role: "codex:66".into(),
        body: format!("✓ task complete · {}\n{summary}", task.title),
        model: Some("gpt-6.1-sol".into()),
        effort: Some("xhigh".into()),
    });
    e.tasks.push(done);
    screen(&mut app, 160, 90);
    app.activate_control(Control::Drill(Key::Task(id))).unwrap();
    for width in [160, 124, 80] {
        let buffer = screen(&mut app, width, 90);
        let text = rows(&buffer).join("\n");
        assert!(text.contains("Saved review") && text.contains("Evidence"));
        assert!(!text.contains("\"owner\"") && !text.contains("Address only"));
        assert!(
            buffer
                .content
                .iter()
                .any(|cell| cell.symbol() == "P" && cell.fg == Color::Red)
        );
    }
    app.activate_control(Control::Drill(Key::Task(done_id)))
        .unwrap();
    let buffer = screen(&mut app, 160, 70);
    let text = rows(&buffer).join("\n");
    assert!(text.contains("Worker report") && text.contains("3 passed"));
    assert!(!text.contains("task complete") && !text.contains("0 failed"));
    let pane = app.workspace.inspector.area;
    assert!(
        (pane.y..pane.bottom()).any(|y| buffer[(pane.x + 2, y)].symbol() == "✓"
            && buffer[(pane.x + 2, y)].fg == Color::Green)
    );
    app.engine.view = View::Task(done_id, 0);
    let text = rows(&overview_screen(&mut app, 80, 46)).join("\n");
    assert!(text.contains("Worker report") && text.contains("3 passed"));
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn agents_display_persisted_operation_timings_with_the_draft_intact() {
    let (_data, mut app, _) = execution_app();
    let mod_id = app.engine.mods[0].id;
    app.engine
        .store
        .record_timing(
            mod_id,
            0,
            &crate::diagnostics::timing::Observation {
                phase: crate::diagnostics::timing::Phase::Boot,
                duration_ms: 1234,
                source: "observed-source".into(),
                environment: Some("sha256:observed-guest".into()),
                completed: true,
                attempt: None,
            },
        )
        .unwrap();
    app.engine.mods[0].diagnostics = app.engine.store.diagnostics(mod_id).unwrap();
    screen(&mut app, 160, 44);
    click(&mut app, Control::Tab(WorkspaceView::Agents));
    app.show_evidence = true;
    let output = rows(&screen(&mut app, 160, 44)).join("\n");
    assert!(output.contains("VM boot: 1.234s"), "{output}");
    assert!(output.contains("Recorded setup timings"));
    assert_eq!(app.workspace.inspector.tab, WorkspaceView::Agents);
    assert_eq!(app.input.lines(), ["keep this draft"]);
    assert!(queued(&app).is_empty());
}
