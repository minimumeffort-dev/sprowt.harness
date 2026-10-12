use super::*;
use crate::app::{
    controls::{Control, EvidenceTab},
    timeline::{Focus, Key},
};
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
    let e = app.mods[0].execution.as_mut().unwrap();
    e.status = "blocked".into();
    e.checks[0].exit_code = Some(1);
    e.checks[0].output = "Expected an empty name to exit nonzero; received exit 0.\nhello".into();
    e.checks[0].duration_ms = Some(82);
    e.checks[0].task = Some(e.tasks[0].id);
    (data, app, root)
}

fn setup_failed_app() -> (TestData, App, PathBuf, i64) {
    let (data, mut app, root) = execution_app();
    let m = &app.mods[0];
    let id = m.execution.as_ref().unwrap().tasks[0].id;
    let source = crate::store::task_source(id, 2);
    let recipe = crate::environment::Recipe {
        resources: None,
        reusable_paths: vec![],
        inputs: vec![],
        system_packages: vec![],
        setup: vec![crate::environment::Command {
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
            name: "Browser runtime available".into(),
            argv: vec!["/usr/bin/node".into(), "--version".into()],
            timeout_seconds: 5,
        }],
        baseline: vec![],
    };
    let mut plan = m.planning.as_ref().unwrap().plan.clone().unwrap();
    plan.environment = Some(recipe.clone());
    app.store
        .save_plan(m.id, &m.planning.as_ref().unwrap().source, &plan)
        .unwrap();
    app.store.0.execute("UPDATE task_runs SET status='paused',attempt=2,source=?2,summary=?3,checks='[]' WHERE id=?1", rusqlite::params![id,source,"Environment preflight failed: Setup failed: Install browser runtime\nGuest command timed out after 180s."]).unwrap();
    app.store
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
    let project = app.project.clone();
    drop(app);
    let app = App::load(project, false, data.store()).unwrap();
    (data, app, root, id)
}

#[test]
fn workbench_persisted_setup_failure_is_visible_without_a_live_worker() {
    for width in [80, 160] {
        let (_data, mut app, _, id) = setup_failed_app();
        assert!(app.workers.is_empty());
        assert!(
            app.state_summary()
                .error
                .as_deref()
                .is_some_and(|error| error.contains("Guest command timed out after 180s"))
        );
        assert!(app.inspect_task(id).unwrap().checks.is_empty());
        assert!(app.mods[0].execution.as_ref().unwrap().checks.is_empty());
        assert!(
            app.timeline_items().iter().any(
                |item| item.key == Key::Environment && item.state == timeline::State::Attention
            )
        );
        screen(&mut app, width, 64);
        app.activate_control(Control::Stage(Key::Build)).unwrap();
        app.activate_control(Control::Tab(EvidenceTab::Checks))
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
        assert!(matches!(app.view, View::Task(task, 0) if task == id));
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }
}

