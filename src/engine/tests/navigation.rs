use super::*;

fn destination(engine: &Engine) -> serde_json::Value {
    serde_json::to_value(engine.snapshot()).unwrap()["navigation"].clone()
}

#[test]
fn desktop_navigation_preserves_both_drafts_and_truncates_a_new_selection() {
    let (_data, mut engine, first, second) = session();
    engine
        .command(Command::SetDraft {
            mod_id: first,
            text: "First instruction".into(),
        })
        .unwrap();
    engine.command(Command::BeginCreation {}).unwrap();
    engine.composer = "Unsent **creation**".into();
    engine.command(Command::GoBack {}).unwrap();
    assert_eq!(engine.composer, "First instruction");
    assert_eq!(
        destination(&engine),
        serde_json::json!({"destination":{"kind":"codemod","mod_id":first},"can_go_back":false,"can_go_forward":true})
    );
    engine.command(Command::GoForward {}).unwrap();
    assert_eq!(engine.composer, "Unsent **creation**");
    engine.command(Command::GoBack {}).unwrap();
    engine
        .command(Command::SelectMod { mod_id: second })
        .unwrap();
    engine
        .command(Command::SelectMod { mod_id: second })
        .unwrap();
    assert!(!engine.navigation_snapshot().can_go_forward);
    engine.command(Command::GoBack {}).unwrap();
    assert_eq!(engine.current_mod().unwrap().id, first);
    assert_eq!(engine.composer, "First instruction");
    assert!(!engine.navigation_snapshot().can_go_back);
    engine.command(Command::BeginCreation {}).unwrap();
    assert_eq!(engine.composer, "Unsent **creation**");
    assert!(engine.workers.is_empty() && engine.git_jobs.is_empty() && engine.mods.len() == 2);
    assert_eq!(
        engine.store.load_project(&engine.project).unwrap().mods[0].draft,
        "First instruction"
    );
    let reopened = Engine::load(engine.project.clone(), _data.store()).unwrap();
    assert!(
        !reopened.navigation_snapshot().can_go_back
            && !reopened.navigation_snapshot().can_go_forward
    );
}

#[test]
fn desktop_navigation_keeps_closed_work_readable_and_removes_deleted_destinations() {
    let (_data, mut engine, first, second) = session();
    engine.store.close_mod(engine.project_id, second).unwrap();
    engine.mods[1].closed = true;
    engine
        .command(Command::SelectMod { mod_id: second })
        .unwrap();
    engine.command(Command::GoBack {}).unwrap();
    assert_eq!(engine.current_mod().unwrap().id, first);
    engine.command(Command::GoForward {}).unwrap();
    assert!(engine.current_mod().unwrap().closed && engine.read_only());
    engine
        .command(Command::Action {
            mod_id: Some(second),
            action: "delete".into(),
            task_id: None,
        })
        .unwrap();
    engine
        .command(Command::Confirm {
            mod_id: Some(second),
            kind: "delete".into(),
        })
        .unwrap();
    assert_eq!(engine.current_mod().unwrap().id, first);
    assert_eq!(
        destination(&engine),
        serde_json::json!({"destination":{"kind":"codemod","mod_id":first},"can_go_back":false,"can_go_forward":false})
    );
    engine.command(Command::GoForward {}).unwrap();
    assert_eq!(engine.current_mod().unwrap().id, first);
    assert_eq!(engine.mods.len(), 1);
}

#[test]
fn desktop_navigation_rejects_dialogs_before_changing_history_or_text() {
    let (_data, mut engine, first, second) = session();
    engine
        .command(Command::SelectMod { mod_id: second })
        .unwrap();
    engine.composer = "Keep the instruction".into();
    engine
        .command(Command::Action {
            mod_id: Some(second),
            action: "delete".into(),
            task_id: None,
        })
        .unwrap();
    for command in [
        Command::GoBack {},
        Command::GoForward {},
        Command::BeginCreation {},
        Command::SelectMod { mod_id: first },
    ] {
        assert!(engine.command(command).unwrap_err().contains("open dialog"));
        assert_eq!(engine.composer, "Keep the instruction");
        assert_eq!(engine.current_mod().unwrap().id, second);
        assert!(!engine.navigation_snapshot().can_go_back);
    }
    engine.command(Command::Cancel {}).unwrap();
    engine.command(Command::GoBack {}).unwrap();
    assert_eq!(engine.current_mod().unwrap().id, first);
    assert!(engine.navigation_snapshot().can_go_forward);
}

