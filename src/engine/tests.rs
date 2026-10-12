use super::*;
use crate::{
    engine::api::{Command, Direction},
    store::test_support::TestData,
};

fn session() -> (TestData, Engine, i64, i64) {
    let data = TestData::new();
    let mut store = data.store();
    let project = data.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project_id = store.load_project(&project).unwrap().id;
    let first = store
        .create_mod(project_id, "Add native controls")
        .unwrap()
        .id;
    let second = store.create_mod(project_id, "Other codemod").unwrap().id;
    store.select_mod(project_id, first).unwrap();
    let engine = Engine::load(project, store).unwrap();
    (data, engine, first, second)
}

#[test]
fn desktop_commands_preserve_drafts_queue_order_and_saved_steering_on_reopen() {
    let (data, mut engine, first, second) = session();
    engine
        .command(Command::Submit {
            mod_id: first,
            text: "First instruction".into(),
        })
        .unwrap();
    engine
        .command(Command::Submit {
            mod_id: first,
            text: "Second instruction".into(),
        })
        .unwrap();
    engine
        .command(Command::SetDraft {
            mod_id: first,
            text: "Unsent draft".into(),
        })
        .unwrap();
    let ids = engine.mods[0]
        .queue
        .iter()
        .map(|m| m.id)
        .collect::<Vec<_>>();
    engine
        .command(Command::EditQueue {
            mod_id: first,
            message_id: ids[0],
            text: "Revised first".into(),
        })
        .unwrap();
    engine
        .command(Command::MoveQueue {
            mod_id: first,
            message_id: ids[1],
            direction: Direction::Up,
        })
        .unwrap();
    assert_eq!(
        engine.mods[0]
            .queue
            .iter()
            .map(|m| m.body.as_str())
            .collect::<Vec<_>>(),
        ["Second instruction", "Revised first"]
    );
    engine
        .command(Command::SteerQueue {
            mod_id: first,
            message_ids: ids,
        })
        .unwrap();
    assert_eq!(
        engine.mods[0].steering,
        ["Second instruction", "Revised first"]
    );
    assert_eq!(engine.mods[0].draft, "Unsent draft");
    assert!(
        engine.mods[0]
            .messages
            .iter()
            .all(|m| m.body != "Second instruction")
    );
    engine
        .command(Command::SelectMod { mod_id: second })
        .unwrap();
    engine
        .command(Command::SetDraft {
            mod_id: second,
            text: "Other draft".into(),
        })
        .unwrap();
    engine
        .command(Command::SelectMod { mod_id: first })
        .unwrap();
    let project = engine.project.clone();
    drop(engine);
    let reopened = Engine::load(project, data.store()).unwrap();
    assert_eq!(reopened.current_mod().unwrap().draft, "Unsent draft");
    assert_eq!(
        reopened.current_mod().unwrap().steering,
        ["Second instruction", "Revised first"]
    );
    assert_eq!(reopened.mods[1].draft, "Other draft");
    assert!(reopened.workers.is_empty() && reopened.auto_plans.is_empty());
}

#[test]
fn stale_commands_and_closed_submissions_preserve_saved_work() {
    let (_data, mut engine, first, second) = session();
    engine
        .command(Command::SetDraft {
            mod_id: first,
            text: "Saved draft".into(),
        })
        .unwrap();
    assert!(
        engine
            .command(Command::Submit {
                mod_id: second,
                text: "Wrong session".into()
            })
            .unwrap_err()
            .contains("selected codemod changed")
    );
    assert!(
        engine
            .command(Command::Action {
                mod_id: Some(first),
                action: "publish".into(),
                task_id: None
            })
            .unwrap_err()
            .contains("no longer available")
    );
    engine.mods[0].closed = true;
    assert_eq!(
        engine
            .command(Command::Submit {
                mod_id: first,
                text: "Forbidden replacement".into()
            })
            .unwrap_err(),
        "Wait for this operation to finish."
    );
    assert_eq!(engine.mods[0].draft, "Saved draft");
    assert!(engine.mods[0].queue.is_empty());
    let saved = engine.store.load_project(&engine.project).unwrap();
    assert_eq!(saved.mods[0].draft, "Saved draft");
}

