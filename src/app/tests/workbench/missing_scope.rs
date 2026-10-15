use super::*;
use crate::{engine::api::Command, repair::missing_scope_tests, store::task_source};

#[test]
fn amended_file_assignment_survives_native_and_terminal_retry_without_losing_peers() {
    for desktop in [false, true] {
        let (_data, mut store, mod_id, _, ids, _, repair) = missing_scope_tests::fixture();
        store
            .save_draft(mod_id, "Keep my next instruction")
            .unwrap();
        let source = task_source(ids[2], 1);
        assert_eq!(
            store.request_repair(mod_id, &source, &repair).unwrap(),
            Ok(())
        );
        assert!(store.resume_repairs(mod_id).unwrap());
        store
            .finish_task(
                mod_id,
                &task_source(ids[2], 2),
                "paused",
                "Saved repair interrupted",
                &[],
            )
            .unwrap();
        let mut app = App::load(PathBuf::from("/repair-test"), false, store).unwrap();
        app.view = View::Task(ids[2], 0);
        app.show_evidence = true;
        let text = rows(&screen(&mut app, 120, 60)).join("\n");
        assert!(text.contains("integration.spec.mjs"), "{text}");
        assert_eq!(app.input.lines(), ["Keep my next instruction"]);
        let cursor = app.input.cursor();
        let before = serde_json::to_value(app.engine.snapshot()).unwrap();
        let tasks = &before["active_mod"]["plan"]["tasks"];
        assert_eq!(tasks[0]["completed"], true);
        assert_eq!(tasks[1]["completed"], true);
        assert!(
            tasks[2]["files"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(missing_scope_tests::MISSED_FILE))
        );

        if desktop {
            app.engine
                .command(Command::Action {
                    mod_id: Some(mod_id),
                    action: "retry_task".into(),
                    task_id: Some(ids[2]),
                })
                .unwrap();
        } else {
            key(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        }
        let after = serde_json::to_value(app.engine.snapshot()).unwrap();
        for i in [0, 1] {
            assert_eq!(after["active_mod"]["plan"]["tasks"][i], tasks[i]);
        }
        let run = &app.engine.mods[0].execution.as_ref().unwrap().tasks[2];
        assert_eq!(run.source, task_source(ids[2], 3));
        assert_eq!(run.status, "pending");
        assert_eq!(app.input.lines(), ["Keep my next instruction"]);
        assert_eq!(app.input.cursor(), cursor);
        assert_eq!(
            app.engine
                .store
                .load_project(&app.engine.project)
                .unwrap()
                .mods[0]
                .draft,
            "Keep my next instruction"
        );
        assert!(!app.engine.can_publish());
    }
}