#[test]
fn workbench_preparation_full_evidence_includes_the_output_tail() {
    let (_data, mut app, _, id) = setup_failed_app();
    app.view = View::Task(id, 0);
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
    assert!(matches!(app.view, View::Task(task, 0) if task == id));
    assert_eq!(app.inspect_task(id).unwrap().run.source, source);
    assert_eq!(app.inspect_task(id).unwrap().run.status, "paused");
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn workbench_previous_preparation_never_becomes_a_new_attempt_failure() {
    let (_data, mut app, _, id) = setup_failed_app();
    let next_source = crate::store::task_source(id, 3);
    app.store.0.execute("UPDATE task_runs SET status='pending',attempt=3,source=?2,summary='',checks='[]' WHERE id=?1", rusqlite::params![id,next_source]).unwrap();
    app.mods[0].execution = app.store.execution(app.mods[0].id).unwrap();
    assert!(app.state_summary().error.is_none());
    assert!(
        !app.timeline_items()
            .iter()
            .any(|item| item.key == Key::Environment && item.state == timeline::State::Attention)
    );
    app.view = View::Task(id, 0);
    let saved = rows(&screen(&mut app, 160, 70)).join("\n");
    assert!(saved.contains("Earlier preparation"), "{saved}");
    assert!(saved.contains("Guest command timed out after 180s"));
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn workbench_active_worker_takes_precedence_over_saved_preparation_failure() {
    let (_data, mut app, _, id) = setup_failed_app();
    let record = app.store.worker(app.mods[0].id).unwrap();
    let mut worker =
        Worker::start(&app.project, &app.mods[0], record, Role::Planner, None).unwrap();
    worker.role = Role::Executor;
    worker.status = Status::Starting;
    worker.enabled = true;
    worker.bind_test_task(app.inspect_task(id).unwrap().run.source.clone());
    let run = &mut app.mods[0].execution.as_mut().unwrap().tasks[0];
    run.worker = Some(worker.id);
    run.status = "running".into();
    app.workers.insert(worker.id, worker);
    assert_eq!(app.inspect_task(id).unwrap().state, "Starting");
    assert!(app.inspect_task(id).unwrap().error.is_none());
    assert!(app.state_summary().error.is_none());
    assert!(
        !app.timeline_items()
            .iter()
            .any(|item| item.key == Key::Environment && item.state == timeline::State::Attention)
    );
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
    app.store
        .finish_task(
            app.mods[0].id,
            &source,
            "paused",
            "Environment preflight failed: Environment preparation stopped.",
            &[],
        )
        .unwrap();
    app.mods[0].execution = app.store.execution(app.mods[0].id).unwrap();
    (data, app, root, id)
}

#[test]
fn workbench_stopped_preparation_is_not_a_failed_environment() {
    let (_data, mut app, _, id) = stopped_setup_app();
    app.mods[0].user_paused = true;
    assert!(app.state_summary().error.is_none());
    assert!(
        !app.timeline_items()
            .iter()
            .any(|item| item.key == Key::Environment && item.state == timeline::State::Attention)
    );
    app.view = View::Task(id, 0);
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
        let record = app.store.worker(app.mods[0].id).unwrap();
        let mut worker =
            Worker::start(&app.project, &app.mods[0], record, Role::Planner, None).unwrap();
        worker.role = Role::Executor;
        worker.status = Status::Failed;
        worker.bind_test_task(source);
        worker.error =
            Some("Environment preflight failed: Environment preparation stopped.".into());
        app.store
            .0
            .execute(
                "UPDATE task_runs SET worker_id=?2 WHERE id=?1",
                rusqlite::params![id, worker.id],
            )
            .unwrap();
        app.mods[0].execution = app.store.execution(app.mods[0].id).unwrap();
        app.store.set_user_paused(app.mods[0].id, paused).unwrap();
        app.mods[0].user_paused = paused;
        app.workers.insert(worker.id, worker);
        assert!(
            app.inspect_task(id).unwrap().error.is_none(),
            "retained worker; paused={paused}"
        );
        assert!(app.state_summary().error.is_none());
        assert!(
            !app.timeline_items().iter().any(
                |item| item.key == Key::Environment && item.state == timeline::State::Attention
            )
        );
        let project = app.project.clone();
        drop(app);
        let mut app = App::load(project, false, data.store()).unwrap();
        assert!(app.workers.is_empty());
        assert!(app.inspect_task(id).unwrap().error.is_none());
        assert!(app.state_summary().error.is_none());
        app.view = View::Task(id, 0);
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
    let record = app.store.worker(app.mods[0].id).unwrap();
    let mut worker =
        Worker::start(&app.project, &app.mods[0], record, Role::Planner, None).unwrap();
    worker.status = Status::Failed;
    worker.error = Some("Planner could not connect to its provider.".into());
    app.workers.insert(worker.id, worker);
    assert!(app.inspect_task(id).unwrap().error.is_none());
    assert_eq!(
        app.state_summary().error.as_deref(),
        Some("Planner could not connect to its provider.")
    );
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
#[ignore = "exports actual Ratatui buffers for visual inspection"]
fn export_workbench_buffers() {
    let directory = std::env::var_os("SPROWT_UI_CAPTURE_DIR").expect("SPROWT_UI_CAPTURE_DIR");
    let directory = PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let (_review_data, mut review) = reviewed_app();
    let (_failed_data, mut failed, _) = failed_app();
    let (_working_data, mut working, _) = execution_app();
    working.mods[0].execution.as_mut().unwrap().status = "running".into();
    working.mods[0].execution.as_mut().unwrap().checks.clear();
    working.mods[0].execution.as_mut().unwrap().tasks[0].status = "running".into();
    working.mods[0].execution.as_mut().unwrap().tasks[0]
        .checks
        .clear();
    let (_idle_data, mut idle, _) = execution_app();
    let (_setup_data, mut setup, _, _) = setup_failed_app();
    let record = working.store.worker(working.mods[0].id).unwrap();
    let mut worker = Worker::start(
        &working.project,
        &working.mods[0],
        record,
        Role::Planner,
        None,
    )
    .unwrap();
    worker.role = Role::Executor;
    worker.status = Status::Running;
    worker.bind_test_task(
        working.mods[0].execution.as_ref().unwrap().tasks[0]
            .source
            .clone(),
    );
    working.mods[0].execution.as_mut().unwrap().tasks[0].worker = Some(worker.id);
    working.workers.insert(worker.id, worker);
    for (name, app, selected) in [
        ("review", &mut review, Key::Review),
        ("failed-check", &mut failed, Key::Checks),
        ("working", &mut working, Key::Build),
        ("idle", &mut idle, Key::Plan),
        ("setup-failure", &mut setup, Key::Build),
    ] {
        app.project = PathBuf::from("/example/project");
        for (suffix, width, height) in [("desktop", 180, 52), ("narrow", 80, 42), ("small", 36, 36)]
        {
            screen(app, width, height);
            app.activate_control(Control::Stage(selected)).unwrap();
            app.timeline.focus = Focus::Inspector;
            if name == "setup-failure" {
                app.activate_control(Control::Tab(EvidenceTab::Checks))
                    .unwrap();
            }
            let buffer = screen(app, width, height);
            let cells: Vec<_> = buffer.content.iter().enumerate().map(|(index, cell)| serde_json::json!({"x": index % width as usize, "y": index / width as usize, "symbol": cell.symbol(), "fg": format!("{:?}", cell.fg), "bg": format!("{:?}", cell.bg), "bold": cell.modifier.contains(ratatui::style::Modifier::BOLD)})).collect();
            std::fs::write(
                directory.join(format!("{name}-{suffix}.json")),
                serde_json::to_vec(
                    &serde_json::json!({"width": width, "height": height, "cells": cells}),
                )
                .unwrap(),
            )
            .unwrap();
        }
    }
}

#[test]
fn workbench_tabs_preserve_draft_and_independent_offsets() {
    let (_data, mut app, _) = execution_app();
    app.mods[0]
        .planning
        .as_mut()
        .unwrap()
        .plan
        .as_mut()
        .unwrap()
        .contracts = (0..60)
        .map(|i| format!("Contract {i}: retain the accepted result."))
        .collect();
    screen(&mut app, 160, 44);
    click(&mut app, Control::Stage(Key::Plan));
    app.show_evidence = true;
    screen(&mut app, 160, 44);
    let area = app.timeline.inspector.area;
    wheel(&mut app, false, area);
    screen(&mut app, 160, 44);
    assert_eq!(app.timeline.inspector.offset, 3);
    let top = app.timeline.top;
    click(&mut app, Control::Tab(EvidenceTab::Checks));
    screen(&mut app, 160, 44);
    assert_eq!(app.timeline.inspector.offset, 0);
    key(&mut app, KeyCode::Char('1'), KeyModifiers::NONE);
    screen(&mut app, 160, 44);
    assert_eq!(app.timeline.inspector.offset, 3);
    assert_eq!(app.timeline.top, top);
    assert!(app.timeline.held);
    assert_eq!(app.input.lines(), ["keep this draft"]);
    assert!(queued(&app).is_empty());
    click(&mut app, Control::Stage(Key::Build));
    assert_eq!(app.timeline.inspector.offset, 0);
    assert_eq!(app.timeline.inspector.tab, EvidenceTab::Summary);
}

#[test]
fn activity_displays_persisted_operation_timings_with_the_draft_intact() {
    let (_data, mut app, _) = execution_app();
    let mod_id = app.mods[0].id;
    app.store
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
    app.mods[0].diagnostics = app.store.diagnostics(mod_id).unwrap();
    screen(&mut app, 160, 44);
    click(&mut app, Control::Stage(Key::Build));
    screen(&mut app, 160, 44);
    click(&mut app, Control::Tab(EvidenceTab::Activity));
    let output = rows(&screen(&mut app, 160, 44)).join("\n");
    assert!(output.contains("VM boot: 1.234s"), "{output}");
    assert!(output.contains("Recorded setup timings"));
    assert_eq!(app.timeline.inspector.tab, EvidenceTab::Activity);
    assert_eq!(app.input.lines(), ["keep this draft"]);
    assert!(queued(&app).is_empty());
}

#[test]
fn workbench_selected_repair_keeps_successful_evidence_after_completion() {
    for width in [80, 160] {
        let (_data, mut app, _) = execution_app();
        let execution = app.mods[0].execution.as_mut().unwrap();
        execution.status = "running".into();
        execution.checks.clear();
        let run = &mut execution.tasks[0];
        let source = run.source.clone();
        run.status = "checking".into();
        run.check_repair = true;
        run.checks[0].exit_code = Some(1);
        run.checks[0].output = "Repair has not passed yet".into();
        let mut passed = run.checks.clone();
        passed[0].exit_code = Some(0);
        passed[0].output = "Repair passed".into();
        screen(&mut app, width, 46);
        app.timeline.expanded.insert(Key::Build, true);
        screen(&mut app, width, 46);
        let selected = app
            .navigation_items()
            .iter()
            .find(|item| item.title.starts_with("Repair 1."))
            .unwrap()
            .key;
        click(&mut app, Control::Stage(selected));
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        screen(&mut app, width, 46);
        click(&mut app, Control::Tab(EvidenceTab::Checks));
        app.show_evidence = true;
        let before = rows(&screen(&mut app, width, 46)).join("\n");
        assert!(before.contains("Repair has not passed yet"));

        let mod_id = app.mods[0].id;
        app.store
            .finish_task(mod_id, &source, "done", "Repair passed", &passed)
            .unwrap();
        app.mods[0].execution = app.store.execution(mod_id).unwrap();
        let after = rows(&screen(&mut app, width, 46)).join("\n");
        assert!(after.contains("✓ prints hello"), "{width}:\n{after}");
        assert!(after.contains("Repair passed"));
        assert!(!after.contains("no longer in the current plan"));
        assert_eq!(app.timeline.selected, Some(selected));
        assert_eq!(app.timeline.inspector.key, Some(selected));
        assert_eq!(app.timeline.inspector.tab, EvidenceTab::Checks);
        assert_eq!(app.timeline.focus, Focus::Inspector);
        assert!(app.timeline.held);
        assert_eq!(app.input.lines(), ["keep this draft"]);
        assert!(queued(&app).is_empty());

        key(&mut app, KeyCode::Char('1'), KeyModifiers::NONE);
        let summary = rows(&screen(&mut app, width, 46)).join("\n");
        assert!(summary.contains("Greeting") && summary.contains("Done"));
        assert_eq!(app.timeline.selected, Some(selected));
    }
}

#[test]
fn overview_inspected_repair_keeps_result_when_implementation_finishes() {
    let (_data, mut app, _) = execution_app();
    let execution = app.mods[0].execution.as_mut().unwrap();
    execution.status = "running".into();
    execution.checks.clear();
    let run = &mut execution.tasks[0];
    let source = run.source.clone();
    let id = run.id;
    run.status = "checking".into();
    run.check_repair = true;
    run.checks[0].exit_code = Some(1);
    run.checks[0].output = "Repair has not passed yet".into();
    let mut passed = run.checks.clone();
    passed[0].exit_code = Some(0);
    passed[0].output = "Repair passed".into();
    let record = app.store.worker(app.mods[0].id).unwrap();
    let mut worker =
        Worker::start(&app.project, &app.mods[0], record, Role::Planner, None).unwrap();
    worker.role = Role::Executor;
    worker.status = Status::Checking;
    worker.bind_test_task(source.clone());
    app.mods[0].execution.as_mut().unwrap().tasks[0].worker = Some(worker.id);
    app.workers.insert(worker.id, worker);
    let following = rows(&overview_screen(&mut app, 160, 46)).join("\n");
    assert_eq!(app.timeline.selected, Some(Key::Task(id)), "{following}");
    assert!(!app.timeline.held);
    assert!(!app.timeline.expanded.contains_key(&Key::Build));
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    app.show_evidence = true;
    let before = rows(&overview_screen(&mut app, 160, 46)).join("\n");
    assert!(before.contains("Repair has not passed yet"));
    assert_eq!(app.timeline.focus, Focus::Inspector);
    assert!(app.timeline.held);

    let mod_id = app.mods[0].id;
    app.store
        .finish_task(mod_id, &source, "done", "Repair passed", &passed)
        .unwrap();
    app.mods[0].execution = app.store.execution(mod_id).unwrap();
    app.workers.values_mut().next().unwrap().status = Status::Complete;
    let after = rows(&overview_screen(&mut app, 160, 46)).join("\n");
    assert!(after.contains("Repair passed"), "{after}");
    assert!(after.contains("✓ prints hello"));
    assert!(!after.contains("no longer in the current plan"));
    assert_eq!(app.timeline.selected, Some(Key::Task(id)));
    assert_eq!(app.timeline.focus, Focus::Inspector);
    assert!(!app.timeline.expanded.contains_key(&Key::Build));
    assert_eq!(app.input.lines(), ["keep this draft"]);

    let mut peer = app.mods[0]
        .planning
        .as_ref()
        .unwrap()
        .plan
        .as_ref()
        .unwrap()
        .tasks[0]
        .clone();
    peer.id = "completed-peer".into();
    peer.title = "Another completed task".into();
    let mut peer_run = app.mods[0].execution.as_ref().unwrap().tasks[0].clone();
    peer_run.id += 1;
    peer_run.task_id = peer.id.clone();
    peer_run.source = crate::store::task_source(peer_run.id, 1);
    app.mods[0]
        .planning
        .as_mut()
        .unwrap()
        .plan
        .as_mut()
        .unwrap()
        .tasks
        .push(peer);
    app.mods[0].execution.as_mut().unwrap().tasks.push(peer_run);
    app.timeline.expanded.insert(Key::Build, false);
    let collapsed = rows(&overview_screen(&mut app, 160, 46)).join("\n");
    assert!(collapsed.contains("Repair passed") && collapsed.contains("✓ prints hello"));
    assert!(!collapsed.contains("Another completed task"));
    assert_eq!(app.timeline.expanded.get(&Key::Build), Some(&false));

    key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE);
    screen(&mut app, 160, 46);
    click(&mut app, Control::Tab(EvidenceTab::Checks));
    let checks = rows(&screen(&mut app, 160, 46)).join("\n");
    assert!(checks.contains("✓ prints hello") && checks.contains("Repair passed"));
    assert_eq!(app.timeline.selected, Some(Key::Task(id)));
}

#[test]
fn workbench_narrow_tabs_and_task_drilldown_are_accessible() {
    let (_data, mut app, _) = execution_app();
    let id = app.mods[0].execution.as_ref().unwrap().tasks[0].id;
    for width in [36, 48, 80, 104, 160] {
        screen(&mut app, width, 46);
        app.activate_control(Control::Stage(Key::Build)).unwrap();
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let text = rows(&screen(&mut app, width, 46)).join("\n");
        for tab in EvidenceTab::ALL {
            assert!(
                text.contains(tab.label()),
                "{width}: {}\n{text}",
                tab.label()
            );
            assert!(
                app.controls
                    .hits
                    .iter()
                    .any(|(_, control)| *control == Control::Tab(tab))
            );
        }
        click(&mut app, Control::Drill(Key::Task(id)));
        let text = rows(&screen(&mut app, width, 46)).join("\n");
        assert_eq!(app.timeline.selected, Some(Key::Task(id)));
        assert_eq!(app.timeline.focus, Focus::Inspector);
        assert!(text.contains("Print hello"));
        if width < 104 {
            assert!(app.scroll.content.is_empty());
            click(&mut app, Control::Back);
            screen(&mut app, width, 46);
            assert!(!app.scroll.content.is_empty());
        }
    }
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn workbench_mouse_submit_uses_composer_even_with_evidence_focus() {
    let (_data, mut app, _) = execution_app();
    screen(&mut app, 160, 44);
    app.timeline.focus = Focus::Inspector;
    click(&mut app, Control::Submit);
    assert_eq!(queued(&app), ["keep this draft"]);
    assert!(app.input.is_empty());
    assert_eq!(app.timeline.focus, Focus::Composer);
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
    assert!(app.executing_mods.contains(&app.mods[0].id));
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
    assert!(app.view == View::Chat);
    screen(&mut app, 100, 42);
    click(&mut app, Control::Action(Action::Mods));
    screen(&mut app, 100, 42);
    click(&mut app, Control::PickMod(1));
    assert!(app.view == View::NewMod);
    assert_eq!(app.mods[0].draft, "keep this draft");
}

#[test]
fn workbench_header_status_aligns_with_the_dock_and_reports_pending_work() {
    let (_data, mut app, _) = execution_app();
    for width in [80, 104, 160, 200] {
        let text = rows(&screen(&mut app, width, 42));
        let row = text
            .iter()
            .find(|r| r.contains("Work saved locally"))
            .unwrap();
        assert_eq!(row.trim_end().chars().count(), width as usize - 4);
        assert!(row.contains("sprowt harness"));
    }
    app.input.insert_str(" changed");
    assert_eq!(app.local_status(), "Saving draft…");
    app.auto_runs.insert(app.mods[0].id);
    assert_eq!(app.local_status(), "Starting work…");
    app.view = View::NewMod;
    assert_eq!(app.local_status(), "New codemod · not submitted");
}

#[test]
fn workbench_global_shortcuts_are_not_consumed_by_pane_controls() {
    let (_data, mut app, _) = execution_app();
    for focus in [Focus::Timeline, Focus::Inspector] {
        screen(&mut app, 160, 44);
        app.timeline.focus = focus;
        assert!(
            !app.timeline_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL))
                .unwrap()
        );
        assert!(
            !app.timeline_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::CONTROL))
                .unwrap()
        );
        assert!(!app.show_evidence);
        app.timeline.focus = focus;
        key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert!(!matches!(app.view, View::Chat));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    }
}

