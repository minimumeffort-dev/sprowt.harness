use super::*;
use crate::{
    codex::Client,
    plan::{Plan, Role},
    store::test_support::TestData,
    worker_settings::{ModelChoice, Provider},
    workspace,
};
use std::time::Instant;

#[test]
#[ignore = "Runs selected Muse Wonder planner in a disposable read-only VM with the signed-in subscription"]
fn selected_muse_planner_reads_source_and_returns_a_plan() {
    let data = TestData::new();
    let project = data.0.join("project");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("hello.py"), "print('hello')\n").unwrap();
    fs::write(project.join("README.md"), "Run python3 hello.py\n").unwrap();
    fs::write(
        project.join(".env.local"),
        "PRIVATE_TEST_CANARY=not-a-real-secret\n",
    )
    .unwrap();
    let mut store = data.store();
    let project_id = store.load_project(&project).unwrap().id;
    let mut m = store
        .create_mod(
            project_id,
            "Update hello.py to print hello sprowt and document it in README.md.",
        )
        .unwrap();
    let root = store.workspace_path(m.id).unwrap();
    workspace::create(&project, &root).unwrap();
    store.save_git_root(m.id, &root).unwrap();
    m.git_root = Some(root.clone());
    let source = m.planning.as_ref().unwrap().source.clone();
    let result = run(
        &project,
        &mut store,
        &m,
        Role::Planner,
        &source,
        &m.description,
        None,
    );
    let plan = Plan::parse_generated(&result).unwrap();
    assert!(
        plan.tasks
            .iter()
            .any(|t| t.files.contains(&"hello.py".into()))
    );
}

#[test]
#[ignore = "Runs selected Muse Wonder reviewer against a known defect in a disposable read-only VM"]
fn selected_muse_reviewer_finds_defect_without_editing_source() {
    let (data, mut store, mut m, plan) = crate::review::tests::fixture();
    let fingerprint = m.execution.as_ref().unwrap().fingerprint.clone().unwrap();
    store.begin_review(m.id, &fingerprint).unwrap();
    m.agent_review = store.review_state(m.id).unwrap();
    let source = m.agent_review.as_ref().unwrap().source.clone();
    let input = store.review_input(&m).unwrap().unwrap().texts.join("\n\n");
    let output = run(
        &data.0.join("project"),
        &mut store,
        &m,
        Role::Reviewer,
        &source,
        &input,
        Some(&plan),
    );
    let report: crate::review::Report = serde_json::from_str(&output).unwrap();
    assert!(
        report.findings.iter().any(|f| f.file == "greet.sh"),
        "{output}"
    );
}

fn run(
    project: &std::path::Path,
    store: &mut crate::store::Store,
    m: &crate::store::CodeMod,
    role: Role,
    source: &str,
    input: &str,
    plan: Option<&Plan>,
) -> String {
    let choice = ModelChoice {
        provider: Provider::Muse,
        model: "muse-wonder-1.0".into(),
        effort: "high".into(),
    };
    let worker = store.new_model_worker(m.id, role, &choice).unwrap();
    let mut context = Context::worker(m, worker.id, role).with_mailbox(&worker.database);
    context.provider = "muse".into();
    context.muse = true;
    let root = context.workspace().unwrap().to_owned();
    let before = workspace::source_state(&root.join("work")).unwrap();
    let original = workspace::source_state(project).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut client = Client::start(
            project,
            None,
            role,
            &m.description,
            plan,
            Some(choice.selection()),
            context,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(240);
        let mut output = None;
        let mut observed_messages = Vec::new();
        let mut complete = false;
        while Instant::now() < deadline && !complete {
            for event in client.poll() {
                match event {
                    Event::Ready { model, effort, .. } => {
                        assert_eq!(model.as_deref(), Some("muse-wonder-1.0"));
                        assert_eq!(effort.as_deref(), Some("high"));
                        client
                            .send(Action::Run {
                                source: source.into(),
                                text: input.into(),
                                routing: plan.map(|p| serde_json::to_value(p).unwrap()),
                            })
                            .unwrap();
                    }
                    Event::Notification(message) if message["method"] == "item/completed" => {
                        let item = &message["params"]["item"];
                        observed_messages.push(json!({"type":item["type"], "phase":item["phase"],
                            "report_fields":item["text"].as_str().and_then(|s|serde_json::from_str::<Value>(s).ok()).and_then(|v|v.as_object().map(|o|o.keys().cloned().collect::<Vec<_>>()))}));
                        if item["type"] == "agentMessage" && item["phase"] != "commentary" {
                            output = item["text"].as_str().map(str::to_owned);
                        }
                    }
                    Event::Notification(message) if message["method"] == "turn/completed" => {
                        assert_eq!(
                            message["params"]["turn"]["status"], "completed",
                            "{message}"
                        );
                        complete = true;
                    }
                    Event::Failed(error) | Event::Rejected { message: error, .. } => {
                        panic!("{error}")
                    }
                    Event::QuotaRejected { .. } => panic!("Live subscription quota exhausted"),
                    _ => {}
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
        client.shutdown();
        assert!(complete, "Muse role turn timed out");
        assert!(before == workspace::source_state(&root.join("work")).unwrap());
        assert!(original == workspace::source_state(project).unwrap());
        output.unwrap_or_else(|| {
            panic!("Muse returned no final role report; observed messages: {observed_messages:?}")
        })
    }));
    crate::sandbox::delete(&root).unwrap();
    result.unwrap()
}