#[test]
fn deletion_requires_the_open_matching_confirmation_and_can_be_cancelled() {
    let (_data, mut engine, first, second) = session();
    engine
        .command(Command::SetDraft {
            mod_id: first,
            text: "Keep this".into(),
        })
        .unwrap();
    assert!(
        engine
            .command(Command::Confirm {
                mod_id: Some(first),
                kind: "delete".into()
            })
            .is_err()
    );
    engine
        .command(Command::Action {
            mod_id: Some(first),
            action: "delete".into(),
            task_id: None,
        })
        .unwrap();
    assert_eq!(engine.snapshot().dialog.unwrap().kind, "delete");
    assert!(
        engine
            .command(Command::Confirm {
                mod_id: Some(second),
                kind: "delete".into()
            })
            .is_err()
    );
    engine.command(Command::Cancel {}).unwrap();
    assert_eq!(engine.mods.len(), 2);
    assert_eq!(engine.composer, "Keep this");
    engine
        .command(Command::Action {
            mod_id: Some(first),
            action: "delete".into(),
            task_id: None,
        })
        .unwrap();
    engine
        .command(Command::Confirm {
            mod_id: Some(first),
            kind: "delete".into(),
        })
        .unwrap();
    assert_eq!(
        engine.mods.iter().map(|m| m.id).collect::<Vec<_>>(),
        [second]
    );
}

#[test]
fn new_project_preview_does_not_initialize_git_until_confirmed() {
    let data = TestData::new();
    let project = data.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("hello.txt"), "Keep this source").unwrap();
    let mut engine = Engine::load(project.clone(), data.store()).unwrap();
    engine
        .command(Command::NewMod {
            description: "Add controls".into(),
        })
        .unwrap();
    assert_eq!(engine.snapshot().dialog.unwrap().kind, "project_setup");
    assert!(!project.join(".git").exists());
    assert!(engine.mods.is_empty());
    engine.command(Command::Cancel {}).unwrap();
    assert_eq!(
        std::fs::read_to_string(project.join("hello.txt")).unwrap(),
        "Keep this source"
    );
    assert_eq!(engine.composer, "Add controls");
}

#[test]
fn repository_setup_has_distinct_entry_and_confirmation_without_a_side_effect() {
    let (_data, mut engine, first, _) = session();
    engine.view = View::Repository(false);
    assert_eq!(
        engine.snapshot().dialog.unwrap().confirm_label.as_deref(),
        Some("Continue")
    );
    engine
        .command(Command::Repository {
            mod_id: first,
            slug: "example/project".into(),
            create: true,
        })
        .unwrap();
    let dialog = engine.snapshot().dialog.unwrap();
    assert_eq!(dialog.kind, "repository_confirmation");
    assert_eq!(dialog.confirm_label.as_deref(), Some("Create repository"));
    assert!(engine.git_jobs.is_empty());
    engine.command(Command::Cancel {}).unwrap();
    assert!(engine.git_jobs.is_empty());
}

#[test]
fn ownership_rejects_duplicate_controllers_and_releases_on_exit() {
    let data = TestData::new();
    let database = data.0.join("state.db");
    let project = data.0.join("project");
    let first = ownership::acquire(&database, &project).unwrap();
    assert!(
        ownership::acquire(&database, &project)
            .unwrap_err()
            .to_string()
            .contains("already open")
    );
    let other = ownership::acquire(&database, &data.0.join("other-project")).unwrap();
    drop(first);
    let reopened = ownership::acquire(&database, &project).unwrap();
    drop((other, reopened));
}

