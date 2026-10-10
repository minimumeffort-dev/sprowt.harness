use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    execution::{Check, CheckResult, Report},
    sandbox::Sandbox,
    store::Store,
    task_worktree, workspace,
};

pub const TOOL: &str = "run_task_checks";

// Reuse only within this call. Each task keeps its own cwd, HOME and permissions.
pub fn unique_results(
    checks: &[Check],
    cancelled: &AtomicBool,
    mut run: impl FnMut(&Check) -> io::Result<CheckResult>,
) -> io::Result<Vec<CheckResult>> {
    let mut results: Vec<CheckResult> = Vec::new();
    for check in checks {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let mut result = match results.iter().find(|r| {
            r.task == check.task && r.command == check.command && r.timeout() == check.timeout()
        }) {
            Some(result) => result.clone(),
            None => run(check)?,
        };
        result.check.clone_from(&check.check);
        let passed = result.exit_code == Some(0);
        results.push(result);
        if !passed {
            break;
        }
    }
    Ok(results)
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    pub name: String,
    pub content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub source: String,
    pub checks: Vec<Check>,
    pub scripts: Option<Vec<Script>>,
}

#[derive(Default, Deserialize, Serialize)]
struct Bundle {
    scripts: Vec<Script>,
    receipt: Option<Receipt>,
}

#[derive(Deserialize, Serialize)]
struct Receipt {
    source: String,
    fingerprint: String,
    scripts_fingerprint: String,
    passed: bool,
    results: Vec<CheckResult>,
}

pub fn folder(id: i64) -> String {
    format!("/opt/sprowt-checks/{id}")
}

fn path(root: &Path, id: i64) -> PathBuf {
    root.join("checks").join(format!("{id}.json"))
}

fn load(root: &Path, id: i64) -> io::Result<Bundle> {
    match fs::read(path(root, id)) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Bundle::default()),
        Err(error) => Err(error),
    }
}

fn save(root: &Path, id: i64, bundle: &Bundle) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let path = path(root, id);
    fs::create_dir_all(path.parent().unwrap())?;
    fs::set_permissions(path.parent().unwrap(), fs::Permissions::from_mode(0o700))?;
    let next = path.with_extension("next");
    fs::write(&next, serde_json::to_vec(bundle)?)?;
    fs::set_permissions(&next, fs::Permissions::from_mode(0o600))?;
    fs::rename(next, path)
}

fn validate_scripts(scripts: &[Script]) -> io::Result<()> {
    let mut names = BTreeSet::new();
    if scripts.len() > 16
        || scripts.iter().map(|s| s.content.len()).sum::<usize>() > 256 * 1024
        || scripts.iter().any(|script| {
            script.name.is_empty()
                || script.name.len() > 100
                || !script.name.as_bytes()[0].is_ascii_alphanumeric()
                || !script
                    .name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
                || !names.insert(&script.name)
        })
    {
        return Err(io::Error::other(
            "Save at most 16 scripts (256 KiB total) with unique plain filenames; paths are not accepted.",
        ));
    }
    Ok(())
}

impl Request {
    pub fn parse(arguments: Value) -> io::Result<Self> {
        let request: Self = serde_json::from_value(arguments)?;
        request.validate()?;
        Ok(request)
    }

    fn validate(&self) -> io::Result<()> {
        task_worktree::task_id(&self.source)?;
        if self.source.len() > 100
            || self.checks.is_empty()
            || self.checks.len() > 32
            || self.checks.iter().any(|c| {
                c.command.len() > 128
                    || c.command.iter().map(String::len).sum::<usize>() > 64 * 1024
            })
        {
            return Err(io::Error::other(
                "Run 1–32 declared check commands with bounded arguments.",
            ));
        }
        if let Some(scripts) = &self.scripts {
            validate_scripts(scripts)?;
        }
        Ok(())
    }
}

