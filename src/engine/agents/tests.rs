use super::*;
use crate::{engine::Engine, plan::Role, store::test_support::TestData, worker::Worker};

pub(crate) const FAILURE: &str = "Environment preflight failed: Apple Container failed.\nTraceback (most recent call last):\n  File \"<string>\", line 45, in visit\nValueError: Source file exceeds 64 MiB: .cache/ms-playwright/chromium-1193/chrome-linux/chrome (408.16 MiB).";

pub(crate) fn fixture() -> (TestData, Engine) {
    let data = TestData::new();
    let project = data.0.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let mut store = data.store();
    let project_id = store.load_project(&project).unwrap().id;
    let m = store
        .create_mod(project_id, "Add Searchable PDF Task Attachments")
        .unwrap();
    let plan = crate::plan::Plan::parse_generated(r#"{"title":"Add Searchable PDF Task Attachments","summary":"PDF attachments","tasks":[{"id":"extract","title":"Extract and embed PDF passages","outcome":"Extract PDF text","files":["extract.js"],"depends_on":[],"worker":"codex","checks":["PDF text is extracted"]},{"id":"persist","title":"Persist searchable task attachments","outcome":"Persist PDFs","files":["storage.js"],"depends_on":[],"worker":"codex","checks":["PDFs survive reopening"]},{"id":"ui","title":"Add task PDF controls","outcome":"Attach PDFs","files":["ui.js"],"depends_on":[],"worker":"codex","checks":["Controls accept PDFs"]}]}"#).unwrap();
    store
        .save_plan(m.id, &m.planning.as_ref().unwrap().source, &plan)
        .unwrap();
    store
        .create_execution(m.id, &data.0.join("workspace"), &plan)
        .unwrap();
    store
        .save_draft(m.id, "Keep this unsent draft intact.")
        .unwrap();
    let mut engine = Engine::load(project, store).unwrap();
    engine.mods[0].name = "Add Searchable PDF Task Attachments".into();
    for index in 0..3 {
        let record = engine.store.worker_at(m.id, Role::Executor, index).unwrap();
        let mut worker = Worker::start(
            &engine.project,
            &engine.mods[0],
            record,
            Role::Planner,
            None,
        )
        .unwrap();
        worker.role = Role::Executor;
        worker.status = if index == 0 {
            Status::Ready
        } else {
            Status::Running
        };
        worker.enabled = index != 0;
        worker.model = Some("gpt-6.1-sol".into());
        worker.effort = Some("xhigh".into());
        let task = &mut engine.mods[0].execution.as_mut().unwrap().tasks[index];
        task.worker = Some(worker.id);
        task.status = if index == 0 { "paused" } else { "running" }.into();
        if index == 0 {
            task.summary = FAILURE.into();
            worker.error = Some(FAILURE.into());
        }
        worker.bind_test_task(task.source.clone());
        engine.workers.insert(worker.id, worker);
    }
    for (phase, duration_ms) in [
        (crate::diagnostics::timing::Phase::DependencySetup, 316_000),
        (crate::diagnostics::timing::Phase::SlotWait, 103_000),
        (crate::diagnostics::timing::Phase::Source, 61_000),
        (crate::diagnostics::timing::Phase::StartupWait, 47_400),
        (
            crate::diagnostics::timing::Phase::ProviderConnection,
            25_100,
        ),
        (crate::diagnostics::timing::Phase::Capabilities, 11_200),
        (crate::diagnostics::timing::Phase::FirstResponse, 3900),
        (crate::diagnostics::timing::Phase::Boot, 1800),
        (crate::diagnostics::timing::Phase::BaseSetup, 500),
        (crate::diagnostics::timing::Phase::ImageWait, 0),
    ] {
        engine.mods[0].diagnostics.timings.push((
            0,
            crate::diagnostics::timing::Observation {
                phase,
                duration_ms,
                source: "fixture".into(),
                environment: None,
                completed: true,
                attempt: None,
            },
        ));
    }
    (data, engine)
}

#[test]
fn agents_present_current_task_failure_instead_of_retained_runtime_ready() {
    let (_data, engine) = fixture();
    let agents = engine.agents().unwrap();
    assert_eq!(agents.summary, "Task 1 needs attention · 2 workers running");
    assert_eq!(agents.workers[0].title, "Extract and embed PDF passages");
    assert_eq!(agents.workers[0].state, "Needs attention");
    assert_eq!(
        agents.workers[0].failure.as_ref().unwrap().summary,
        "Browser download exceeds the source-file limit."
    );
    assert_eq!(agents.workers[0].failure.as_ref().unwrap().detail, FAILURE);
    assert_eq!(agents.workers[1].state, "Working");
    assert!(agents.workers[1].activity.is_empty());
    assert!(agents.workers[1].previous_diagnostic.is_none());
    assert!(agents.owns_error(FAILURE));
    assert!(!agents.owns_error("Unable to save the project database."));
    assert_eq!(agents.timings[0].label, "Dependency setup");
    assert_eq!(agents.timings[0].duration, "5m 16s");
    assert_eq!(agents.timings[0].duration_ms, 316_000);
    if let Ok(path) = std::env::var("SPROWT_AGENTS_FIXTURE") {
        export_snapshot(&engine, &path);
    }
}

#[test]
fn agents_never_promote_a_previous_attempt_error_or_ready_worker_to_working() {
    let (_data, mut engine) = fixture();
    let run = &mut engine.mods[0].execution.as_mut().unwrap().tasks[0];
    run.status = "pending".into();
    run.summary.clear();
    run.source = crate::store::task_source(run.id, 2);
    let agents = engine.agents().unwrap();
    assert_eq!(agents.workers[0].state, "Ready");
    assert!(agents.workers[0].failure.is_none());
    assert_eq!(
        agents.workers[0].previous_diagnostic.as_deref(),
        Some(FAILURE)
    );
    assert!(!agents.owns_error(FAILURE));
    engine.mods[0].user_paused = true;
    assert_eq!(engine.agents().unwrap().workers[0].state, "Paused");
}

#[test]
fn agents_keep_role_only_failures_and_do_not_invent_a_task_assignment() {
    let (_data, mut engine) = fixture();
    engine.mods[0].execution = None;
    let worker = engine.workers.values_mut().next().unwrap();
    worker.role = Role::Planner;
    worker.status = Status::Failed;
    worker.error = Some("Model quota is exhausted.\nRetry after reset.".into());
    let agents = engine.agents().unwrap();
    let agent = &agents.workers[0];
    assert_eq!(agent.task_number, None);
    assert_eq!(agent.worker.role, "planner");
    assert_eq!(agent.state, "Needs attention");
    assert_eq!(
        agent.failure.as_ref().unwrap().detail,
        "Model quota is exhausted.\nRetry after reset."
    );
    assert!(agent.previous_diagnostic.is_none());
    assert_eq!(agents.workers[1].state, "Working");
}

#[test]
fn agents_distinguish_current_failed_checks_from_previous_attempt_feedback() {
    let (_data, mut engine) = fixture();
    let run = &mut engine.mods[0].execution.as_mut().unwrap().tasks[0];
    run.summary.clear();
    run.checks.push(crate::execution::CheckResult {
        task: Some(run.id),
        check: "PDF storage".into(),
        command: vec!["test".into()],
        exit_code: Some(1),
        output: "Error: PDF did not survive reopening".into(),
        timeout_seconds: None,
        duration_ms: None,
        missing_runtime: None,
    });
    let agents = engine.agents().unwrap();
    assert_eq!(agents.workers[0].state, "Needs attention");
    assert_eq!(
        agents.workers[0].failure.as_ref().unwrap().detail,
        "Error: PDF did not survive reopening"
    );
    let run = &mut engine.mods[0].execution.as_mut().unwrap().tasks[0];
    run.status = "pending".into();
    run.verification_feedback = std::mem::take(&mut run.checks);
    let agents = engine.agents().unwrap();
    assert_eq!(agents.workers[0].state, "Ready");
    assert!(agents.workers[0].failure.is_none());
    assert!(!agents.owns_error("Error: PDF did not survive reopening"));
}

pub(crate) fn active_fixture() -> (TestData, Engine) {
    let (data, mut engine) = fixture();
    let mut record = engine
        .store
        .worker_at(engine.mods[0].id, Role::Executor, 1)
        .unwrap();
    record.id = 80;
    let (mut worker, events, _actions) =
        Worker::recording(&engine.project, &engine.mods[0], record).unwrap();
    worker.role = Role::Executor;
    worker.status = Status::Starting;
    worker.model = Some("gpt-6.1-sol".into());
    worker.effort = Some("xhigh".into());
    let execution = engine.mods[0].execution.as_mut().unwrap();
    for (index, task) in execution.tasks.iter_mut().enumerate() {
        task.status = if index == 1 { "running" } else { "done" }.into();
        task.summary.clear();
        task.worker = (index == 1).then_some(80);
    }
    worker.bind_test_task(execution.tasks[1].source.clone());
    events
        .send(crate::codex::Event::Preparing(
            "Preflight · baseline · Existing source-only Node baseline".into(),
        ))
        .unwrap();
    worker
        .poll(
            &mut engine.store,
            &mut engine.mods[0],
            false,
            &engine.project,
        )
        .unwrap();
    engine.workers.clear();
    engine.workers.insert(worker.id, worker);
    engine.mods[0].diagnostics.timings.clear();
    use crate::diagnostics::timing::{Observation, Phase};
    for (phase, duration_ms) in [
        (Phase::DependencySetup, 409000),
        (Phase::Source, 166000),
        (Phase::SlotWait, 162000),
        (Phase::StartupWait, 95000),
        (Phase::FirstResponse, 71000),
        (Phase::Baseline, 34700),
        (Phase::ProviderConnection, 31200),
        (Phase::Capabilities, 16400),
        (Phase::Boot, 4200),
        (Phase::BaseSetup, 1400),
        (Phase::ImageWait, 0),
    ] {
        engine.mods[0].diagnostics.timings.push((
            80,
            Observation {
                phase,
                duration_ms,
                source: "fixture".into(),
                environment: None,
                completed: true,
                attempt: None,
            },
        ));
    }
    (data, engine)
}

#[test]
fn agents_share_task_focus_metadata_activity_and_relative_timings() {
    let (_data, mut engine) = active_fixture();
    let agents = engine.agents().unwrap();
    let agent = &agents.workers[0];
    assert_eq!(agent.task_number, Some(2));
    assert_eq!(agent.state, "Preparing");
    assert_eq!(agent.worker_label, "Codex · w80");
    assert_eq!(agent.model, "gpt-6.1-sol");
    assert_eq!(agent.reasoning, "Extra high");
    assert_eq!(agent.activity_preview, "Checking the existing baseline");
    assert_eq!(
        agent.output,
        "Preflight · baseline · Existing source-only Node baseline"
    );
    assert_eq!(agents.timings[0].relative_duration, 1.0);
    assert_eq!(agents.timings[0].duration, "6m 49s");
    assert_eq!(agents.timings[1].relative_duration, 166.0 / 409.0);
    assert_eq!(agents.timings.last().unwrap().relative_duration, 0.0);
    if let Ok(path) = std::env::var("SPROWT_TASK_FOCUS_FIXTURE") {
        export_snapshot(&engine, &path);
    }
    let worker = engine.workers.get_mut(&80).unwrap();
    worker.status = Status::Ready;
    worker.model = None;
    worker.effort = None;
    let agent = engine.agents().unwrap().workers.remove(0);
    assert_eq!(agent.model, "Not reported");
    assert_eq!(agent.reasoning, "Not reported");
    assert!(agent.output.is_empty());
}

fn export_snapshot(engine: &Engine, path: &str) {
    let mut snapshot = serde_json::to_value(engine.snapshot()).unwrap();
    snapshot["projects"] = serde_json::json!([{"id":engine.project_id,"path":engine.project,"name":"todo","activity":"working","mods":[{"id":engine.mods[0].id,"name":engine.mods[0].name,"closed":false}]}]);
    snapshot["expanded_project"] = serde_json::json!(engine.project_id);
    std::fs::write(path, serde_json::to_vec_pretty(&serde_json::json!({"protocol_version":1,"request_id":null,"error":null,"snapshot":snapshot})).unwrap()).unwrap();
}

#[test]
fn activity_labels_only_translate_known_preflight_phases() {
    assert_eq!(
        activity_summary("Preflight · capabilities · Browser OPFS"),
        "Checking environment capabilities"
    );
    assert_eq!(
        activity_summary("Preflight · setup · npm ci"),
        "Preparing dependencies"
    );
    assert_eq!(
        activity_summary("Waiting for a provider response"),
        "Waiting for a provider response"
    );
    assert_eq!(
        activity_summary("Custom status\nfull diagnostic below"),
        "Custom status"
    );
}
