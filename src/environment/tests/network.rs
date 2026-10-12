use super::*;

#[test]
#[ignore = "checks preparation approval, baseline gating and both provider sessions in a disposable VM; no downloads or inference"]
fn live_preparation_network_requests_pause_commands_and_resume_scoped_attempts() {
    let (_data, mut store, m, root, workers) = crate::network::tests::fixture();
    fs::remove_dir(root.join("work")).unwrap();
    let source = vec![(PathBuf::from("source.txt"), b"original".to_vec(), 0o644)];
    workspace::create_snapshot(&source, &root).unwrap();
    let flag = AtomicBool::new(false);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> io::Result<()> {
        let mut controller = Sandbox::prepare(&root, &flag, |_| {})?;
        let ids = m
            .execution
            .as_ref()
            .unwrap()
            .tasks
            .iter()
            .map(|run| run.id)
            .collect::<Vec<_>>();
        controller.prepare_tasks(&ids, &flag)?;
        let vm = std::sync::Arc::new(std::sync::Mutex::new(controller));
        for (index, worker) in workers.iter().enumerate() {
            let mut context =
                crate::tools::Context::worker(&m, *worker, crate::plan::Role::Executor)
                    .with_mailbox(Path::new(store.0.path().unwrap()));
            context.muse = index == 1;
            let mut attempt = m.execution.as_ref().unwrap().tasks[index].source.clone();
            let recipe: Recipe = serde_json::from_value(json!({
                "inputs": [], "setup": [{"name":"Prepare verified fixture","argv":["/bin/sh","-c","printf ready > \"$HOME/model-ready\""],"timeout_seconds":30,"network_domains":[format!("models-{index}.example.org")]}],
                "capabilities": [{"name":"Fixture is ready","argv":["/bin/sh","-c","test \"$(cat \"$HOME/model-ready\")\" = ready"],"timeout_seconds":30}],
                "baseline": [{"name":"Starting assertion failure","argv":["/bin/sh","-c","printf baseline-ran; exit 1"],"timeout_seconds":30,"network_domains":[format!("baseline-{index}.example.org")]}]
            }))?;
            for (phase, domain) in [
                ("setup", format!("models-{index}.example.org")),
                ("baseline", format!("baseline-{index}.example.org")),
            ] {
                let mut session =
                    crate::sandbox::session::prepare_task(&vm, &context, &attempt, &flag)?;
                let error = session
                    .preflight(Some(&recipe), &flag, |_| {}, Timing::quiet())
                    .unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
                assert!(error.to_string().contains(&domain));
                let id = ids[index];
                let receipt = load(&root, id)?.unwrap();
                assert!(receipt.baseline.is_empty());
                if phase == "setup" {
                    assert!(receipt.setup.is_empty());
                    let check = Check {
                        task: None,
                        check: "No setup side effect before approval".into(),
                        command: vec![
                            "/bin/sh".into(),
                            "-c".into(),
                            "test ! -e \"$HOME/model-ready\"".into(),
                        ],
                        timeout_seconds: Some(30),
                    };
                    assert_eq!(session.command(&check, &flag).exit_code, Some(0));
                } else {
                    assert_eq!(receipt.setup[0].exit_code, Some(0));
                    assert_eq!(receipt.capabilities[0].exit_code, Some(0));
                }
                drop(session);
                let access = store
                    .network_requests(m.id)
                    .unwrap()
                    .into_iter()
                    .find(|access| access.task == id && access.status == "pending")
                    .unwrap();
                assert_eq!(access.domains, [domain]);
                store
                    .finish_task(m.id, &attempt, "paused", &error.to_string(), &[])
                    .unwrap();
                store.decide_network(m.id, access.id, true).unwrap();
                store.resume_network(&access).unwrap().unwrap();
                attempt = store
                    .task_input(
                        m.id,
                        *worker,
                        m.planning.as_ref().unwrap().plan.as_ref().unwrap(),
                    )
                    .unwrap()
                    .unwrap()
                    .source;
            }
            let mut session =
                crate::sandbox::session::prepare_task(&vm, &context, &attempt, &flag)?;
            let summary = session.preflight(Some(&recipe), &flag, |_| {}, Timing::quiet())?;
            assert!(summary.contains("Baseline 0/1 passed"));
            assert_eq!(
                load(&root, ids[index])?.unwrap().baseline[0].output,
                "baseline-ran"
            );
            assert_eq!(workspace::source_state(&root.join("work"))?, source);
        }
        Ok(())
    }));
    crate::sandbox::delete(&root).unwrap();
    outcome.unwrap().unwrap();
}