pub fn tool() -> Value {
    json!({"type":"function","name":TOOL,
        "description":"Run all declared task checks in the controller's actual verification environment before returning a completed report. Use your current task attempt source. Commands have absolute executables, run in your task folder with a fresh environment, and default to 30 seconds each. Use timeout_seconds null for the default or 1–180 seconds for longer measured checks. Allow startup, execution and cleanup headroom; align inner test and wrapper deadlines with this budget. Diagnose hangs and preserve assertions when repairing timeouts. Return the same budget in the final report. Optional scripts replace the saved bundle at /opt/sprowt-checks/<task-run-id>/<name>; null preserves it. Scripts are read-only, survive retries and restart, and must create temporary fixtures under /tmp or HOME, never /static. Do not run concurrent source edits. Returns observed results and the tested source fingerprint; later edits require another run. This does not finish or merge the task; return the same commands in your final report.",
        "inputSchema":{"type":"object","additionalProperties":false,
            "properties":{"source":{"type":"string"},
                "checks":{"type":"array","minItems":1,"maxItems":32,"items":crate::execution::check_schema()},
                "scripts":{"type":["array","null"],"maxItems":16,"items":{"type":"object","additionalProperties":false,"properties":{"name":{"type":"string"},"content":{"type":"string"}},"required":["name","content"]}}},
            "required":["source","checks","scripts"]}})
}

fn authorize(
    store: &Store,
    root: &Path,
    mod_id: i64,
    worker: i64,
    source: &str,
) -> io::Result<(i64, crate::plan::Task)> {
    let execution = store
        .execution(mod_id)
        .map_err(io::Error::other)?
        .ok_or_else(|| io::Error::other("Task checks require an active execution."))?;
    let run = execution.tasks.iter().find(|run| {
        run.source == source
            && run.worker == Some(worker)
            && matches!(run.status.as_str(), "sending" | "running")
    });
    if execution.backend != "apple-container"
        || execution.workspace.canonicalize()? != root.canonicalize()?
        || run.is_none()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Checks must belong to this worker's current task attempt and VM.",
        ));
    }
    let run = run.unwrap();
    let plan = store
        .planning(mod_id)
        .map_err(io::Error::other)?
        .and_then(|p| p.plan)
        .ok_or_else(|| io::Error::other("Task checks need a saved plan."))?;
    let task = plan
        .tasks
        .into_iter()
        .find(|task| task.id == run.task_id)
        .ok_or_else(|| io::Error::other("Task is no longer in the plan."))?;
    Ok((run.id, task))
}

pub fn run(
    store: &Store,
    vm: &mut Sandbox,
    mod_id: i64,
    worker: i64,
    request: Request,
    cancelled: &AtomicBool,
    progress: impl FnMut(&str),
) -> io::Result<String> {
    request.validate()?;
    let (id, task) = authorize(store, vm.root(), mod_id, worker, &request.source)?;
    if vm.tasks.active != Some(id) || vm.tasks.owners.get(&id) != Some(&worker) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Check runner is not assigned to this task.",
        ));
    }
    let checks = Report::parse(
        &json!({"status":"completed","summary":"Check run","checks":request.checks}).to_string(),
        &task,
    )
    .map_err(io::Error::other)?
    .checks;
    if cancelled.load(Ordering::Relaxed) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Task checks stopped.",
        ));
    }
    let mut bundle = load(vm.root(), id)?;
    if let Some(scripts) = request.scripts {
        bundle.scripts = scripts;
    }
    bundle.receipt = None;
    save(vm.root(), id, &bundle)?;
    let (before, results) = vm.verify_with_progress(&checks, cancelled, progress)?;
    let after = vm.snapshot(&task_worktree::folder(id), &AtomicBool::new(false))?;
    // A stop or replacement attempt cannot save successful evidence for the old run.
    authorize(store, vm.root(), mod_id, worker, &request.source)?;
    let passed = !cancelled.load(Ordering::Relaxed)
        && before == after
        && results.len() == checks.len()
        && results.iter().all(|r| r.exit_code == Some(0));
    let receipt = Receipt {
        source: request.source,
        fingerprint: workspace::fingerprint(&before)?,
        scripts_fingerprint: workspace::fingerprint(&script_snapshot(&bundle.scripts))?,
        passed,
        results,
    };
    let result = json!({"passed":passed,"source":receipt.source,"fingerprint":receipt.fingerprint,
        "scripts_fingerprint":receipt.scripts_fingerprint,"script_directory":folder(id),
        "source_changed":before != after,"cancelled":cancelled.load(Ordering::Relaxed),
        "results":receipt.results,"unrun":checks.len().saturating_sub(receipt.results.len())})
    .to_string();
    bundle.receipt = Some(receipt);
    save(vm.root(), id, &bundle)?;
    Ok(result)
}

