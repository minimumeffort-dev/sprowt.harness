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
fn setup_budgets_allow_large_downloads_without_extending_probes_or_baselines() {
    let mut r = recipe();
    let command = r.capabilities[0].clone();
    r.setup = vec![command.clone()];
    for budget in [1, 180, 181, 600] {
        r.setup[0].timeout_seconds = budget;
        let parsed: Recipe = serde_json::from_slice(&serde_json::to_vec(&r).unwrap()).unwrap();
        parsed.validate().unwrap();
    }
    for budget in [0, 601] {
        r.setup[0].timeout_seconds = budget;
        assert!(r.validate().unwrap_err().contains("1–600"));
    }
    r.setup[0].timeout_seconds = 600;
    r.baseline = vec![command];
    for phase in ["capabilities", "baseline"] {
        for budget in [1, 180] {
            let commands = if phase == "capabilities" {
                &mut r.capabilities
            } else {
                &mut r.baseline
            };
            commands[0].timeout_seconds = budget;
            r.validate().unwrap();
        }
        for budget in [0, 181, 600] {
            let commands = if phase == "capabilities" {
                &mut r.capabilities
            } else {
                &mut r.baseline
            };
            commands[0].timeout_seconds = budget;
            assert!(r.validate().unwrap_err().contains("1–180"));
        }
        r.capabilities[0].timeout_seconds = 30;
        r.baseline[0].timeout_seconds = 30;
    }
    r.setup[0].argv[0] = "/tasks/65/.venv/bin/python".into();
    assert!(r.validate().unwrap_err().contains("temporary worker paths"));
}

#[test]
fn validated_setup_budget_reaches_the_command_and_saved_result() {
    let mut r = recipe();
    r.setup = vec![Command {
        name: "Download browser".into(),
        argv: vec!["/bin/sh".into(), "-c".into(), "printf ready".into()],
        timeout_seconds: 600,
    }];
    r.validate().unwrap();
    let results = command_batch(
        &r.setup,
        &AtomicBool::new(false),
        "setup",
        &mut |_| {},
        |check| {
            assert_eq!(check.command, ["/bin/sh", "-c", "printf ready"]);
            assert_eq!(check.timeout(), 600);
            CheckResult {
                timeout_seconds: Some(check.timeout()),
                duration_ms: Some(1),
                task: check.task,
                check: check.check.clone(),
                command: check.command.clone(),
                exit_code: Some(0),
                output: "ready".into(),
                missing_runtime: None,
            }
        },
    )
    .unwrap();
    assert_eq!(results[0].timeout_seconds, Some(600));
    assert_eq!(results[0].exit_code, Some(0));
    assert_eq!(results[0].output, "ready");
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
        &vec![
            (PathBuf::from("source.txt"), b"original".to_vec(), 0o644),
            (PathBuf::from("package-lock.json"), b"locked".to_vec(), 0o644),
            (PathBuf::from(".pytest_cache/v/cache/nodeids"), b"tracked baseline".to_vec(), 0o644),
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
    let mut r = Recipe {
        resources: None,
        inputs: vec![],
        system_packages: vec![],
        reusable_paths: vec![],
        setup: vec![cmd(
            "prepare",
            "echo prepared >> \"$HOME/preflight-count\"; touch \"$HOME/preflight-ready\"; printf refreshed > .pytest_cache/v/cache/nodeids",
        )],
        capabilities: vec![cmd("available", "test -f \"$HOME/preflight-ready\"")],
        baseline: vec![cmd("known failure", "printf tested > .pytest_cache/v/cache/nodeids; test original = broken")],
    };
    r.setup[0].timeout_seconds = 600;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> io::Result<()> {
        let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
        vm.prepare_tasks(&[1], &flag)?;
        vm.assign_task(1, 1, &flag)?;
        let first = vm.preflight(Some(&r), 1, "attempt-1", &flag, |_| {})?;
        assert!(first.contains("Baseline 0/1 passed"));
        let receipt = load(&root, 1)?.unwrap();
        assert_eq!(receipt.setup[0].timeout_seconds, Some(600));
        assert_eq!(receipt.setup[0].exit_code, Some(0));
        assert!(receipt.setup[0].duration_ms.unwrap() < 10_000);
        assert!(receipt.baseline[0].failed());
        assert_eq!(workspace::source_state(&root.join("work"))?.iter().find(|f| f.0 == Path::new(".pytest_cache/v/cache/nodeids")).unwrap().1, b"tracked baseline");
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
        r.setup = vec![cmd("bad setup", "echo changed > source.txt; echo changed > package-lock.json; exit 1")];
        let error = vm.preflight(Some(&r), 1, "attempt-5", &flag, |_| {}).unwrap_err().to_string();
        assert!(error.contains("changed source") && error.contains("source.txt") && error.contains("package-lock.json") && error.contains("Setup failed: bad setup"), "{error}");
        assert_eq!(
            fs::read_to_string(root.join("work/source.txt"))?,
            "changed\n"
        );
        assert!(root.join("environments/1-attempt-1.json").exists());
        let cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let stop = cancelled.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(2));
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let result = vm.run_check_command(
            &Check {
                task: None,
                check: "Long setup is cancellable".into(),
                command: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf cancellation-started; sleep 15; printf should-not-finish".into(),
                ],
                timeout_seconds: Some(600),
            },
            &cancelled,
        );
        thread.join().unwrap();
        assert_eq!(result.timeout_seconds, Some(600));
        assert!(result.failed());
        assert!(result.output.contains("Guest command stopped"));
        assert!(result.output.contains("cancellation-started"));
        assert!(!result.output.contains("should-not-finish"));
        assert!(result.duration_ms.unwrap() < 10_000);
        Ok(())
    }));
    crate::sandbox::delete(&root).unwrap();
    outcome.unwrap().unwrap();
}