#[test]
fn workbench_stage_scroll_does_not_follow_the_selected_task_back() {
    let (_data, mut app, _) = execution_app();
    let original = app.mods[0]
        .planning
        .as_ref()
        .unwrap()
        .plan
        .as_ref()
        .unwrap()
        .tasks[0]
        .clone();
    for index in 1..20 {
        let mut next = original.clone();
        next.id = format!("task-{index}");
        next.title = format!("Additional task {index}");
        app.mods[0]
            .planning
            .as_mut()
            .unwrap()
            .plan
            .as_mut()
            .unwrap()
            .tasks
            .push(next);
    }
    screen(&mut app, 160, 42);
    app.activate_control(Control::Stage(Key::Build)).unwrap();
    app.activate_control(Control::Expand).unwrap();
    screen(&mut app, 160, 42);
    let selected = app.timeline.selected;
    let mut previous = app.timeline.top;
    for _ in 0..4 {
        let area = app.scroll.content;
        wheel(&mut app, false, area);
        screen(&mut app, 160, 42);
        assert!(app.timeline.top > previous);
        previous = app.timeline.top;
        assert_eq!(app.timeline.selected, selected);
    }
    click(&mut app, Control::Current);
    screen(&mut app, 160, 42);
    assert!(!app.timeline.held);
    assert_eq!(app.timeline.inspector.offset, 0);
}