fn script_snapshot(scripts: &[Script]) -> workspace::Snapshot {
    scripts
        .iter()
        .map(|s| (PathBuf::from(&s.name), s.content.as_bytes().to_vec(), 0o444))
        .collect()
}

impl Sandbox {
    pub(crate) fn restore_check_scripts(&self, cancelled: &AtomicBool) -> io::Result<()> {
        let Some(id) = self.tasks.active else {
            return Ok(());
        };
        if !path(self.root(), id).exists() {
            return Ok(());
        }
        let bundle = load(self.root(), id)?;
        validate_scripts(&bundle.scripts)?;
        let archive = path(self.root(), id).with_extension("tar");
        let mut tar = tar::Builder::new(Vec::new());
        for (path, bytes, mode) in script_snapshot(&bundle.scripts) {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(mode);
            header.set_cksum();
            tar.append_data(&mut header, path, bytes.as_slice())?;
        }
        fs::write(&archive, tar.into_inner()?)?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&archive, fs::Permissions::from_mode(0o600))?;
        let result = (|| {
            self.copy_in(&archive, "/opt/sprowt-transfer/checks.tar", cancelled)?;
            // This protected directory is read-only to workers; the archive contains only validated regular files.
            self.guest(&["/bin/rm", "-rf", &folder(id)], cancelled)?;
            self.guest(&["/bin/mkdir", "-p", &folder(id)], cancelled)?;
            self.guest(
                &[
                    "/bin/tar",
                    "--no-same-owner",
                    "--same-permissions",
                    "-xf",
                    "/opt/sprowt-transfer/checks.tar",
                    "-C",
                    &folder(id),
                ],
                cancelled,
            )
        })();
        let _ = fs::remove_file(archive);
        let _ = self.guest(
            &["/bin/rm", "-f", "/opt/sprowt-transfer/checks.tar"],
            &AtomicBool::new(false),
        );
        result
    }

    pub(crate) fn remove_check_scripts(&self, id: i64, cancelled: &AtomicBool) -> io::Result<()> {
        self.guest(&["/bin/rm", "-rf", &folder(id)], cancelled)?;
        let path = path(self.root(), id);
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        plan::{Plan, Role},
        store::{CodeMod, test_support::TestData},
        tools::{Context, Dispatcher},
    };

    fn fixture() -> (TestData, Store, CodeMod, i64, String, PathBuf) {
        let data = TestData::new();
        let mut store = data.store();
        let project = data.0.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("source.txt"), "original").unwrap();
        let project_id = store.load_project(&project).unwrap().id;
        let mut code_mod = store.create_mod(project_id, "Check source").unwrap();
        let plan = Plan::parse(r#"{"summary":"Verify source","tasks":[{"id":"verify","title":"Check source","outcome":"A repeatable check","files":["source.txt"],"depends_on":[],"worker":"codex","checks":["source is correct"]}]}"#).unwrap();
        store
            .save_plan(
                code_mod.id,
                &code_mod.planning.as_ref().unwrap().source,
                &plan,
            )
            .unwrap();
        let root = data.0.join(format!("checks-{}", std::process::id()));
        workspace::create(&project, &root).unwrap();
        store.create_execution(code_mod.id, &root, &plan).unwrap();
        let worker = store.worker(code_mod.id).unwrap().id;
        let input = store
            .task_input(code_mod.id, worker, &plan)
            .unwrap()
            .unwrap();
        code_mod.execution = store.execution(code_mod.id).unwrap();
        (data, store, code_mod, worker, input.source, root)
    }

    fn arguments(source: &str, script: Option<&str>) -> Value {
        let id = task_worktree::task_id(source).unwrap();
        json!({"source":source,"checks":[{"check":"source is correct","command":["/usr/bin/node",format!("{}/check.mjs", folder(id))]}],
            "scripts":script.map(|content| vec![json!({"name":"check.mjs","content":content})])})
    }

    #[test]
    fn scripts_reject_paths_duplicates_and_unbounded_content() {
        let source = crate::store::task_source(1, 1);
        for name in [
            "",
            "..",
            "/static",
            "../check",
            "a/b",
            "a\\b",
            "-x",
            "bad\nname",
        ] {
            let mut args = arguments(&source, Some("test"));
            args["scripts"][0]["name"] = json!(name);
            assert!(Request::parse(args).is_err(), "{name}");
        }
        let mut args = arguments(&source, Some("test"));
        args["scripts"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"check.mjs","content":"duplicate"}));
        assert!(Request::parse(args).is_err());
        assert!(Request::parse(arguments(&source, Some(&"x".repeat(256 * 1024 + 1)))).is_err());
        assert!(Request::parse(arguments(&source, None)).is_ok());
    }

    #[test]
    fn check_authorization_is_bound_to_current_owner_attempt_and_workspace() {
        let (_data, store, code_mod, worker, source, root) = fixture();
        let id = task_worktree::task_id(&source).unwrap();
        assert!(authorize(&store, &root, code_mod.id, worker, &source).is_ok());
        assert!(authorize(&store, &root, code_mod.id, worker + 1, &source).is_err());
        assert!(authorize(&store, root.parent().unwrap(), code_mod.id, worker, &source).is_err());
        assert!(
            authorize(
                &store,
                &root,
                code_mod.id,
                worker,
                &crate::store::task_source(id, 2)
            )
            .is_err()
        );
        for role in [Role::Planner, Role::Executor, Role::Reviewer] {
            for muse in [false, true] {
                let mut context = Context::worker(&code_mod, worker, role)
                    .with_mailbox(Path::new(store.0.path().unwrap()));
                context.muse = muse;
                assert_eq!(
                    crate::tools::advertised(&context)
                        .iter()
                        .any(|t| t["name"] == TOOL),
                    role == Role::Executor
                );
            }
        }
        store
            .task_status(code_mod.id, &source, "checking", None)
            .unwrap();
        assert!(authorize(&store, &root, code_mod.id, worker, &source).is_err());
    }

    #[test]
    fn duplicate_commands_run_once_per_pass_without_losing_coverage() {
        let flag = AtomicBool::new(false);
        let check = |task, name: &str, command: &str| Check {
            timeout_seconds: None,
            task: Some(task),
            check: name.into(),
            command: vec![command.into()],
        };
        let checks = [
            check(1, "First", "/a"),
            check(1, "Other", "/b"),
            check(1, "Regression", "/a"),
            check(2, "Other environment", "/a"),
        ];
        let mut calls = Vec::new();
        for _ in 0..2 {
            let results = unique_results(&checks, &flag, |c| {
                calls.push((c.task, c.command.clone()));
                Ok(CheckResult {
                    timeout_seconds: None,
                    duration_ms: None,
                    task: c.task,
                    check: c.check.clone(),
                    command: c.command.clone(),
                    exit_code: Some(0),
                    output: "observed".into(),
                    missing_runtime: None,
                })
            })
            .unwrap();
            assert_eq!(results.len(), checks.len());
            assert_eq!(
                results.iter().map(|r| &r.check).collect::<Vec<_>>(),
                checks.iter().map(|c| &c.check).collect::<Vec<_>>()
            );
            assert_eq!(results[2].output, "observed");
        }
        assert_eq!(
            calls.len(),
            6,
            "No reuse across passes or task environments"
        );
        let mut calls = 0;
        let failed = unique_results(&checks, &flag, |c| {
            calls += 1;
            Ok(CheckResult {
                timeout_seconds: None,
                duration_ms: None,
                task: c.task,
                check: c.check.clone(),
                command: c.command.clone(),
                exit_code: Some(1),
                output: "regression".into(),
                missing_runtime: None,
            })
        })
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(failed.len(), 1);
        flag.store(true, Ordering::Relaxed);
        assert!(
            unique_results(&checks, &flag, |_| panic!("Stopped checks ran"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn check_reuse_requires_the_same_budget() {
        let checks: Vec<Check> = serde_json::from_value(json!([
            {"check":"Default", "command":["/bin/true"]},
            {"check":"Longer", "command":["/bin/true"], "timeout_seconds":120},
            {"check":"Explicit default", "command":["/bin/true"], "timeout_seconds":30}
        ]))
        .unwrap();
        let mut budgets = Vec::new();
        let results = unique_results(&checks, &AtomicBool::new(false), |check| {
            budgets.push(check.timeout());
            Ok(CheckResult {
                task: check.task,
                check: check.check.clone(),
                command: check.command.clone(),
                timeout_seconds: Some(check.timeout()),
                duration_ms: Some(25_000),
                exit_code: Some(0),
                output: String::new(),
                missing_runtime: None,
            })
        })
        .unwrap();
        assert_eq!(budgets, [30, 120]);
        assert_eq!(results.len(), 3);
        assert_eq!(results[1].timeout(), 120);
        assert_eq!(results[2].duration_ms, Some(25_000));
    }

    #[test]
    #[ignore = "repairs a faulty combined check with saved scripts and peer source in a disposable VM"]
    fn requested_check_repair_preserves_combined_source_and_saved_scripts() {
        let (_data, mut store, mut m, worker, source, root) = fixture();
        let id = task_worktree::task_id(&source).unwrap();
        let flag = AtomicBool::new(false);
        let bad = "from pathlib import Path\nfor p in Path('.').rglob('*'):\n    assert 'backup' not in p.name, str(p)\n";
        let good = "from pathlib import Path\ndef private(name):\n    return name.endswith(('.db', '.duckdb', '.wal', '-backup.json'))\nassert all(private(n) for n in ['private.db', 'private.duckdb', 'private.wal', 'private-backup.json'])\nassert not private('task-backup.mjs')\nfor p in Path('.').rglob('*'):\n    assert not private(p.name), str(p)\nassert Path('source.txt').read_text() == 'original'\nassert Path('static/task-backup.mjs').read_text() == 'export const backup = true;\\n'\n";
        let args = |source: &str, script: &str| {
            json!({
                "source":source,
                "checks":[{"check":"source is correct","command":["/usr/bin/python3",format!("{}/check.py", folder(id))]}],
                "scripts":[{"name":"check.py","content":script}],
            })
        };
        let call =
            |vm: &mut Sandbox, m: &CodeMod, store: &Store, args: Value| -> io::Result<Value> {
                let context = Context::worker(m, worker, Role::Executor)
                    .with_mailbox(Path::new(store.0.path().unwrap()));
                let output = Dispatcher::new(&context, Some(vm), &flag)
                    .worker_call(TOOL, args, |_| {})?
                    .message();
                Ok(serde_json::from_str(&output)?)
            };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> io::Result<()> {
                let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
                vm.prepare_tasks(&[id, id + 1], &flag)?;
                vm.assign_task(id, worker, &flag)?;
                vm.install_packages(
                    &crate::packages::Request {
                        packages: vec!["python3".into()],
                        reason: "Run the saved-check repair regression".into(),
                    },
                    &flag,
                )?;
                let first = call(&mut vm, &m, &store, args(&source, bad))?;
                assert_eq!(first["passed"], true, "{first}");
                let mut checks = Request::parse(args(&source, bad))?.checks;
                checks[0].task = Some(id);
                let (_, passed) = vm.verify_execution(&source, &checks, &flag)?;
                assert_eq!(passed[0].exit_code, Some(0));
                store
                    .finish_task(m.id, &source, "done", "Passed", &passed)
                    .unwrap();
                store.pending(worker, None).unwrap();

                // A peer adds a legitimate module after the owner's check passed.
                vm.assign_task(id + 1, worker + 1, &flag)?;
                vm.guest(&["/bin/sh", "-c", &format!("mkdir -p /tasks/{}/static; printf 'export const backup = true;\\n' > /tasks/{}/static/task-backup.mjs", id+1, id+1)], &flag)?;
                vm.integrate_task(id + 1, &flag)?;
                let (_, failed) = vm.verify_execution("final:test", &checks, &flag)?;
                assert_eq!(failed[0].exit_code, Some(1));
                assert!(failed[0].output.contains("static/task-backup.mjs"));
                store
                    .execution_checks(m.id, "blocked", &failed, None)
                    .unwrap();
                assert!(store.fix_failed_check(m.id, id).unwrap());
                let plan = store.planning(m.id).unwrap().unwrap().plan.unwrap();
                let input = store.task_input(m.id, worker, &plan).unwrap().unwrap();
                m.execution = store.execution(m.id).unwrap();
                let saved = fs::read(path(&root, id))?;
                vm.guest(&["/bin/rm", "-rf", &folder(id)], &flag)?;
                drop(vm);

                let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
                vm.assign_task(id, worker, &flag)?;
                vm.refresh_for_repair(id, &input.source, &flag)?;
                assert_eq!(fs::read(path(&root, id))?, saved);
                assert!(vm.guest_exists(&format!("{}/check.py", folder(id)))?);
                assert!(vm.guest_exists(&format!("/tasks/{id}/static/task-backup.mjs"))?);
                assert_eq!(
                    call(&mut vm, &m, &store, args(&input.source, good))?["passed"],
                    true
                );
                let (_, repaired) = vm.verify_execution(&input.source, &checks, &flag)?;
                assert_eq!(repaired[0].exit_code, Some(0));
                store
                    .finish_task(m.id, &input.source, "done", "Repaired", &repaired)
                    .unwrap();
                assert!(store.execution(m.id).unwrap().unwrap().needs_final_checks());
                let (_, final_checks) = vm.verify_execution("final:test", &checks, &flag)?;
                assert_eq!(final_checks[0].exit_code, Some(0));
                assert!(root.join("work/static/task-backup.mjs").exists());
                assert_eq!(
                    fs::read_to_string(root.join("work/source.txt"))?,
                    "original"
                );
                Ok(())
            },
        ));
        crate::sandbox::delete(&root).unwrap();
        result.unwrap().unwrap();
    }

    #[test]
    #[ignore = "reproduces /static failure and verifies saved scripts, restart, source binding and cleanup in a disposable VM"]
    fn shared_check_runner_recovers_and_survives_restart() {
        let (_data, store, code_mod, worker, source, root) = fixture();
        let id = task_worktree::task_id(&source).unwrap();
        let flag = AtomicBool::new(false);
        let bad = "import {mkdirSync} from 'node:fs'; mkdirSync('/static', {recursive:true});";
        let good = "import {mkdtempSync, readFileSync, rmSync, writeFileSync} from 'node:fs'; import {tmpdir} from 'node:os'; import {join} from 'node:path'; import assert from 'node:assert/strict'; const dir=mkdtempSync(join(tmpdir(),'sprowt-check-')); try { writeFileSync(join(dir,'fixture'), 'stub'); assert.equal(readFileSync('source.txt','utf8'), 'original'); console.log('source checked'); } finally { rmSync(dir, {recursive:true}); }";
        let mut context = Context::worker(&code_mod, worker, Role::Executor)
            .with_mailbox(Path::new(store.0.path().unwrap()));
        let call = |vm: &mut Sandbox, context: &Context, args: Value| -> io::Result<Value> {
            let output = Dispatcher::new(context, Some(vm), &flag)
                .worker_call(TOOL, args, |_| {})?
                .message();
            Ok(serde_json::from_str(&output)?)
        };
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> io::Result<()> {
                let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
                vm.prepare_tasks(&[id], &flag)?;
                vm.assign_task(id, worker, &flag)?;
                vm.install_packages(
                    &crate::packages::Request {
                        packages: vec!["nodejs".into()],
                        reason: "Verify the shared check runner".into(),
                    },
                    &flag,
                )?;
                let failed = call(&mut vm, &context, arguments(&source, Some(bad)))?;
                assert_eq!(failed["passed"], false);
                assert!(
                    failed["results"][0]["output"]
                        .as_str()
                        .unwrap()
                        .contains("/static")
                );
                assert!(!vm.guest_exists("/static")?);
                context.muse = true;
                let passed = call(&mut vm, &context, arguments(&source, Some(good)))?;
                assert_eq!(passed["passed"], true, "{passed}");
                assert!(vm.tasks.integrated.is_empty());
                assert_eq!(
                    load(&root, id)?.receipt.unwrap().fingerprint,
                    passed["fingerprint"]
                );
                // No unverified source changes may retain a passing receipt.
                let mutating =
                    "import {writeFileSync} from 'node:fs'; writeFileSync('source.txt','changed');";
                let changed = call(&mut vm, &context, arguments(&source, Some(mutating)))?;
                assert_eq!(changed["passed"], false);
                assert_eq!(changed["source_changed"], true);
                assert!(!load(&root, id)?.receipt.unwrap().passed);
                vm.guest(
                    &[
                        "/bin/sh",
                        "-c",
                        &format!("printf original > /tasks/{id}/source.txt"),
                    ],
                    &flag,
                )?;
                assert_eq!(
                    call(&mut vm, &context, arguments(&source, Some(good)))?["passed"],
                    true
                );
                let saved = fs::read(path(&root, id))?;
                let stopped = AtomicBool::new(true);
                assert!(
                    Dispatcher::new(&context, Some(&mut vm), &stopped)
                        .worker_call(TOOL, arguments(&source, Some(bad)), |_| {})
                        .is_err()
                );
                assert_eq!(fs::read(path(&root, id))?, saved);
                // Restart with the guest copy removed; the host bundle restores it for both tool and final checks.
                vm.guest(&["/bin/rm", "-rf", &folder(id)], &flag)?;
                drop(vm);
                let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
                vm.assign_task(id, worker, &flag)?;
                assert_eq!(
                    call(&mut vm, &context, arguments(&source, None))?["passed"],
                    true
                );
                let mut checks = Request::parse(arguments(&source, None))?.checks;
                checks[0].task = Some(id);
                assert_eq!(
                    vm.verify_execution(&source, &checks, &flag)?.1[0].exit_code,
                    Some(0)
                );
                assert_eq!(
                    vm.verify_execution("final:test", &checks, &flag)?.1[0].exit_code,
                    Some(0)
                );
                vm.assign_task(id, worker, &flag)?;
                vm.guest(
                    &[
                        "/bin/sh",
                        "-c",
                        &format!("printf changed > /tasks/{id}/source.txt"),
                    ],
                    &flag,
                )?;
                let receipt = load(&root, id)?.receipt.unwrap();
                assert_ne!(
                    receipt.fingerprint,
                    workspace::fingerprint(&vm.snapshot(&task_worktree::folder(id), &flag)?)?
                );
                assert_ne!(vm.verify(&checks, &flag)?.1[0].exit_code, Some(0));
                vm.prepare_tasks(&[id + 1], &flag)?;
                assert!(!path(&root, id).exists());
                assert!(!vm.guest_exists(&folder(id))?);
                Ok(())
            }));
        crate::sandbox::delete(&root).unwrap();
        result.unwrap().unwrap();
    }
}
