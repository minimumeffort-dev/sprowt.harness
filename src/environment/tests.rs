use super::*;

#[test]
fn memory_requests_use_the_accepted_recipe_or_source_file() {
    let mut explicit = recipe();
    explicit.resources = Some(Resources { memory_gib: 8 });
    let mut file = recipe();
    file.resources = Some(Resources { memory_gib: 4 });
    let snapshot = vec![(
        PathBuf::from(".sprowt/environment.json"),
        serde_json::to_vec(&file).unwrap(),
        0o644,
    )];
    assert_eq!(
        requested_memory(Some(&explicit), &snapshot).unwrap(),
        Some(8)
    );
    assert!(
        requested_memory(None, &snapshot)
            .unwrap_err()
            .to_string()
            .contains("memory_gib: 8")
    );
    assert_eq!(requested_memory(None, &[].to_vec()).unwrap(), None);
    file.resources = Some(Resources { memory_gib: 8 });
    let snapshot = vec![(
        PathBuf::from(".sprowt/environment.json"),
        serde_json::to_vec(&file).unwrap(),
        0o644,
    )];
    assert_eq!(requested_memory(None, &snapshot).unwrap(), Some(8));
}
fn recipe() -> Recipe {
    Recipe {
        resources: None,
        inputs: vec!["config.txt".into()],
        system_packages: vec![],
        reusable_paths: vec![],
        setup: vec![],
        capabilities: vec![Command {
            name: "Node".into(),
            argv: vec![
                "/usr/local/bin/mise".into(),
                "exec".into(),
                "node@22.16.0".into(),
                "--".into(),
                "node".into(),
                "--version".into(),
            ],
            timeout_seconds: 30,
        }],
        baseline: vec![],
    }
}
#[test]
fn recipes_reject_ephemeral_paths_and_invalidate_on_dependency_changes() {
    let mut r = recipe();
    r.validate().unwrap();
    let mut snapshot = vec![
        (PathBuf::from("package-lock.json"), b"one".to_vec(), 0),
        (PathBuf::from("app.js"), b"a".to_vec(), 0),
    ];
    let platform = crate::sandbox::Identity {
        digest: "sha256:test-image".into(),
        architecture: "arm64".into(),
    };
    let key = r.key(&snapshot, &platform).unwrap();
    snapshot[1].1 = b"b".to_vec();
    assert_eq!(key, r.key(&snapshot, &platform).unwrap());
    snapshot[0].1 = b"two".to_vec();
    assert_ne!(key, r.key(&snapshot, &platform).unwrap());
    r.capabilities[0].argv[0] = "/tasks/65/.venv/bin/python".into();
    assert!(r.validate().is_err());
    r = recipe();
    r.inputs = vec!["../outside".into()];
    assert!(r.validate().is_err());
    r = recipe();
    r.capabilities.clear();
    assert!(r.validate().is_err());
}
#[test]
fn prepared_environment_identity_includes_actual_platform_recipe_and_declared_inputs() {
    let mut r = recipe();
    let source = vec![(PathBuf::from("config.txt"), b"one".to_vec(), 0o644)];
    let mut platform = crate::sandbox::Identity {
        digest: "sha256:one".into(),
        architecture: "arm64".into(),
    };
    let key = r.key(&source, &platform).unwrap();
    platform.digest = "sha256:two".into();
    assert_ne!(key, r.key(&source, &platform).unwrap());
    platform.digest = "sha256:one".into();
    platform.architecture = "amd64".into();
    assert_ne!(key, r.key(&source, &platform).unwrap());
    platform.architecture = "arm64".into();
    r.capabilities[0].timeout_seconds += 1;
    assert_ne!(key, r.key(&source, &platform).unwrap());
    assert_ne!(
        key,
        recipe()
            .key(
                &vec![(PathBuf::from("config.txt"), b"two".to_vec(), 0o644)],
                &platform
            )
            .unwrap()
    );
}
#[test]
fn dependency_snapshots_are_opt_in_and_need_locked_current_inputs() {
    let mut r = recipe();
    let old = serde_json::to_value(&r).unwrap();
    let mut old = old.as_object().unwrap().clone();
    old.remove("reusable_paths");
    assert!(
        serde_json::from_value::<Recipe>(Value::Object(old))
            .unwrap()
            .reusable_paths
            .is_empty()
    );
    r.reusable_paths = vec![".venv".into()];
    assert!(r.validate().is_err());
    r.reusable_paths = vec!["node_modules".into()];
    r.inputs = vec!["package.json".into(), "package-lock.json".into()];
    assert!(r.validate().is_err());
    r.setup = vec![Command {
        name: "Install dependencies".into(),
        argv: vec!["/bin/true".into()],
        timeout_seconds: 30,
    }];
    r.validate().unwrap();
    let platform = crate::sandbox::Identity {
        digest: "sha256:test".into(),
        architecture: "arm64".into(),
    };
    assert!(r.key(&vec![], &platform).is_err());
    let source = vec![
        (PathBuf::from("package.json"), b"{}".to_vec(), 0o644),
        (PathBuf::from("package-lock.json"), b"{}".to_vec(), 0o644),
    ];
    r.key(&source, &platform).unwrap();
}
#[test]
#[ignore = "exercises private runtime/download copies, locked dependency snapshots, corruption and current baselines in a disposable VM; no inference or downloads"]
fn live_prepared_assets_are_private_and_locked_dependencies_reuse_only_exact_inputs() {
    let data = crate::store::test_support::TestData::new();
    fs::create_dir_all(&data.0).unwrap();
    let root = data.0.join("prepared-assets");
    workspace::create_snapshot(
        &vec![
            (PathBuf::from("source.txt"), b"original".to_vec(), 0o644),
            (PathBuf::from("package.json"), b"{}".to_vec(), 0o644),
            (
                PathBuf::from("package-lock.json"),
                b"locked-one".to_vec(),
                0o644,
            ),
        ],
        &root,
    )
    .unwrap();
    let flag = AtomicBool::new(false);
    let cmd = |name: &str, shell: &str| Command {
        name: name.into(),
        argv: vec!["/bin/sh".into(), "-c".into(), shell.into()],
        timeout_seconds: 30,
    };
    let r = Recipe {
        resources: None,
        inputs: vec!["package.json".into(), "package-lock.json".into()],
        system_packages: vec![],
        reusable_paths: vec!["node_modules".into()],
        setup: vec![cmd(
            "prepare fixture assets",
            "mkdir -p \"$HOME/.local/share/mise/installs/fixture/1/bin\" \"$HOME/.npm/_cacache/content-v2/sha512/00\" node_modules/pkg; printf runtime > \"$HOME/.local/share/mise/installs/fixture/1/bin/runtime\"; printf download > \"$HOME/.npm/_cacache/content-v2/sha512/00/blob\"; cp package-lock.json node_modules/pkg/lock; printf x >> \"$HOME/setup-count\"; printf secret > \"$HOME/auth.json\"; printf secret > \"$HOME/.npmrc\"",
        )],
        capabilities: vec![cmd(
            "current private assets",
            "test \"$(cat \"$HOME/.local/share/mise/installs/fixture/1/bin/runtime\")\" = runtime && test \"$(cat \"$HOME/.npm/_cacache/content-v2/sha512/00/blob\")\" = download && cmp package-lock.json node_modules/pkg/lock && printf x >> \"$HOME/probe-count\"",
        )],
        baseline: vec![cmd(
            "current source",
            "test \"$(cat source.txt)\" = original && cmp package-lock.json node_modules/pkg/lock && printf x >> \"$HOME/baseline-count\"",
        )],
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> io::Result<()> {
        let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
        vm.prepare_tasks(&[1, 2, 3, 4, 5, 6], &flag)?;
        vm.assign_task(1, 101, &flag)?;
        vm.preflight(Some(&r), 1, "first", &flag, |_| {})?;
        let first = load(&root, 1)?.unwrap();
        assert_eq!(first.cache_saved, Some(cache::Use::Published));
        assert_eq!(first.setup.len(), 1);
        vm.assign_task(2, 102, &flag)?;
        vm.preflight(Some(&r), 2, "second", &flag, |_| {})?;
        let second = load(&root, 2)?.unwrap();
        assert!(second.cache.dependencies());
        assert!(second.setup.is_empty());
        assert_ne!(first.runtime_home, second.runtime_home);
        assert_eq!(second.capabilities[0].exit_code, Some(0));
        assert_eq!(second.baseline[0].exit_code, Some(0));
        assert_eq!(vm.run_check_command(&Check { task: None, check: "no shared secrets or setup count".into(),
            command: cmd("private", "test ! -e \"$HOME/auth.json\" && test ! -e \"$HOME/.npmrc\" && test ! -e \"$HOME/setup-count\"").argv,
            timeout_seconds: Some(30) }, &flag).exit_code, Some(0));
        vm.preflight(Some(&r), 2, "same-home", &flag, |_| {})?;
        assert_eq!(load(&root, 2)?.unwrap().cache, cache::Use::Retained);
        assert_eq!(vm.run_check_command(&Check { task: None, check: "probes and baselines rerun".into(),
            command: cmd("counts", "test \"$(cat \"$HOME/probe-count\")\" = xx && test \"$(cat \"$HOME/baseline-count\")\" = xx").argv,
            timeout_seconds: Some(30) }, &flag).exit_code, Some(0));
        let private_change = cmd(
            "change private copies",
            "printf changed > \"$HOME/.local/share/mise/installs/fixture/1/bin/runtime\"; printf changed > node_modules/pkg/lock",
        );
        assert_eq!(
            vm.run_check_command(
                &Check {
                    task: None,
                    check: private_change.name,
                    command: private_change.argv,
                    timeout_seconds: Some(30)
                },
                &flag
            )
            .exit_code,
            Some(0)
        );
        vm.assign_task(3, 103, &flag)?;
        vm.preflight(Some(&r), 3, "third", &flag, |_| {})?;
        assert!(load(&root, 3)?.unwrap().setup.is_empty());
        assert_eq!(load(&root, 3)?.unwrap().capabilities[0].exit_code, Some(0));
        vm.assign_task(4, 104, &flag)?;
        vm.guest(
            &[
                "/bin/sh",
                "-c",
                "printf locked-two > /tasks/4/package-lock.json",
            ],
            &flag,
        )?;
        vm.preflight(Some(&r), 4, "changed-lock", &flag, |_| {})?;
        let changed = load(&root, 4)?.unwrap();
        assert_ne!(changed.environment, first.environment);
        assert_eq!(changed.cache, cache::Use::Miss);
        assert_eq!(changed.setup.len(), 1);
        let corrupt = format!(
            "/opt/sprowt-environments/{}/0/fixture/1/bin/runtime",
            first.environment
        );
        vm.guest(
            &[
                "/bin/sh",
                "-c",
                &format!(
                    "chmod u+w '{corrupt}'; printf corrupt > '{corrupt}'; chmod a-w '{corrupt}'"
                ),
            ],
            &flag,
        )?;
        vm.assign_task(5, 105, &flag)?;
        vm.preflight(Some(&r), 5, "corrupt-entry", &flag, |_| {})?;
        let recovered = load(&root, 5)?.unwrap();
        assert!(matches!(recovered.cache, cache::Use::Rejected { .. }));
        assert_eq!(recovered.setup.len(), 1);
        assert_eq!(recovered.cache_saved, Some(cache::Use::Published));
        vm.assign_task(3, 106, &flag)?;
        vm.preflight(Some(&r), 3, "new-home", &flag, |_| {})?;
        let reassigned = load(&root, 3)?.unwrap();
        assert_ne!(reassigned.cache, cache::Use::Retained);
        assert_eq!(reassigned.setup.len(), 1); // Existing task dependencies never prove a new HOME's setup.
        vm.assign_task(6, 107, &flag)?;
        let protected = cmd(
            "cache is controller-owned",
            &format!(
                "test ! -w /opt/sprowt-environments/{} && test ! -w '{corrupt}'",
                first.environment
            ),
        );
        assert_eq!(
            vm.run_check_command(
                &Check {
                    task: None,
                    check: protected.name,
                    command: protected.argv,
                    timeout_seconds: Some(30)
                },
                &flag
            )
            .exit_code,
            Some(0)
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(root.join("environments/1.json"))?
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        Ok(())
    }));
    crate::sandbox::delete(&root).unwrap();
    outcome.unwrap().unwrap();
}
#[test]
#[ignore = "runs setup, reuse, failed baselines, capability gating and source preservation in a disposable Linux VM"]
fn live_preflight_retains_evidence_and_reuses_only_proven_setup() {
    let data = crate::store::test_support::TestData::new();
    fs::create_dir_all(&data.0).unwrap();
    let root = data.0.join("preflight");
    workspace::create_snapshot(
        &vec![(PathBuf::from("source.txt"), b"original".to_vec(), 0o644)],
        &root,
    )
    .unwrap();
    let flag = AtomicBool::new(false);
    let cmd = |name: &str, shell: &str| Command {
        name: name.into(),
        argv: vec!["/bin/sh".into(), "-c".into(), shell.into()],
        timeout_seconds: 30,
    };
    let mut r = Recipe {
        resources: None,
        inputs: vec![],
        system_packages: vec![],
        reusable_paths: vec![],
        setup: vec![cmd(
            "prepare",
            "echo prepared >> \"$HOME/preflight-count\"; touch \"$HOME/preflight-ready\"",
        )],
        capabilities: vec![cmd("available", "test -f \"$HOME/preflight-ready\"")],
        baseline: vec![cmd("known failure", "test original = broken")],
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> io::Result<()> {
        let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
        vm.prepare_tasks(&[1], &flag)?;
        vm.assign_task(1, 1, &flag)?;
        let first = vm.preflight(Some(&r), 1, "attempt-1", &flag, |_| {})?;
        assert!(first.contains("Baseline 0/1 passed"));
        assert!(load(&root, 1)?.unwrap().baseline[0].failed());
        vm.preflight(Some(&r), 1, "attempt-2", &flag, |_| {})?;
        assert!(load(&root, 1)?.unwrap().setup.is_empty());
        assert_eq!(
            vm.run_check_command(
                &Check {
                    task: None,
                    check: "setup ran once".into(),
                    command: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        "test \"$(wc -l < \"$HOME/preflight-count\")\" -eq 1".into()
                    ],
                    timeout_seconds: Some(30)
                },
                &flag
            )
            .exit_code,
            Some(0)
        );
        drop(vm);
        let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
        vm.assign_task(1, 1, &flag)?;
        vm.preflight(Some(&r), 1, "attempt-3", &flag, |_| {})?;
        assert!(load(&root, 1)?.unwrap().setup.is_empty());
        r.capabilities = vec![cmd("missing capability", "exit 1")];
        assert!(
            vm.preflight(Some(&r), 1, "attempt-4", &flag, |_| {})
                .unwrap_err()
                .to_string()
                .contains("missing capability")
        );
        let failed = load(&root, 1)?.unwrap();
        assert!(failed.baseline.is_empty());
        assert!(failed.error.is_some());
        r.setup = vec![cmd("bad setup", "echo changed > source.txt; exit 1")];
        assert!(
            vm.preflight(Some(&r), 1, "attempt-5", &flag, |_| {})
                .unwrap_err()
                .to_string()
                .contains("changed source")
        );
        assert_eq!(
            fs::read_to_string(root.join("work/source.txt"))?,
            "changed\n"
        );
        assert!(root.join("environments/1-attempt-1.json").exists());
        Ok(())
    }));
    crate::sandbox::delete(&root).unwrap();
    outcome.unwrap().unwrap();
}
