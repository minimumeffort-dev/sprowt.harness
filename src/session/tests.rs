use super::*;
use crate::{
    engine::{Destination, View},
    store::test_support::TestData,
};

fn project_session() -> (TestData, Session, i64, i64) {
    let data = TestData::new();
    let path = data.0.join("project");
    std::fs::create_dir_all(&path).unwrap();
    let path = path.canonicalize().unwrap();
    let mut store = data.store();
    let id = store.load_project(&path).unwrap().id;
    let first = store.create_mod(id, "First change").unwrap().id;
    let second = store.create_mod(id, "Second change").unwrap().id;
    store.select_mod(id, first).unwrap();
    let mut engine = Engine::load(path.clone(), store).unwrap();
    engine._ownership = Some(ownership::acquire(&data.0.join("state.db"), &path).unwrap());
    (data, Session::from_engine(engine).unwrap(), first, second)
}

fn second_project(data: &TestData, engine: &mut Session) -> (i64, i64, PathBuf) {
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
fn removing_projects_keeps_running_work_and_explicit_open_restores_the_same_session() {
    use crate::{codex::Event, worker::Worker};
    let (data, mut session, first, _) = project_session();
    let a = session.project_id;
    let path = session.project.clone();
    let (b, _, other) = second_project(&data, &mut session);
    session
        .command(Command::SetDraft {
            mod_id: first,
            text: "Keep this draft".into(),
        })
        .unwrap();
    let record = session.store.worker(first).unwrap();
    let worker_id = record.id;
    let (worker, events, actions) = Worker::recording(&path, &session.mods[0], record).unwrap();
    session.workers.insert(worker_id, worker);
    let request: crate::engine::api::Request = serde_json::from_value(
        serde_json::json!({"id":1,"command":{"type":"remove_project","project_id":a}}),
    )
    .unwrap();
    session.command(request.command).unwrap();
    assert_eq!(
        session.projects.iter().map(|p| p.id).collect::<Vec<_>>(),
        [b]
    );
    assert_eq!(session.expanded_project, None);
    assert_eq!(session.project_id, a);
    assert_eq!(session.composer, "Keep this draft");
    session
        .command(Command::OpenProject {
            path: path.display().to_string(),
        })
        .unwrap();
    assert_eq!(
        session.projects.iter().map(|p| p.id).collect::<Vec<_>>(),
        [a, b]
    );
    session
        .command(Command::SelectProject { project_id: b })
        .unwrap();
    session
        .command(Command::RemoveProject { project_id: a })
        .unwrap();
    events
        .send(Event::Ready {
            thread: serde_json::json!({"id":"hidden-project-worker"}),
            model: Some("same-model".into()),
            effort: None,
        })
        .unwrap();
    session.poll_workers().unwrap();
    assert_eq!(
        session.projects.iter().map(|p| p.id).collect::<Vec<_>>(),
        [b]
    );
    assert_eq!(
        session.background[&a].workers[&worker_id].model.as_deref(),
        Some("same-model")
    );
    assert_eq!(
        session.store.worker(first).unwrap().thread_id.as_deref(),
        Some("hidden-project-worker")
    );
    assert_eq!(session.project_id, b);
    session
        .command(Command::OpenProject {
            path: path.display().to_string(),
        })
        .unwrap();
    assert_eq!(
        session.projects.iter().map(|p| p.id).collect::<Vec<_>>(),
        [a, b]
    );
    assert_eq!(session.composer, "Keep this draft");
    assert_eq!(
        session.workers[&worker_id].model.as_deref(),
        Some("same-model")
    );
    assert!(
        actions.try_recv().is_err(),
        "Removing and restoring a project must not stop or restart workers"
    );
    session
        .command(Command::RemoveProject { project_id: b })
        .unwrap();
    session
        .command(Command::OpenProject {
            path: other.display().to_string(),
        })
        .unwrap();
    assert_eq!(session.project_id, b);
    assert_eq!(
        session.projects.iter().map(|p| p.id).collect::<Vec<_>>(),
        [a, b]
    );
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
fn project_switch_rejects_missing_locked_and_confirming_targets_without_losing_state() {
    let (data, mut engine, first, _) = project_session();
    let a = engine.project_id;
    let (b, _, path) = second_project(&data, &mut engine);
    engine
        .command(Command::SetDraft {
            mod_id: first,
            text: "Keep this instruction".into(),
        })
        .unwrap();
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
fn opened_projects_keep_their_locks_and_history_until_shutdown() {
    let (data, mut engine, first, second) = project_session();
    let a = engine.project_id;
    let path = engine.project.clone();
    let (b, _, other) = second_project(&data, &mut engine);
    engine
        .command(Command::SelectMod { mod_id: second })
        .unwrap();
    engine.command(Command::BeginCreation {}).unwrap();
    engine
        .command(Command::SelectProject { project_id: b })
        .unwrap();
    assert_eq!(
        ownership::acquire(&data.0.join("state.db"), &path)
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    engine
        .command(Command::SelectProject { project_id: a })
        .unwrap();
    engine.command(Command::GoBack {}).unwrap();
    assert_eq!(engine.current_mod().unwrap().id, second);
    engine.command(Command::GoBack {}).unwrap();
    assert_eq!(engine.current_mod().unwrap().id, first);
    drop(engine);
    let _a = ownership::acquire(&data.0.join("state.db"), &path).unwrap();
    let _b = ownership::acquire(&data.0.join("state.db"), &other).unwrap();
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

#[test]
fn background_worker_receipts_and_git_results_progress_without_restarting_or_taking_focus() {
    use crate::{
        codex::Event,
        worker::{Status, Worker},
    };
    let (data, mut session, first, _) = project_session();
    let a = session.project_id;
    let (b, second, _) = second_project(&data, &mut session);
    let record = session.store.worker(first).unwrap();
    let worker_id = record.id;
    let (worker, events, actions) =
        Worker::recording(&session.project, &session.mods[0], record).unwrap();
    session.workers.insert(worker_id, worker);
    let (release, gate) = std::sync::mpsc::channel();
    session.project_error = Some("Old sync error".into());
    session.project_job = Some(git_mod::Job::start(
        "syncing project branch",
        move |cancelled| {
            gate.recv().unwrap();
            assert!(!cancelled.load(std::sync::atomic::Ordering::Relaxed));
            Ok(git_mod::Result::ProjectSynced)
        },
    ));
    session
        .command(Command::SelectMod { mod_id: second })
        .unwrap();
    session
        .command(Command::SetDraft {
            mod_id: second,
            text: "B stays focused".into(),
        })
        .unwrap();
    events
        .send(Event::Ready {
            thread: serde_json::json!({"id":"same-provider-session"}),
            model: Some("model-from-provider".into()),
            effort: Some("high".into()),
        })
        .unwrap();
    release.send(()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while session.background[&a].project_job.is_some() {
        assert!(std::time::Instant::now() < deadline);
        session.poll_workers().unwrap();
        std::thread::yield_now();
    }
    assert_eq!(session.project_id, b);
    assert_eq!(session.composer, "B stays focused");
    let runtime = &session.background[&a];
    assert_eq!(
        runtime.store.worker(first).unwrap().thread_id.as_deref(),
        Some("same-provider-session")
    );
    assert_eq!(
        runtime.workers[&worker_id].model.as_deref(),
        Some("model-from-provider")
    );
    assert!(runtime.workers[&worker_id].status == Status::Ready);
    assert_eq!(runtime.project_error, None);
    assert!(
        actions.try_recv().is_err(),
        "Switching must not send stop or restart commands"
    );
    session
        .command(Command::SelectProject { project_id: a })
        .unwrap();
    assert_eq!(
        session.workers[&worker_id].model.as_deref(),
        Some("model-from-provider")
    );
    assert_eq!(session.workers.len(), 1);
    session
        .command(Command::SelectProject { project_id: b })
        .unwrap();
    events
        .send(Event::Failed("Provider quota exhausted".into()))
        .unwrap();
    session.poll_workers().unwrap();
    let snapshot = serde_json::to_value(session.response(None, None)).unwrap();
    assert_eq!(snapshot["snapshot"]["project_id"], b);
    assert_eq!(snapshot["snapshot"]["projects"][0]["activity"], "attention");
    assert!(snapshot["snapshot"]["dialog"].is_null());
    assert_eq!(session.composer, "B stays focused");
    session
        .command(Command::SelectProject { project_id: a })
        .unwrap();
    assert_eq!(
        session.workers[&worker_id].error.as_deref(),
        Some("Provider quota exhausted")
    );
}

#[test]
fn one_projects_poll_failure_does_not_stop_other_projects() {
    let (data, mut session, _, _) = project_session();
    let a = session.project_id;
    let (b, _, _) = second_project(&data, &mut session);
    session
        .command(Command::SelectProject { project_id: b })
        .unwrap();
    let background = session.background.get_mut(&a).unwrap();
    let id = background.mods[0].id;
    let record = background.store.worker(id).unwrap();
    let (worker, events, _actions) =
        crate::worker::Worker::recording(&background.project, &background.mods[0], record).unwrap();
    background.workers.insert(worker.id, worker);
    events
        .send(crate::codex::Event::Ready {
            thread: serde_json::json!({"id":"save-fails"}),
            model: None,
            effort: None,
        })
        .unwrap();
    // Only this connection is read-only; other project runtimes still write normally.
    session.background[&a]
        .store
        .0
        .pragma_update(None, "query_only", true)
        .unwrap();
    session.project_error = Some("Old error".into());
    session.project_job = Some(git_mod::Job::start("syncing project branch", |_| {
        Ok(git_mod::Result::ProjectSynced)
    }));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while session.project_job.is_some() {
        assert!(std::time::Instant::now() < deadline);
        session.poll_workers().unwrap();
        std::thread::yield_now();
    }
    assert_eq!(session.project_error, None);
    assert!(
        session.background[&a]
            .poll_error
            .as_deref()
            .unwrap()
            .contains("readonly")
    );
    assert_eq!(session.project_id, b);
    session.background[&a]
        .store
        .0
        .pragma_update(None, "query_only", false)
        .unwrap();
    session.poll_workers().unwrap();
    assert_eq!(session.background[&a].poll_error, None);
    let worker = session.background[&a].workers.values().next().unwrap();
    assert!(worker.status == crate::worker::Status::Failed && !worker.enabled);
    assert!(
        worker
            .error
            .as_deref()
            .unwrap()
            .contains("Could not save worker progress")
    );
    session
        .command(Command::SelectProject { project_id: a })
        .unwrap();
    let snapshot = serde_json::to_value(session.response(None, None)).unwrap();
    assert!(
        snapshot["snapshot"]["actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == "retry")
    );
}

#[test]
fn dropping_session_cancels_jobs_and_releases_every_project_lock() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let (data, mut session, _, _) = project_session();
    let first_path = session.project.clone();
    let (b, _, second_path) = second_project(&data, &mut session);
    let mut completed = Vec::new();
    for destination in [None, Some(b)] {
        if let Some(id) = destination {
            session
                .command(Command::SelectProject { project_id: id })
                .unwrap();
        }
        let done = Arc::new(AtomicBool::new(false));
        completed.push(done.clone());
        session.project_job = Some(git_mod::Job::start(
            "syncing project branch",
            move |cancelled| {
                while !cancelled.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                done.store(true, Ordering::Relaxed);
                Err(io::Error::from(io::ErrorKind::Interrupted))
            },
        ));
    }
    drop(session);
    assert!(completed.iter().all(|done| done.load(Ordering::Relaxed)));
    let _first = ownership::acquire(&data.0.join("state.db"), &first_path).unwrap();
    let _second = ownership::acquire(&data.0.join("state.db"), &second_path).unwrap();
}

#[test]
fn preparation_completion_is_polled_after_leaving_its_project() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let (data, mut session, first, _) = project_session();
    let a = session.project_id;
    let (b, _, _) = second_project(&data, &mut session);
    let release = Arc::new(AtomicBool::new(false));
    let gate = release.clone();
    session.preparations =
        crate::engine::preparation::Preparations::with_backend(Arc::new(move |_, cancelled, _| {
            while !gate.load(Ordering::Relaxed) {
                assert!(!cancelled.load(Ordering::Relaxed));
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(io::Error::other("Preparation result from background"))
        }));
    session
        .foreground
        .preparations
        .root(first, &session.foreground.store, None)
        .unwrap();
    session.preparations.start(first);
    session
        .command(Command::SelectProject { project_id: b })
        .unwrap();
    release.store(true, Ordering::Relaxed);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while session.background[&a].preparations.warming() {
        assert!(std::time::Instant::now() < deadline);
        session.poll_workers().unwrap();
        std::thread::yield_now();
    }
    assert_eq!(session.project_id, b);
    assert_eq!(
        session.store.diagnostics(first).unwrap().failures[0].evidence,
        "Preparation result from background"
    );
    assert_eq!(
        session.store.planning(first).unwrap().unwrap().status,
        "pending"
    );
}

#[test]
fn background_network_request_waits_for_its_own_explicit_approval() {
    let (data, store, m, root, workers) = crate::network::tests::fixture();
    let mut session =
        Session::from_engine(Engine::load(data.0.join("project"), store).unwrap()).unwrap();
    let a = session.project_id;
    let (b, second, _) = second_project(&data, &mut session);
    session
        .command(Command::SelectMod { mod_id: second })
        .unwrap();
    session
        .command(Command::SetDraft {
            mod_id: second,
            text: "No approval here".into(),
        })
        .unwrap();
    let source = &m.execution.as_ref().unwrap().tasks[0].source;
    let policy = crate::sandbox::network(&root).unwrap();
    let request = crate::network::Request::parse(
        serde_json::json!({"domains":["models.example.org"],"reason":"Download test fixture"}),
    )
    .unwrap();
    crate::network::require_preparation(
        &session.store,
        &root,
        m.id,
        workers[0],
        source,
        request,
        &policy,
    )
    .unwrap_err();
    session.poll_workers().unwrap();
    assert_eq!(session.project_id, b);
    assert_eq!(session.composer, "No approval here");
    let snapshot = serde_json::to_value(session.response(None, None)).unwrap();
    assert_eq!(snapshot["snapshot"]["projects"][0]["activity"], "attention");
    assert!(snapshot["snapshot"]["dialog"].is_null());
    assert!(crate::network::grants(&root).unwrap().is_empty());
    session
        .command(Command::SelectMod { mod_id: m.id })
        .unwrap();
    assert_eq!(session.project_id, a);
    session
        .command(Command::Action {
            mod_id: Some(m.id),
            action: "network".into(),
            task_id: None,
        })
        .unwrap();
    let snapshot = serde_json::to_value(session.response(None, None)).unwrap();
    assert_eq!(snapshot["snapshot"]["dialog"]["kind"], "network");
    let id = snapshot["snapshot"]["dialog"]["request_id"]
        .as_i64()
        .unwrap();
    session
        .command(Command::NetworkDecision {
            mod_id: m.id,
            request_id: id,
            allow: false,
        })
        .unwrap();
    assert_eq!(
        session.store.network_requests(m.id).unwrap()[0].status,
        "denied"
    );
    assert!(crate::network::grants(&root).unwrap().is_empty());
}

#[test]
fn switching_reuses_the_latest_appearance_without_waiting_for_a_poll() {
    use crate::appearance::Appearance;
    let (data, mut session, _, _) = project_session();
    let a = session.project_id;
    let (b, _, _) = second_project(&data, &mut session);
    session
        .command(Command::SetAppearance {
            appearance: Appearance::Light,
        })
        .unwrap();
    session
        .command(Command::SelectProject { project_id: b })
        .unwrap();
    // The terminal uses the same native Rust setter as its appearance picker.
    session.set_appearance(Appearance::Dark).unwrap();
    session
        .command(Command::SelectProject { project_id: a })
        .unwrap();
    assert_eq!(session.appearance, Appearance::Dark);
    let snapshot = serde_json::to_value(session.response(None, None)).unwrap();
    assert_eq!(snapshot["snapshot"]["appearance"], "dark");
    session
        .command(Command::SetAppearance {
            appearance: Appearance::Light,
        })
        .unwrap();
    session
        .command(Command::SelectProject { project_id: b })
        .unwrap();
    assert_eq!(session.appearance, Appearance::Light);
}

#[test]
fn leaving_read_only_inspection_does_not_withhold_background_worker_dispatch() {
    let (data, mut session, first, _) = project_session();
    let a = session.project_id;
    let (b, _, _) = second_project(&data, &mut session);
    session
        .command(Command::SetDraft {
            mod_id: first,
            text: "A draft before inspection".into(),
        })
        .unwrap();
    let record = session.store.worker(first).unwrap();
    let (mut worker, events, actions) =
        crate::worker::Worker::recording(&session.project, session.current_mod().unwrap(), record)
            .unwrap();
    worker.enabled = true;
    let worker_id = worker.id;
    session.workers.insert(worker_id, worker);
    session.view = View::Review(0);
    session
        .command(Command::SelectProject { project_id: b })
        .unwrap();
    events
        .send(crate::codex::Event::Ready {
            thread: serde_json::json!({"id":"inspection-worker"}),
            model: None,
            effort: None,
        })
        .unwrap();
    session.poll_workers().unwrap();
    let crate::codex::Action::Run { text, .. } = actions.try_recv().unwrap() else {
        panic!("Expected queued planner work");
    };
    assert!(text.contains("First change"));
    assert!(session.background[&a].workers[&worker_id].status == crate::worker::Status::Starting);
    assert_eq!(session.project_id, b);
    session
        .command(Command::SelectProject { project_id: a })
        .unwrap();
    assert!(session.view == View::Chat);
    assert_eq!(session.composer, "A draft before inspection");
}
