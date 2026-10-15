use super::*;

#[test]
fn non_utf8_pdf_diff_opens_in_both_interfaces_and_enters_review() {
    for desktop in [false, true] {
        let (_data, mut app, root) = execution_app();
        let pdf = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n%%EOF\n";
        std::fs::write(root.join("work/fixture.pdf"), pdf).unwrap();
        let fingerprint =
            workspace::fingerprint(&workspace::source_state(&root.join("work")).unwrap()).unwrap();
        let id = app.engine.mods[0].id;
        let checks = app.engine.mods[0]
            .execution
            .as_ref()
            .unwrap()
            .checks
            .clone();
        app.engine
            .store
            .execution_checks(id, "review", &checks, Some(&fingerprint))
            .unwrap();
        app.engine.mods[0].execution = app.engine.store.execution(id).unwrap();

        if desktop {
            app.engine
                .command(crate::engine::api::Command::Action {
                    mod_id: Some(id),
                    action: "diff".into(),
                    task_id: None,
                })
                .unwrap();
        } else {
            key(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
        }
        assert!(matches!(app.engine.view, View::Review(_)));
        assert!(app.worker_error().is_none());
        let frame = serde_json::to_value(app.engine.snapshot()).unwrap();
        assert!(
            frame["dialog"]["body"]
                .as_str()
                .unwrap()
                .contains("fixture.pdf")
        );
        assert!(
            rows(&screen(&mut app, 100, 40))
                .join("\n")
                .contains("fixture.pdf")
        );

        app.engine.store.begin_review(id, &fingerprint).unwrap();
        app.engine.mods[0].agent_review = app.engine.store.review_state(id).unwrap();
        let submission = app
            .engine
            .store
            .review_input(&app.engine.mods[0])
            .unwrap()
            .unwrap();
        assert!(submission.texts[0].contains("fixture.pdf"));
        assert!(submission.texts[0].contains("%PDF-1.7"));
        let saved = app.engine.store.execution(id).unwrap().unwrap();
        assert_eq!(saved.status, "review");
        assert_eq!(saved.tasks[0].status, "done");
        assert_eq!(
            app.engine
                .store
                .0
                .query_row(
                    "SELECT attempt FROM task_runs WHERE id=?1",
                    [saved.tasks[0].id],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(saved.fingerprint.as_deref(), Some(fingerprint.as_str()));
        assert_eq!(std::fs::read(root.join("work/fixture.pdf")).unwrap(), pdf);
    }
}
