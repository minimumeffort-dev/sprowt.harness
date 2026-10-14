use super::*;
use crate::{app::controls::WorkspaceView, appearance::Appearance};
use ratatui::style::Color;

#[test]
fn appearance_keys_apply_cancel_and_preserve_both_drafts_caret_and_workspace() {
    let data = TestData::new();
    let project = test_project(&data, "appearance");
    let mut app = App::load(project.clone(), false, data.store()).unwrap();
    paste(&mut app, "Creation draft");
    key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    let cursor = app.input.cursor();
    key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(app.appearance_picker, Some(Appearance::System));
    paste(&mut app, "must not replace draft");
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.engine.appearance, Appearance::Light);
    assert_eq!(app.input.lines(), ["Creation draft"]);
    assert_eq!(app.input.cursor(), cursor);
    key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.engine.appearance, Appearance::Light);
    assert_eq!(app.input.cursor(), cursor);
    let reopened = App::load(project, false, data.store()).unwrap();
    assert_eq!(reopened.engine.appearance, Appearance::Light);

    let (_data, mut app, _) = execution_app();
    app.workspace.inspector.set_tab(WorkspaceView::Checks);
    app.workspace.focus = workspace_navigation::Focus::Content;
    app.input.move_cursor(ratatui_textarea::CursorMove::Head);
    let cursor = app.input.cursor();
    key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.engine.appearance, Appearance::Dark);
    assert_eq!(app.workspace.inspector.tab, WorkspaceView::Checks);
    assert_eq!(app.workspace.focus, workspace_navigation::Focus::Content);
    assert_eq!(app.input.cursor(), cursor);
    assert_eq!(app.input.lines(), ["keep this draft"]);
}

#[test]
fn appearance_save_failure_keeps_the_picker_open_and_previous_colors() {
    let data = TestData::new();
    let mut app = App::load(
        test_project(&data, "read-only-appearance"),
        false,
        data.store(),
    )
    .unwrap();
    app.engine.set_appearance(Appearance::Light).unwrap();
    app.engine
        .store
        .0
        .execute_batch("PRAGMA query_only = ON;")
        .unwrap();
    app.appearance_picker = Some(Appearance::Dark);
    app.handle(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )))
    .unwrap();
    assert_eq!(app.engine.appearance, Appearance::Light);
    assert_eq!(app.appearance_picker, Some(Appearance::Dark));
    let buffer = screen(&mut app, 80, 36);
    assert!(
        rows(&buffer)
            .join("\n")
            .contains("Could not save appearance:")
    );
    assert_eq!(buffer[(0, 0)].bg, Color::Rgb(250, 251, 248));
}

#[test]
fn appearance_palettes_paint_all_workspace_and_dialog_cells_in_both_themes() {
    for (appearance, paper, ink, accent) in [
        (
            Appearance::Light,
            Color::Rgb(250, 251, 248),
            Color::Rgb(32, 41, 35),
            Color::Rgb(41, 103, 70),
        ),
        (
            Appearance::Dark,
            Color::Rgb(24, 28, 26),
            Color::Rgb(237, 241, 235),
            Color::Rgb(162, 213, 180),
        ),
    ] {
        let (_data, mut app, _) = super::workbench::all_views_app();
        app.engine.project = PathBuf::from("/example/project");
        app.engine.set_appearance(appearance).unwrap();
        for width in [40, 80, 160] {
            for tab in WorkspaceView::ALL {
                screen(&mut app, width, 44);
                app.workspace.inspector.set_tab(tab);
                let buffer = screen(&mut app, width, 44);
                assert_eq!(buffer[(0, 0)].bg, paper);
                assert_eq!(buffer[(0, 0)].fg, ink);
                assert!(
                    buffer
                        .content
                        .iter()
                        .any(|cell| cell.fg == accent && cell.symbol() != " ")
                );
                assert!(
                    buffer
                        .content
                        .iter()
                        .all(|cell| matches!(cell.fg, Color::Rgb(..))
                            && matches!(cell.bg, Color::Rgb(..)))
                );
                capture(
                    &buffer,
                    &format!(
                        "{}-{}-{width}",
                        appearance.label().to_lowercase(),
                        tab.label().to_lowercase()
                    ),
                );
            }
            app.appearance_picker = Some(appearance);
            let buffer = screen(&mut app, width, 44);
            let text = rows(&buffer).join("\n");
            assert!(
                text.contains("appearance")
                    && text.contains("System")
                    && text.contains("Light")
                    && text.contains("Dark")
            );
            assert!(
                buffer
                    .content
                    .iter()
                    .all(|cell| matches!(cell.fg, Color::Rgb(..))
                        && matches!(cell.bg, Color::Rgb(..)))
            );
            capture(
                &buffer,
                &format!("{}-appearance-{width}", appearance.label().to_lowercase()),
            );
            app.appearance_picker = None;
        }
    }
}

fn capture(buffer: &ratatui::buffer::Buffer, name: &str) {
    let Some(directory) = std::env::var_os("SPROWT_APPEARANCE_BUFFERS") else {
        return;
    };
    let directory = PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let width = buffer.area.width;
    let cells: Vec<_> = buffer.content.iter().enumerate().map(|(index,cell)| serde_json::json!({"x":index % width as usize,"y":index / width as usize,"symbol":cell.symbol(),"fg":format!("{:?}",cell.fg),"bg":format!("{:?}",cell.bg),"bold":cell.modifier.contains(ratatui::style::Modifier::BOLD)})).collect();
    std::fs::write(
        directory.join(format!("{name}.json")),
        serde_json::to_vec(
            &serde_json::json!({"width":width,"height":buffer.area.height,"cells":cells}),
        )
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        directory.join(format!("{name}.txt")),
        rows(buffer).join("\n"),
    )
    .unwrap();
}
