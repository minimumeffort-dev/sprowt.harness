use super::*;
use crate::app::controls::Control;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

fn fixture() -> (TestData, App, i64, i64, i64) {
    let data = TestData::new();
    let project = test_project(&data, "project");
    let mut store = data.store();
    let project_id = store.load_project(&project).unwrap().id;
    let first = store.create_mod(project_id, "First change").unwrap().id;
    let second = store.create_mod(project_id, "Second change").unwrap().id;
    let third = store.create_mod(project_id, "Third change").unwrap().id;
    store.select_mod(project_id, first).unwrap();
    (
        data,
        App::load(project, false, store).unwrap(),
        first,
        second,
        third,
    )
}

fn next_mod(app: &mut App) {
    key(app, KeyCode::Char('p'), KeyModifiers::CONTROL);
    key(app, KeyCode::Down, KeyModifiers::NONE);
    key(app, KeyCode::Enter, KeyModifiers::NONE);
}

#[test]
fn terminal_navigation_keys_preserve_drafts_creation_and_selection_branches() {
    let (_data, mut app, first, second, third) = fixture();
    paste(&mut app, "First draft");
    next_mod(&mut app);
    assert_eq!(app.current_mod().unwrap().id, second);
    paste(&mut app, "Second draft");
    next_mod(&mut app);
    assert_eq!(app.current_mod().unwrap().id, third);
    key(&mut app, KeyCode::Left, KeyModifiers::ALT);
    assert_eq!(app.input.lines(), ["Second draft"]);
    key(&mut app, KeyCode::Right, KeyModifiers::ALT);
    assert_eq!(app.current_mod().unwrap().id, third);
    key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(app.engine.view == View::NewMod);
    paste(&mut app, "Unsent **new change**\nwith details");
    key(&mut app, KeyCode::Left, KeyModifiers::ALT);
    assert_eq!(app.current_mod().unwrap().id, third);
    key(&mut app, KeyCode::Right, KeyModifiers::ALT);
    assert_eq!(app.input.lines(), ["Unsent **new change**", "with details"]);
    key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.engine.view == View::NewMod);
    assert_eq!(app.input.lines(), ["Unsent **new change**", "with details"]);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.current_mod().unwrap().id, third);
    key(&mut app, KeyCode::Left, KeyModifiers::ALT);
    assert!(app.engine.view == View::NewMod);
    key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.current_mod().unwrap().id, first);
    assert_eq!(app.input.lines(), ["First draft"]);
    assert!(!app.navigation_snapshot().can_go_forward);
    assert_eq!(app.engine.mods.len(), 3);
    assert!(app.engine.workers.is_empty() && app.engine.git_jobs.is_empty());
}

#[test]
fn terminal_navigation_controls_render_and_click_at_narrow_and_wide_sizes() {
    for width in [40, 80, 160] {
        let (_data, mut app, first, second, _) = fixture();
        next_mod(&mut app);
        next_mod(&mut app);
        key(&mut app, KeyCode::Left, KeyModifiers::ALT);
        let buffer = screen(&mut app, width, 44);
        let rendered = rows(&buffer).join("\n");
        assert!(
            rendered.contains("‹")
                && rendered.contains("Alt←")
                && rendered.contains("›")
                && rendered.contains("Alt→")
        );
        if let Some(directory) = std::env::var_os("SPROWT_UI_CAPTURE_DIR") {
            let directory = PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join(format!("navigation-{width}.txt")), &rendered).unwrap();
            let cells: Vec<_> = buffer.content.iter().enumerate().map(|(i, cell)| serde_json::json!({"x":i % width as usize,"y":i / width as usize,"symbol":cell.symbol(),"fg":format!("{:?}",cell.fg),"bg":format!("{:?}",cell.bg),"bold":cell.modifier.contains(ratatui::style::Modifier::BOLD)})).collect();
            std::fs::write(
                directory.join(format!("navigation-{width}.json")),
                serde_json::to_vec(&serde_json::json!({"width":width,"height":44,"cells":cells}))
                    .unwrap(),
            )
            .unwrap();
        }
        let back = app
            .controls
            .hits
            .iter()
            .find(|(_, c)| *c == Control::NavigateBack)
            .unwrap()
            .0;
        app.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: back.x,
            row: back.y,
            modifiers: KeyModifiers::NONE,
        }))
        .unwrap();
        assert_eq!(app.current_mod().unwrap().id, first);
        screen(&mut app, width, 44);
        let forward = app
            .controls
            .hits
            .iter()
            .find(|(_, c)| *c == Control::NavigateForward)
            .unwrap()
            .0;
        app.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: forward.x,
            row: forward.y,
            modifiers: KeyModifiers::NONE,
        }))
        .unwrap();
        assert_eq!(app.current_mod().unwrap().id, second);
        assert!(app.engine.workers.is_empty());
    }
}