#[test]
fn workbench_collapsed_implementation_retains_blocked_task_attention() {
    let (_data, mut app, _) = execution_app();
    let execution = app.mods[0].execution.as_mut().unwrap();
    execution.status = "blocked".into();
    execution.checks.clear();
    execution.tasks[0].status = "blocked".into();
    execution.tasks[0].summary = "The environment could not start.".into();
    execution.tasks[0].checks.clear();
    let id = execution.tasks[0].id;
    screen(&mut app, 160, 44);
    app.timeline.expanded.insert(Key::Build, false);
    let items = app.navigation_items();
    assert!(!items.iter().any(|item| item.branch));
    assert!(
        items
            .iter()
            .any(|item| item.key == Key::Build && item.state == timeline::State::Attention)
    );
    app.activate_control(Control::Stage(Key::Build)).unwrap();
    screen(&mut app, 160, 44);
    assert!(
        app.controls
            .hits
            .iter()
            .any(|(_, control)| *control == Control::Drill(Key::Task(id)))
    );
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn workbench_review_opens_unresolved_findings_and_keeps_selected_identity() {
    let (_data, mut app) = reviewed_app();
    let review = app.mods[0].agent_review.as_mut().unwrap();
    let mut open = review.issues[0].clone();
    open.id = 2;
    open.finding.id = Some(2);
    open.finding.title = "Preserve named greetings".into();
    review.issues[0].status = "resolved".into();
    review.issues.push(open);
    screen(&mut app, 160, 50);
    app.activate_control(Control::Stage(Key::Review)).unwrap();
    screen(&mut app, 160, 50);
    assert_eq!(app.review_finding_ids(), [2, 1]);
    assert_eq!(app.timeline.inspector.finding, Some(2));
    click(&mut app, Control::Finding(1));
    screen(&mut app, 160, 50);
    assert_eq!(app.timeline.inspector.finding, Some(1));
    let review = app.mods[0].agent_review.as_mut().unwrap();
    let mut new_issue = review.issues[1].clone();
    new_issue.id = 3;
    new_issue.finding.id = Some(3);
    new_issue.finding.title = "Reject whitespace names".into();
    review.issues.push(new_issue);
    screen(&mut app, 160, 50);
    assert_eq!(app.review_finding_ids(), [2, 3, 1]);
    assert_eq!(app.timeline.inspector.finding, Some(1));
    key(&mut app, KeyCode::Char('['), KeyModifiers::NONE);
    assert_eq!(app.timeline.inspector.finding, Some(3));
    key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE);
    assert_eq!(app.timeline.inspector.finding, Some(1));
    key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE);
    assert_eq!(app.timeline.inspector.finding, Some(1));
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn workbench_new_codemod_newline_remains_editable_with_a_closed_selection() {
    let (_data, mut app, _) = execution_app();
    app.mods[0].closed = true;
    app.view = View::NewMod;
    app.input.clear();
    app.input.insert_str("New request");
    screen(&mut app, 100, 42);
    click(&mut app, Control::Newline);
    assert_eq!(app.input.lines(), ["New request", ""]);
    assert_eq!(app.timeline.focus, Focus::Composer);
}

#[test]
fn workbench_environment_receipts_open_tasks_when_review_needs_attention() {
    let (_data, mut app) = reviewed_app();
    app.mods[0]
        .planning
        .as_mut()
        .unwrap()
        .plan
        .as_mut()
        .unwrap()
        .environment = Some(crate::environment::Recipe {
        resources: None,
        reusable_paths: vec![],
        inputs: Vec::new(),
        system_packages: Vec::new(),
        setup: Vec::new(),
        baseline: Vec::new(),
        capabilities: vec![crate::environment::Command {
            name: "Shell available".into(),
            argv: vec!["/bin/sh".into(), "--version".into()],
            timeout_seconds: 5,
        }],
    });
    screen(&mut app, 160, 46);
    app.activate_control(Control::Stage(Key::Environment))
        .unwrap();
    screen(&mut app, 160, 46);
    click(&mut app, Control::FullEvidence);
    assert!(matches!(app.view, View::Tasks(0)));
    assert_eq!(app.input.lines(), ["keep this draft"]);
}
