use super::*;
use crate::{
    app::workspace_navigation::AgentDetail,
    engine::agents::tests::{FAILURE, active_fixture, fixture},
};

#[test]
fn agents_task_focus_toggles_preserve_output_draft_and_keyboard_focus() {
    for width in [40, 100, 120, 160] {
        let (data, engine) = active_fixture();
        let mut app = App::load(engine.project.clone(), false, data.store()).unwrap();
        *app.engine = engine;
        screen(&mut app, width, 75);
        click(&mut app, Control::Tab(WorkspaceView::Agents));
        let expanded = rows(&screen(&mut app, width, 75)).join("\n");
        assert!(
            expanded.contains("Task 2") && expanded.contains("Preparing"),
            "{expanded}"
        );
        assert!(
            expanded.contains("Hide activity") && expanded.contains("Show all 11"),
            "{expanded}"
        );
        assert!(
            expanded.contains("01") && expanded.contains("Preflight"),
            "{expanded}"
        );
        assert!(
            expanded.contains("6m 49s") && !expanded.contains("Provider connection"),
            "{expanded}"
        );
        assert!(!expanded.contains("Worker details") && !expanded.contains("Activity details"));
        let draft = app.input.lines().to_vec();
        let cursor = app.input.cursor();
        key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let compact = rows(&screen(&mut app, width, 75)).join("\n");
        assert!(
            compact.contains("Show activity") && !compact.contains("Preflight"),
            "{compact}"
        );
        assert!(
            compact.contains("Worker")
                && compact.contains("Model")
                && compact.contains("Reasoning")
        );
        click(&mut app, Control::AgentDetail(AgentDetail::AllTimings));
        let timings = rows(&screen(&mut app, width, 75)).join("\n");
        assert!(
            timings.contains("Provider connection") && timings.contains("31.2s"),
            "{timings}"
        );
        assert!(timings.contains("Show longest 3") && timings.contains("may overlap"));
        click(
            &mut app,
            Control::AgentDetail(AgentDetail::ActivityHidden(80)),
        );
        let restored = rows(&screen(&mut app, width, 75)).join("\n");
        assert!(restored.contains("Preflight"));
        assert_eq!(app.input.lines(), draft);
        assert_eq!(app.input.cursor(), cursor);
        assert_eq!(app.workspace.focus, Focus::Content);
    }
}

#[test]
fn agents_keep_current_and_previous_diagnostics_in_the_unified_console() {
    let (data, engine) = fixture();
    let mut app = App::load(engine.project.clone(), false, data.store()).unwrap();
    *app.engine = engine;
    screen(&mut app, 120, 100);
    click(&mut app, Control::Tab(WorkspaceView::Agents));
    let output = rows(&screen(&mut app, 120, 100)).join("\n");
    assert!(
        output.contains("Traceback") && output.contains("408.16"),
        "{output}"
    );
    assert!(output.contains("Browser download exceeds the source-file limit."));
    assert!(app.action_dock().error.is_none());
    assert_eq!(app.engine.action_dock().error.as_deref(), Some(FAILURE));
    let agent = &app.engine.agents().unwrap().workers[0];
    assert_eq!(agent.output, FAILURE);
    let id = agent.worker.id;
    click(
        &mut app,
        Control::AgentDetail(AgentDetail::ActivityHidden(id)),
    );
    let folded = rows(&screen(&mut app, 120, 100)).join("\n");
    assert!(!folded.contains("Traceback") && folded.contains("Browser download exceeds"));
    let task = &mut app.engine.mods[0].execution.as_mut().unwrap().tasks[0];
    task.status = "pending".into();
    task.summary.clear();
    click(
        &mut app,
        Control::AgentDetail(AgentDetail::ActivityHidden(id)),
    );
    let previous = rows(&screen(&mut app, 120, 100)).join("\n");
    assert!(previous.contains("Previous worker diagnostic") && previous.contains("Traceback"));
    app.engine.poll_error = Some("Unable to save the project database.".into());
    assert_eq!(
        app.action_dock().error.as_deref(),
        Some("Unable to save the project database.")
    );
}

#[test]
#[ignore = "exports actual terminal buffers for visual review"]
fn export_task_focus_agents_buffers() {
    let directory =
        PathBuf::from(std::env::var("SPROWT_TASK_FOCUS_BUFFERS").expect("output directory"));
    std::fs::create_dir_all(&directory).unwrap();
    for (name, fixture) in [
        ("active", active_fixture as fn() -> _),
        ("failure", fixture as fn() -> _),
    ] {
        let (data, engine): (crate::store::test_support::TestData, crate::engine::Engine) =
            fixture();
        let mut app = App::load(engine.project.clone(), false, data.store()).unwrap();
        *app.engine = engine;
        for appearance in [
            crate::appearance::Appearance::Light,
            crate::appearance::Appearance::Dark,
        ] {
            app.engine.appearance = appearance;
            for width in [40, 120, 160] {
                for (state, toggles) in [
                    ("expanded", vec![]),
                    (
                        "compact",
                        vec![AgentDetail::ActivityHidden(if name == "active" {
                            80
                        } else {
                            app.engine.agents().unwrap().workers[0].worker.id
                        })],
                    ),
                    ("all-timings", vec![AgentDetail::AllTimings]),
                ] {
                    app.workspace.agent_toggles = toggles.into_iter().collect();
                    screen(&mut app, width, 55);
                    app.activate_control(Control::Tab(WorkspaceView::Agents))
                        .unwrap();
                    let buffer = screen(&mut app, width, 55);
                    let cells: Vec<_> = buffer.content.iter().enumerate().map(|(index, cell)| serde_json::json!({"x":index % width as usize,"y":index / width as usize,"symbol":cell.symbol(),"fg":format!("{:?}",cell.fg),"bg":format!("{:?}",cell.bg),"bold":cell.modifier.contains(ratatui::style::Modifier::BOLD)})).collect();
                    let stem = format!(
                        "{name}-{}-{width}-{state}",
                        if appearance == crate::appearance::Appearance::Light {
                            "light"
                        } else {
                            "dark"
                        }
                    );
                    std::fs::write(
                        directory.join(format!("{stem}.json")),
                        serde_json::to_vec(
                            &serde_json::json!({"width":width,"height":55,"cells":cells}),
                        )
                        .unwrap(),
                    )
                    .unwrap();
                    std::fs::write(
                        directory.join(format!("{stem}.txt")),
                        rows(&buffer).join("\n"),
                    )
                    .unwrap();
                }
            }
        }
    }
}