#[test]
fn terminal_navigation_does_not_escape_confirmations_or_consume_plain_arrows() {
    let (_data, mut app, _, second, _) = fixture();
    next_mod(&mut app);
    paste(&mut app, "Keep this draft");
    key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('!'), KeyModifiers::NONE);
    assert_eq!(app.input.lines(), ["Keep this draf!t"]);
    app.perform_action(Action::Delete).unwrap();
    key(&mut app, KeyCode::Left, KeyModifiers::ALT);
    assert!(matches!(app.engine.view, View::DeleteMod(_)));
    assert_eq!(app.current_mod().unwrap().id, second);
    let text = rows(&screen(&mut app, 80, 44)).join("\n");
    assert!(!text.contains("Alt←"));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    key(&mut app, KeyCode::Left, KeyModifiers::ALT);
    assert_eq!(app.input.lines(), [""]);
}

#[test]
fn terminal_creation_sync_details_escape_preserves_both_route_and_drafts() {
    let (_data, mut app, first, _, _) = fixture();
    paste(&mut app, "Saved instruction");
    app.engine.project_error = Some("Project sync failed".into());
    key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(app.engine.view == View::NewMod);
    paste(&mut app, "Unsent creation");
    key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
    assert!(matches!(app.engine.view, View::Sync(_)));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.engine.view == View::Chat);
    assert_eq!(
        app.navigation_snapshot().destination,
        crate::engine::Destination::Codemod { mod_id: first }
    );
    assert_eq!(app.input.lines(), ["Saved instruction"]);
    assert_eq!(app.current_mod().unwrap().draft, "Saved instruction");
    key(&mut app, KeyCode::Left, KeyModifiers::ALT);
    assert!(app.engine.view == View::NewMod);
    assert_eq!(app.input.lines(), ["Unsent creation"]);
    key(&mut app, KeyCode::Right, KeyModifiers::ALT);
    assert_eq!(app.input.lines(), ["Saved instruction"]);
}

#[test]
fn terminal_task_and_other_inspections_never_offer_creation_or_exit_on_its_menu_key() {
    let (_data, mut app, _) = execution_app();
    key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
    assert!(matches!(app.engine.view, View::Tasks(_)));
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let task = app.engine.view;
    assert!(matches!(task, View::Task(_, _)));
    key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
    assert!(
        !app.action_dock()
            .actions
            .iter()
            .any(|a| a.action == Action::NewMod)
    );
    key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(matches!(app.engine.view, View::Actions(_)));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.engine.view == task && !app.quit);
    assert_eq!(app.input.lines(), ["keep this draft"]);
    for origin in [
        View::History(0),
        View::Findings(0),
        View::Tasks(0),
        View::Queue(0),
    ] {
        app.engine.view = origin;
        assert!(
            !app.action_dock()
                .actions
                .iter()
                .any(|a| a.action == Action::NewMod)
        );
        app.open_actions();
        assert!(
            !app.action_dock()
                .actions
                .iter()
                .any(|a| a.action == Action::NewMod)
        );
        key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.engine.view == origin && !app.quit);
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }
}
