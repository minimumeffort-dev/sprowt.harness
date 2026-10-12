use super::*;

fn shell(script: &str) -> crate::execution::Check {
    crate::execution::Check {
        task: None,
        timeout_seconds: None,
        check: "session is isolated".into(),
        command: vec!["/bin/sh".into(), "-c".into(), script.into()],
    }
}

fn contents(snapshot: &Snapshot, path: &str) -> Vec<u8> {
    snapshot
        .iter()
        .find(|(name, _, _)| name == Path::new(path))
        .map_or_else(Vec::new, |(_, bytes, _)| bytes.clone())
}

#[test]
#[ignore = "uses a cached image and disposable VM; no provider inference or dependency downloads"]
fn dependent_delivery_refreshes_upfront_branch_and_preserves_draft_across_retry_and_restart() {
    let (_data, mut store, root, contexts, sources, workers) = fixture();
    let (_, mod_id) = contexts[0].execution_identity().unwrap();
    let planning = store.planning(mod_id).unwrap().unwrap();
    let mut plan = planning.plan.unwrap();
    plan.tasks[1].depends_on = vec!["a".into()];
    store.save_plan(mod_id, &planning.source, &plan).unwrap();
    store
        .task_status(mod_id, &sources[1], "pending", None)
        .unwrap();
    store.pending(workers[1], None).unwrap();
    assert!(
        store
            .task_input(mod_id, workers[1], &plan)
            .unwrap()
            .is_none()
    );
    let ids = sources
        .iter()
        .map(|s| crate::task_worktree::task_id(s).unwrap())
        .collect::<Vec<_>>();
    let flag = AtomicBool::new(false);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> io::Result<()> {
        let mut controller = Sandbox::prepare(&root, &flag, |_| {})?;
        controller.prepare_tasks(&ids, &flag)?;
        controller.guest(&["/bin/sh", "-c", &format!("printf retained > /tasks/{}/b.txt; mkdir -p /tasks/{}/.venv; printf runtime > /tasks/{}/.venv/proof", ids[1], ids[1], ids[1])], &flag)?;
        let vm = Arc::new(Mutex::new(controller));
        let mut owner = prepare_task(&vm, &contexts[0], &sources[0], false, &flag)?;
        assert_eq!(
            owner
                .command(&shell("printf upstream > a.txt"), &flag)
                .exit_code,
            Some(0)
        );
        drop(owner);
        let check = shell("test \"$(cat a.txt)\" = upstream");
        let (_, checks) =
            vm.lock()
                .unwrap()
                .verify_execution(&sources[0], &[check.clone()], &flag)?;
        assert_eq!(checks[0].exit_code, Some(0));
        store
            .finish_task(mod_id, &sources[0], "done", "Owner passed", &checks)
            .unwrap();
        let dependent = store
            .task_input(mod_id, workers[1], &plan)
            .unwrap()
            .unwrap();
        let mut session = prepare_task(&vm, &contexts[1], &dependent.source, false, &flag)?;
        let snapshot = session.snapshot(&flag)?;
        assert_eq!(
            contents(&snapshot, "a.txt"),
            b"upstream",
            "first dependent delivery must receive completed source"
        );
        assert_eq!(contents(&snapshot, "b.txt"), b"retained");
        assert_eq!(
            session
                .command(
                    &shell("test \"$(cat .venv/proof)\" = runtime && printf updated > b.txt"),
                    &flag
                )
                .exit_code,
            Some(0)
        );
        drop(session);
        let mut duplicate = prepare_task(&vm, &contexts[1], &dependent.source, false, &flag)?;
        assert_eq!(contents(&duplicate.snapshot(&flag)?, "b.txt"), b"updated");
        drop(duplicate);
        {
            let mut controller = vm.lock().unwrap();
            controller.export(&flag)?;
            let base: Snapshot = serde_json::from_slice(&fs::read(
                root.join(format!("task-drafts/{}-base.json", ids[1])),
            )?)?;
            let draft: Snapshot = serde_json::from_slice(&fs::read(
                root.join(format!("task-drafts/{}.json", ids[1])),
            )?)?;
            assert_eq!(contents(&base, "a.txt"), b"upstream");
            assert_eq!(contents(&draft, "a.txt"), b"upstream");
            assert_eq!(
                contents(&controller.review_source(&flag)?, "a.txt"),
                b"upstream",
                "export must not undo an integrated prerequisite"
            );
            controller.checkpoint_tasks(&flag)?;
        }
        store
            .finish_task(mod_id, &dependent.source, "paused", "Controlled pause", &[])
            .unwrap();
        assert!(store.retry_task(mod_id, ids[1]).unwrap());
        let retry = store
            .task_input(mod_id, workers[1], &plan)
            .unwrap()
            .unwrap();
        drop(vm);
        let controller = Sandbox::prepare(&root, &flag, |_| {})?;
        let vm = Arc::new(Mutex::new(controller));
        let mut session = prepare_task(&vm, &contexts[1], &retry.source, false, &flag)?;
        assert_eq!(contents(&session.snapshot(&flag)?, "a.txt"), b"upstream");
        assert_eq!(contents(&session.snapshot(&flag)?, "b.txt"), b"updated");
        assert_eq!(
            session
                .command(&shell("test \"$(cat .venv/proof)\" = runtime"), &flag)
                .exit_code,
            Some(0)
        );
        drop(session);
        let check = shell("test \"$(cat a.txt)\" = upstream && test \"$(cat b.txt)\" = updated");
        let (_, checks) =
            vm.lock()
                .unwrap()
                .verify_execution(&retry.source, &[check.clone()], &flag)?;
        assert_eq!(checks[0].exit_code, Some(0));
        store
            .finish_task(mod_id, &retry.source, "done", "Dependent passed", &checks)
            .unwrap();
        let mut controller = vm.lock().unwrap();
        let combined = controller.snapshot("/workspace", &flag)?;
        let mut check = check;
        check.task = Some(ids[1]);
        let (verified, checks) = controller.verify_execution("final:handoff", &[check], &flag)?;
        assert_eq!(checks[0].exit_code, Some(0));
        assert_eq!(verified, combined);
        assert_eq!(controller.snapshot("/workspace", &flag)?, combined);
        Ok(())
    }));
    delete(&root).unwrap();
    result.unwrap().unwrap();
}
