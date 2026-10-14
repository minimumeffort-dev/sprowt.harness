use super::*;
use crate::{
    app::workspace_navigation::AgentDetail,
    engine::agents::tests::{FAILURE, fixture},
};

#[test]
fn agents_fold_diagnostics_and_timings_below_assignments_with_keyboard_and_mouse() {
    for width in [40, 100, 120, 160] {
        let (data, engine) = fixture();
        let mut app = App::load(engine.project.clone(), false, data.store()).unwrap();
        *app.engine = engine;
        screen(&mut app, width, 75);
        click(&mut app, Control::Tab(WorkspaceView::Agents));
        let compact = rows(&screen(&mut app, width, 75)).join("\n");
        assert!(
            !compact.contains("Traceback") && !compact.contains("408.16"),
            "{compact}"
        );
        assert!(
            compact.contains("Failure details") && compact.contains("Setup timings"),
            "{compact}"
        );
        assert!(compact.find("Add task PDF").unwrap() < compact.find("Setup timings").unwrap());
        assert!(!compact.contains("Recorded setup timings"));
        assert!(app.action_dock().error.is_none());
        assert!(app.engine.action_dock().error.as_deref() == Some(FAILURE));
        let draft = app.input.lines().to_vec();
        let cursor = app.input.cursor();
        if width == 120
            && let Ok(path) = std::env::var("SPROWT_AGENTS_CLI_FIXTURE")
        {
            std::fs::write(
                format!("{path}-compact.txt"),
                rows(&screen(&mut app, 120, 40)).join("\n"),
            )
            .unwrap();
            screen(&mut app, width, 75);
        }
        key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let expanded = rows(&screen(&mut app, width, 75)).join("\n");
        assert!(expanded.contains("Traceback"), "{expanded}");
        if width == 120
            && let Ok(path) = std::env::var("SPROWT_AGENTS_CLI_FIXTURE")
        {
            std::fs::write(
                format!("{path}-expanded.txt"),
                rows(&screen(&mut app, 120, 40)).join("\n"),
            )
            .unwrap();
            screen(&mut app, width, 75);
        }
        let id = app.engine.agents().unwrap().workers[0].worker.id;
        click(&mut app, Control::AgentDetail(AgentDetail::Failure(id)));
        screen(&mut app, width, 75);
        click(&mut app, Control::AgentDetail(AgentDetail::Timings));
        let timings = rows(&screen(&mut app, width, 75)).join("\n");
        assert!(
            timings.contains("Worker slot wait") && timings.contains("1m 43s"),
            "{timings}"
        );
        assert_eq!(app.input.lines(), draft);
        assert_eq!(app.input.cursor(), cursor);
        assert_eq!(app.workspace.focus, Focus::Content);
        app.engine.poll_error = Some("Unable to save the project database.".into());
        assert_eq!(
            app.action_dock().error.as_deref(),
            Some("Unable to save the project database.")
        );
    }
}
