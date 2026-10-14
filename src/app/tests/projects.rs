use super::*;
use crate::app::controls::Control;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

fn fixture() -> (TestData, App, i64, i64) {
    let data = TestData::new();
    let a = test_project(&data, "todo").canonicalize().unwrap();
    let b = test_project(&data, "notes").canonicalize().unwrap();
    let mut store = data.store();
    let first = store.load_project(&a).unwrap().id;
    let second = store.load_project(&b).unwrap().id;
    for (project, title) in [
        (first, "Add PDF search"),
        (first, "Improve onboarding"),
        (second, "Search notes"),
    ] {
        let m = store.create_mod(project, title).unwrap();
        store
            .0
            .execute(
                "UPDATE code_mods SET name=?1 WHERE id=?2",
                rusqlite::params![title, m.id],
            )
            .unwrap();
    }
    (data, App::load(a, false, store).unwrap(), first, second)
}

fn click(app: &mut App, control: Control) {
    screen(app, 100, 36);
    let area = app
        .controls
        .hits
        .iter()
        .find(|(_, item)| *item == control)
        .unwrap()
        .0;
    app.handle(Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x,
        row: area.y,
        modifiers: KeyModifiers::NONE,
    }))
    .unwrap();
}

#[test]
fn terminal_project_accordion_mouse_and_keys_preserve_workspace_and_caret() {
    let (_data, mut app, a, b) = fixture();
    paste(&mut app, "Keep this draft");
    key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    let cursor = app.input.cursor();
    let active = app.current_mod().unwrap().id;
    key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
    click(&mut app, Control::PickMod(0));
    assert_eq!(app.expanded_project, None);
    assert_eq!(app.current_mod().unwrap().id, active);
    assert_eq!(app.input.cursor(), cursor);
    click(&mut app, Control::PickMod(1));
    assert_eq!(app.expanded_project, Some(b));
    assert_eq!(app.project_id, a);
    assert_eq!(app.input.lines(), ["Keep this draft"]);
    click(&mut app, Control::PickMod(2));
    assert_eq!(app.project_id, b);
    assert_eq!(app.current_mod().unwrap().name, "Search notes");
    key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
    click(&mut app, Control::ProjectNew(a));
    assert_eq!(app.project_id, a);
    assert!(app.view == View::NewMod);
    paste(&mut app, "A creation");
    key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
    click(&mut app, Control::ProjectNew(b));
    paste(&mut app, "B creation");
    key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
    click(&mut app, Control::ProjectNew(a));
    assert_eq!(app.input.lines(), ["A creation"]);
    assert_eq!(app.store.creation_draft(b).unwrap(), "B creation");
}

#[test]
fn terminal_open_project_error_keeps_both_inputs_and_ctrl_c_remains_available() {
    let (_data, mut app, _, _) = fixture();
    paste(&mut app, "Unsent instruction");
    key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('+'), KeyModifiers::NONE);
    paste(&mut app, "/missing/sprowt-project");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        app.project_picker_error
            .as_deref()
            .unwrap()
            .contains("Could not open")
    );
    let crate::app::projects::PickerPanel::Open(input) = &app.project_picker else {
        panic!("folder prompt lost");
    };
    assert_eq!(input.lines(), ["/missing/sprowt-project"]);
    assert_eq!(app.input.lines(), ["Unsent instruction"]);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('+'), KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(app.quit);
}

#[test]
fn project_picker_renders_shared_themes_at_narrow_and_wide_sizes() {
    let (_data, mut app, _, _) = fixture();
    app.engine.project = PathBuf::from("/example/todo");
    key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
    for appearance in [
        crate::appearance::Appearance::Light,
        crate::appearance::Appearance::Dark,
    ] {
        app.engine.set_appearance(appearance).unwrap();
        for width in [40, 100] {
            let buffer = screen(&mut app, width, 36);
            let display = rows(&buffer).join("\n");
            assert!(
                display.contains("todo")
                    && display.contains("notes")
                    && display.contains("Add PDF search"),
                "{display}"
            );
            assert!(
                app.controls
                    .hits
                    .iter()
                    .any(|(_, c)| matches!(c, Control::ProjectNew(_)))
            );
            if let Some(path) = std::env::var_os("SPROWT_PROJECT_BUFFERS") {
                let directory = PathBuf::from(path);
                std::fs::create_dir_all(&directory).unwrap();
                let cells: Vec<_> = buffer.content.iter().enumerate().map(|(i,c)| serde_json::json!({"x":i % width as usize,"y":i / width as usize,"symbol":c.symbol(),"fg":format!("{:?}",c.fg),"bg":format!("{:?}",c.bg),"bold":c.modifier.contains(ratatui::style::Modifier::BOLD)})).collect();
                std::fs::write(directory.join(format!("{}-{width}.json", appearance.label().to_lowercase())), serde_json::to_vec(&serde_json::json!({"width":width,"height":buffer.area.height,"cells":cells})).unwrap()).unwrap();
            }
        }
    }
}

#[test]
fn terminal_background_delete_keeps_the_focused_project_row() {
    let (data, mut app, _, b) = fixture();
    paste(&mut app, "Current instruction");
    let removed = app.mods[0].id;
    let root = data.0.join("removed-workspace");
    std::fs::create_dir_all(&root).unwrap();
    app.engine.mods[0].git_root = Some(root);
    app.engine.git_jobs.insert(
        removed,
        Job::start("removing codemod", |_| Ok(git_mod::Result::Removed)),
    );
    app.engine.view = View::Mods(3);
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.mods.iter().any(|m| m.id == removed) {
        assert!(Instant::now() < deadline);
        app.poll_workers().unwrap();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(matches!(app.view, View::Mods(2)));
    assert!(
        matches!(app.project_rows()[2], crate::app::projects::ProjectRow::Project(id) if id == b)
    );
    assert_eq!(app.input.lines(), ["Current instruction"]);
}
