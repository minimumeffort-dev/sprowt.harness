use super::*;

fn project_session() -> (TestData, Engine, i64, i64) {
    let (data, mut engine, first, second) = session();
    engine.project = engine.project.canonicalize().unwrap();
    engine
        .store
        .0
        .execute(
            "UPDATE projects SET path=?1 WHERE id=?2",
            rusqlite::params![engine.project.to_str().unwrap(), engine.project_id],
        )
        .unwrap();
    engine.projects = engine.store.projects().unwrap();
    (data, engine, first, second)
}

fn second_project(data: &TestData, engine: &mut Engine) -> (i64, i64, PathBuf) {
    let path = data.0.join("other");
    std::fs::create_dir_all(&path).unwrap();
    let path = path.canonicalize().unwrap();
    let id = engine.store.load_project(&path).unwrap().id;
    let code_mod = engine
        .store
        .create_mod(id, "Other project change")
        .unwrap()
        .id;
    engine.command(Command::Snapshot {}).unwrap();
    (id, code_mod, path)
}

#[test]
fn project_accordion_only_browses_and_switching_preserves_drafts_and_history() {
    let (data, mut engine, first, _) = project_session();
    let a = engine.project_id;
    let (b, second, _) = second_project(&data, &mut engine);
    engine
        .command(Command::SetDraft {
            mod_id: first,
            text: "A instruction".into(),
        })
        .unwrap();
    engine.command(Command::BeginCreation {}).unwrap();
    engine
        .command(Command::SetCreationDraft {
            project_id: a,
            text: "A creation".into(),
        })
        .unwrap();
    engine
        .command(Command::ToggleProject { project_id: a })
        .unwrap();
    assert_eq!(engine.expanded_project, None);
    engine
        .command(Command::ToggleProject { project_id: b })
        .unwrap();
    assert_eq!(engine.expanded_project, Some(b));
    assert_eq!(engine.project_id, a);
    assert_eq!(engine.composer, "A creation");
    engine
        .command(Command::SelectMod { mod_id: second })
        .unwrap();
    assert_eq!(engine.project_id, b);
    engine
        .command(Command::SetDraft {
            mod_id: second,
            text: "B instruction".into(),
        })
        .unwrap();
    engine
        .command(Command::BeginProjectCreation { project_id: b })
        .unwrap();
    engine
        .command(Command::SetCreationDraft {
            project_id: b,
            text: "B creation".into(),
        })
        .unwrap();
    engine
        .command(Command::SelectProject { project_id: a })
        .unwrap();
    assert_eq!(engine.composer, "A creation");
    assert_eq!(engine.navigation.current(), Destination::NewCodemod);
    engine.command(Command::GoBack {}).unwrap();
    assert_eq!(engine.composer, "A instruction");
    engine
        .command(Command::SelectProject { project_id: b })
        .unwrap();
    assert_eq!(engine.composer, "B creation");
    engine.command(Command::GoBack {}).unwrap();
    assert_eq!(engine.composer, "B instruction");
    assert_eq!(engine.store.creation_draft(a).unwrap(), "A creation");
    assert_eq!(engine.store.creation_draft(b).unwrap(), "B creation");
    let mut reopened = Engine::load(engine.project.clone(), data.store()).unwrap();
    reopened.command(Command::BeginCreation {}).unwrap();
    assert_eq!(reopened.composer, "B creation");
}

#[test]
fn project_switch_rejects_busy_missing_locked_and_confirming_targets_without_losing_state() {
    let (data, mut engine, first, _) = project_session();
    let a = engine.project_id;
    let (b, _, path) = second_project(&data, &mut engine);
    engine
        .command(Command::SetDraft {
            mod_id: first,
            text: "Keep this instruction".into(),
        })
        .unwrap();
    engine.auto_plans.insert(first);
    assert!(
        engine
            .command(Command::SelectProject { project_id: b })
            .unwrap_err()
            .contains("Pause running work")
    );
    engine.auto_plans.clear();
    engine.view = View::DeleteMod(0);
    assert!(
        engine
            .command(Command::SelectProject { project_id: b })
            .unwrap_err()
            .contains("Finish or cancel")
    );
    engine.view = View::Review(0);
    for project_id in [a, b] {
        assert!(
            engine
                .command(Command::BeginProjectCreation { project_id })
                .unwrap_err()
                .contains("Finish or cancel")
        );
        assert_eq!(engine.project_id, a);
        assert!(engine.view == View::Review(0));
    }
    engine.view = View::Chat;
    let lock = ownership::acquire(&data.0.join("state.db"), &path.canonicalize().unwrap()).unwrap();
    assert!(
        engine
            .command(Command::SelectProject { project_id: b })
            .unwrap_err()
            .contains("already open")
    );
    assert_eq!(engine.project_id, a);
    assert_eq!(engine.composer, "Keep this instruction");
    assert_eq!(engine.current_mod().unwrap().id, first);
    drop(lock);
    assert!(
        engine
            .command(Command::OpenProject {
                path: path.join("missing").display().to_string()
            })
            .unwrap_err()
            .contains("Could not open")
    );
    assert_eq!(engine.project_id, a);
    engine
        .command(Command::SelectProject { project_id: b })
        .unwrap();
    assert_eq!(engine.project_id, b);
}

#[test]
fn project_return_prunes_history_deleted_by_another_controller() {
    let (data, mut engine, first, second) = project_session();
    let a = engine.project_id;
    let (b, _, _) = second_project(&data, &mut engine);
    engine
        .command(Command::SelectMod { mod_id: second })
        .unwrap();
    engine.command(Command::BeginCreation {}).unwrap();
    engine
        .command(Command::SelectProject { project_id: b })
        .unwrap();
    data.store().delete_mod(a, second).unwrap();
    engine
        .command(Command::SelectProject { project_id: a })
        .unwrap();
    engine.command(Command::GoBack {}).unwrap();
    assert_eq!(engine.current_mod().unwrap().id, first);
    assert!(!engine.navigation_snapshot().can_go_back);
}

#[test]
fn creation_commit_clears_its_project_draft_and_keeps_other_projects() {
    let (data, mut engine, _, _) = project_session();
    let a = engine.project_id;
    let (b, _, _) = second_project(&data, &mut engine);
    engine
        .store
        .save_creation_draft(a, "Submitted change")
        .unwrap();
    engine
        .store
        .save_creation_draft(b, "Other request")
        .unwrap();
    let created = engine.store.create_mod(a, "Submitted change").unwrap();
    assert_eq!(created.description, "Submitted change");
    assert_eq!(engine.store.creation_draft(a).unwrap(), "");
    assert_eq!(engine.store.creation_draft(b).unwrap(), "Other request");
}
