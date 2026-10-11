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
    ] {
        app.project = PathBuf::from("/example/project");
        for (suffix, width, height) in [("desktop", 180, 52), ("narrow", 80, 42), ("small", 36, 36)]
        {
            screen(app, width, height);
            app.activate_control(Control::Stage(selected)).unwrap();
            app.timeline.focus = Focus::Inspector;
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
