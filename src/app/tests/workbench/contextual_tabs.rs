use super::*;

fn visible_tabs(app: &App) -> Vec<WorkspaceView> {
    app.controls
        .hits
        .iter()
        .filter_map(|(_, control)| {
            if let Control::Tab(tab) = control {
                Some(*tab)
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn tabs_follow_evidence_and_keep_fixed_shortcuts_draft_and_focus() {
    let (_data, mut app, root) = execution_app();
    // The completed source has not been reviewed yet; only saved checks have evidence.
    let text = rows(&screen(&mut app, 80, 50)).join("\n");
    assert_eq!(
        visible_tabs(&app),
        [WorkspaceView::Conversation, WorkspaceView::Checks]
    );
    assert!(text.contains("1 Conversation") && text.contains("3 Checks"));
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('3'), KeyModifiers::NONE);
    assert_eq!(app.workspace.inspector.tab, WorkspaceView::Checks);
    for digit in ['2', '4', '5'] {
        key(&mut app, KeyCode::Char(digit), KeyModifiers::NONE);
        assert_eq!(app.workspace.inspector.tab, WorkspaceView::Checks);
    }
    let cursor = app.input.cursor();
    app.workspace.selected = Some(Key::Task(app.task_ids()[0]));
    let execution = app.engine.mods[0].execution.as_mut().unwrap();
    execution.checks.clear();
    execution.tasks[0].checks.clear();
    screen(&mut app, 40, 50);
    assert_eq!(visible_tabs(&app), [WorkspaceView::Conversation]);
    assert_eq!(app.workspace.inspector.tab, WorkspaceView::Conversation);
    assert_eq!(app.workspace.selected, None);
    assert_eq!(app.workspace.focus, Focus::Content);
    assert_eq!(app.input.lines(), ["keep this draft"]);
    assert_eq!(app.input.cursor(), cursor);
    workspace::review(&root).unwrap();
    screen(&mut app, 80, 50);
    click(&mut app, Control::Tab(WorkspaceView::Changes));
    assert_eq!(app.workspace.inspector.tab, WorkspaceView::Changes);
    std::fs::write(root.join("work/hello.sh"), "old\n").unwrap();
    app.engine.review = Some(workspace::review(&root).unwrap());
    screen(&mut app, 80, 50);
    assert_eq!(visible_tabs(&app), [WorkspaceView::Conversation]);
    assert_eq!(app.workspace.inspector.tab, WorkspaceView::Conversation);
}

#[test]
fn checking_without_results_exposes_real_progress_once() {
    let (_data, mut app, _) = execution_app();
    let execution = app.engine.mods[0].execution.as_mut().unwrap();
    execution.status = "verifying".into();
    execution.checks.clear();
    execution.tasks[0].checks.clear();
    screen(&mut app, 100, 54);
    click(&mut app, Control::Tab(WorkspaceView::Checks));
    let text = rows(&screen(&mut app, 100, 54)).join("\n");
    assert_eq!(text.matches("Combined checks are running.").count(), 1);
    let snapshot = serde_json::to_value(app.engine.snapshot()).unwrap();
    assert_eq!(
        snapshot["active_mod"]["check_activity"],
        serde_json::json!([{
            "task_id": null, "label": "Combined checks are running."
        }])
    );
}