#[test]
fn desktop_navigation_records_an_async_inspection_that_displaces_creation() {
    let (data, mut engine, first, _) = session();
    let root = data.0.join("review");
    workspace::create(&engine.project, &root).unwrap();
    engine.mods[0].git_root = Some(root.clone());
    engine
        .command(Command::SetDraft {
            mod_id: first,
            text: "Keep instruction".into(),
        })
        .unwrap();
    engine.command(Command::BeginCreation {}).unwrap();
    engine.composer = "Keep creation".into();
    engine.git_jobs.insert(
        first,
        Job::start("reviewing changes", move |_| {
            workspace::review(&root).map(git_mod::Result::Reviewed)
        }),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while !engine.git_jobs.is_empty() {
        assert!(Instant::now() < deadline);
        engine.poll_git_jobs().unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(matches!(engine.view, View::Review(_)));
    assert_eq!(
        engine.navigation_snapshot().destination,
        Destination::Codemod { mod_id: first }
    );
    assert_eq!(engine.composer, "Keep instruction");
    engine.command(Command::Cancel {}).unwrap();
    assert_eq!(
        engine.navigation_snapshot().destination,
        Destination::Codemod { mod_id: first }
    );
    engine.command(Command::GoBack {}).unwrap();
    assert!(engine.view == View::NewMod);
    assert_eq!(engine.composer, "Keep creation");
}

#[test]
fn desktop_stale_cancel_preserves_creation_route_and_draft() {
    let (_data, mut engine, first, _) = session();
    engine
        .command(Command::SetDraft {
            mod_id: first,
            text: "Keep instruction".into(),
        })
        .unwrap();
    engine.command(Command::BeginCreation {}).unwrap();
    engine.composer = "Keep creation".into();
    assert_eq!(
        engine.command(Command::Cancel {}).unwrap_err(),
        "This dialog is no longer available."
    );
    assert!(engine.view == View::NewMod);
    assert_eq!(engine.composer, "Keep creation");
    engine.command(Command::GoBack {}).unwrap();
    assert_eq!(engine.composer, "Keep instruction");
    engine.command(Command::GoForward {}).unwrap();
    assert_eq!(engine.composer, "Keep creation");
}

#[test]
fn desktop_creation_keeps_saved_paused_and_running_mod_statuses() {
    for paused in [true, false] {
        let (_data, mut engine, first, _) = session();
        engine.mods[0].user_paused = paused;
        if !paused {
            engine.mods[0].planning.as_mut().unwrap().status = "running".into();
        }
        engine.command(Command::BeginCreation {}).unwrap();
        let snapshot = serde_json::to_value(engine.snapshot()).unwrap().clone();
        let saved = snapshot["mods"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == first)
            .unwrap();
        assert_eq!(
            saved["status"],
            if paused {
                "Paused by you"
            } else {
                "Planning · running"
            }
        );
        assert_eq!(snapshot["status"], "New codemod");
        assert_eq!(snapshot["navigation"]["destination"]["kind"], "new_codemod");
    }
}

#[test]
fn desktop_sync_inspection_from_creation_returns_to_its_recorded_workspace() {
    let (_data, mut engine, first, _) = session();
    engine
        .command(Command::SetDraft {
            mod_id: first,
            text: "Saved instruction".into(),
        })
        .unwrap();
    engine.project_error = Some("Project sync failed".into());
    engine.command(Command::BeginCreation {}).unwrap();
    engine.composer = "Unsent creation".into();
    engine
        .command(Command::Action {
            mod_id: Some(first),
            action: "sync_details".into(),
            task_id: None,
        })
        .unwrap();
    assert_eq!(
        engine.navigation_snapshot().destination,
        Destination::Codemod { mod_id: first }
    );
    engine.command(Command::Cancel {}).unwrap();
    assert!(engine.view == View::Chat);
    assert_eq!(engine.composer, "Saved instruction");
    engine.command(Command::GoBack {}).unwrap();
    assert!(engine.view == View::NewMod);
    assert_eq!(engine.composer, "Unsent creation");
}
