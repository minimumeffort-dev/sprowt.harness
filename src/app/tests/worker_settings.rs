use super::*;
use crate::worker_settings::{
    Provider,
    tests::{catalog, team},
};

fn prepare(app: &mut App) {
    app.engine.models.set_fixture(catalog());
}

#[test]
fn workers_menu_keys_save_cancel_and_preserve_creation_and_codemod_drafts() {
    let data = TestData::new();
    let project = test_project(&data, "workers");
    let mut app = App::load(project.clone(), false, data.store()).unwrap();
    paste(&mut app, "Creation draft");
    key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    let cursor = app.input.cursor();
    app.engine
        .store
        .save_worker_settings(None, app.engine.project_id, &team())
        .unwrap();
    prepare(&mut app);
    key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('w'), KeyModifiers::NONE);
    assert!(app.worker_settings_editor.is_some());
    paste(&mut app, "Do not replace the draft");
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(app.worker_settings_editor.is_none());
    assert_eq!(app.input.lines(), ["Creation draft"]);
    assert_eq!(app.input.cursor(), cursor);
    let saved = app
        .engine
        .store
        .team_draft(app.engine.project_id)
        .unwrap()
        .unwrap();
    assert_eq!(saved.planner.provider, Provider::Muse);
    assert_eq!(saved.planner.effort, "xhigh");
    assert_eq!(saved.capacity(), 3);
    prepare(&mut app);
    app.open_worker_settings().unwrap();
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(
        app.engine.store.team_draft(app.engine.project_id).unwrap(),
        Some(saved.clone())
    );
    assert_eq!(app.input.cursor(), cursor);
    drop(app);
    let app = App::load(project, false, data.store()).unwrap();
    assert_eq!(
        app.engine.store.team_draft(app.engine.project_id).unwrap(),
        Some(saved)
    );

    let (_data, mut app, _) = execution_app();
    let id = app.engine.current_mod().unwrap().id;
    app.engine
        .store
        .save_worker_settings(Some(id), app.engine.project_id, &team())
        .unwrap();
    prepare(&mut app);
    let cursor = app.input.cursor();
    app.open_worker_settings().unwrap();
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    assert_eq!(
        app.engine
            .store
            .worker_settings(id)
            .unwrap()
            .unwrap()
            .capacity(),
        4
    );
    assert_eq!(app.input.lines(), ["keep this draft"]);
    assert_eq!(app.input.cursor(), cursor);
}

#[test]
fn workers_dialog_keeps_selected_row_and_controls_visible_on_short_terminals() {
    let (_data, mut app, _) = execution_app();
    prepare(&mut app);
    app.worker_settings_editor = Some(crate::app::worker_settings::Editor {
        settings: team(),
        mod_id: app.current_mod().map(|m| m.id),
        row: 3,
        field: 1,
        error: None,
    });
    for (width, height) in [(80, 16), (100, 32)] {
        let buffer = screen(&mut app, width, height);
        let text = rows(&buffer).join("\n");
        assert!(text.contains("Reviewer"), "{text}");
        assert!(text.contains("save") && text.contains("cancel"), "{text}");
        if let Some(dir) = std::env::var_os("SPROWT_WORKERS_PREVIEW") {
            let dir = PathBuf::from(dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("workers-{width}-{height}.txt")), text).unwrap();
        }
    }
    app.worker_settings_editor
        .as_mut()
        .unwrap()
        .settings
        .executors[0]
        .count = 16;
    key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(app.worker_settings_editor.as_ref().unwrap().error.is_some());
    assert_eq!(app.input.lines(), ["keep this draft"]);
}