#[test]
fn desktop_contract_matches_the_native_fixture() {
    let (_data, mut engine, first, _) = session();
    engine.project = PathBuf::from("/example/project");
    let plan = crate::plan::Plan::parse(r#"{"summary":"Share the engine with native controls","tasks":[{"id":"core","title":"Extract the engine","outcome":"Both clients use the same lifecycle rules","files":["src/engine.rs"],"depends_on":[],"worker":"codex","checks":["CLI regression suite passes"]},{"id":"native","title":"Add native controls","outcome":"Open a project and manage codemods","files":["macos/Sources"],"depends_on":["core"],"worker":"codex","checks":["Draft survives reopening"]}]}"#).unwrap();
    let source = engine.mods[0].planning.as_ref().unwrap().source.clone();
    engine.store.save_plan(first, &source, &plan).unwrap();
    engine.mods[0].planning = engine.store.planning(first).unwrap();
    engine
        .command(Command::Submit {
            mod_id: first,
            text: "Keep keyboard shortcuts".into(),
        })
        .unwrap();
    engine
        .command(Command::SetDraft {
            mod_id: first,
            text: "Do not lose this draft".into(),
        })
        .unwrap();
    engine.mods[0].messages.push(crate::store::Message {
        source: None,
        task: None,
        item_id: None,
        role: "planner".into(),
        body: "I will split this into two tasks.".into(),
        model: Some("gpt-6.1-sol".into()),
        effort: Some("high".into()),
    });
    engine.mods[0]
        .steering
        .push("Preserve the existing keyboard controls".into());
    engine.mods[0].execution = Some(crate::execution::Execution {
        workspace: PathBuf::from("/example/workspace"),
        backend: "apple-container".into(),
        status: "blocked".into(),
        tasks: Vec::new(),
        fingerprint: None,
        checks: vec![crate::execution::CheckResult {
            timeout_seconds: Some(60),
            duration_ms: Some(50),
            task: None,
            check: "CLI regression suite passes".into(),
            command: vec!["cargo".into(), "test".into()],
            exit_code: Some(1),
            output: "Draft did not survive reopening".into(),
            missing_runtime: None,
        }],
    });
    engine.mods[0].agent_review = Some(crate::review::State {
        status: "findings".into(),
        issues: vec![crate::review::Issue {
            id: 1,
            status: "confirmed".into(),
            attempts: 0,
            regression: "Draft survives reopening".into(),
            history: Vec::new(),
            finding: crate::review::Finding {
                id: Some(1),
                triage: None,
                owner: "core".into(),
                file: "src/engine.rs".into(),
                line: 42,
                priority: "P1".into(),
                title: "Keep the unsent draft".into(),
                evidence: "Reopening discarded the composer text.".into(),
                fix: "Persist the draft before switching sessions.".into(),
            },
        }],
        ..Default::default()
    });
    engine.view = View::DeleteMod(0);
    let actual = serde_json::to_value(engine.response(Some(7), None)).unwrap();
    assert_eq!(
        actual["snapshot"]["active_mod"]["messages"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        actual["snapshot"]["active_mod"]["history"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/desktop.json");
    if std::env::var_os("SPROWT_DESKTOP_FIXTURE").is_some() {
        std::fs::write(
            &fixture,
            serde_json::to_string_pretty(&actual).unwrap() + "\n",
        )
        .unwrap();
    }
    let expected: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&fixture).unwrap()).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn ownership_excludes_a_separate_process() {
    const PROBE: &str = "SPROWT_OWNERSHIP_PROBE";
    if let Some(directory) = std::env::var_os(PROBE) {
        let root = PathBuf::from(directory);
        let error = ownership::acquire(&root.join("state.db"), &root.join("project")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        return;
    }
    let data = TestData::new();
    let _owner = ownership::acquire(&data.0.join("state.db"), &data.0.join("project")).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "engine::tests::ownership_excludes_a_separate_process",
            "--nocapture",
        ])
        .env(PROBE, &data.0)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
