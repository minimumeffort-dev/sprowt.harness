use super::*;
use crate::{
    checks,
    plan::{Plan, Role},
    store::{Store, test_support::TestData},
    tools::{Context, Dispatcher},
};

#[path = "session_handoff_tests.rs"]
mod handoff;

fn fixture() -> (
    TestData,
    Store,
    PathBuf,
    Vec<Context>,
    Vec<String>,
    Vec<i64>,
) {
    let data = TestData::new();
    let mut store = data.store();
    let project = data.0.join("project");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("source.txt"), "original").unwrap();
    let project_id = store.load_project(&project).unwrap().id;
    let mut m = store
        .create_mod(project_id, "Verify independent sessions")
        .unwrap();
    let plan = Plan::parse(&json!({"summary":"Independent checks","tasks":[
        {"id":"a","title":"A","outcome":"A works","files":["a.txt"],"depends_on":[],"worker":"codex","checks":["session is isolated"]},
        {"id":"b","title":"B","outcome":"B works","files":["b.txt"],"depends_on":[],"worker":"muse","checks":["session is isolated"]}
    ]}).to_string()).unwrap();
    store
        .save_plan(m.id, &m.planning.as_ref().unwrap().source, &plan)
        .unwrap();
    let root = data.0.join("vm");
    workspace::create(&project, &root).unwrap();
    store.create_execution(m.id, &root, &plan).unwrap();
    let workers = vec![
        store
            .worker_provider(m.id, Role::Executor, 0, "codex")
            .unwrap()
            .id,
        store
            .worker_provider(m.id, Role::Executor, 0, "muse")
            .unwrap()
            .id,
    ];
    let sources = workers
        .iter()
        .map(|worker| {
            store
                .task_input(m.id, *worker, &plan)
                .unwrap()
                .unwrap()
                .source
        })
        .collect::<Vec<_>>();
    m.execution = store.execution(m.id).unwrap();
    let contexts = workers
        .iter()
        .enumerate()
        .map(|(index, worker)| {
            let mut context = Context::worker(&m, *worker, Role::Executor)
                .with_mailbox(Path::new(store.0.path().unwrap()));
            context.provider = if index == 0 { "codex" } else { "muse" }.into();
            context.muse = index == 1;
            context
        })
        .collect();
    (data, store, root, contexts, sources, workers)
}

fn arguments(source: &str, owner: i64, peer: i64, marker: &str) -> Value {
    let id = crate::task_worktree::task_id(source).unwrap();
    let content = format!(
        r#"import os, time
from pathlib import Path
assert os.getcwd() == '/tasks/{id}'
assert os.environ['HOME'] == '/home/sprowt/workers/{owner}'
assert Path('source.txt').read_text() == 'original'
for task in Path('/tasks').iterdir():
    if str(task) != os.getcwd():
        try:
            (task / 'forbidden').write_text('wrong task')
        except OSError as error:
            assert error.errno in (13, 30), error
        else:
            raise AssertionError('peer task is writable')
try:
    Path('/home/sprowt/workers/{peer}/forbidden').write_text('wrong owner')
except OSError as error:
    assert error.errno in (13, 30), error
else:
    raise AssertionError('peer HOME is writable')
home = Path(os.environ['HOME'])
(home / '{marker}-started').write_text(os.getcwd())
deadline = time.monotonic() + 25
while not (home / '{marker}-release').exists():
    if time.monotonic() > deadline:
        raise TimeoutError('test barrier not released')
    time.sleep(0.02)
print('isolated session completed')
"#
    );
    json!({"source":source,"checks":[{"check":"session is isolated","command":["/usr/bin/python3",format!("{}/check.py",checks::folder(id))],"timeout_seconds":30}],"scripts":[{"name":"check.py","content":content}]})
}

fn call(
    vm: &SharedVm,
    context: &Context,
    source: &str,
    owner: i64,
    peer: i64,
    marker: &str,
    cancelled: &AtomicBool,
) -> io::Result<Value> {
    let output = Dispatcher::shared(context, Some(vm), cancelled)
        .worker_call(checks::TOOL, arguments(source, owner, peer, marker), |_| {})?
        .message();
    Ok(serde_json::from_str(&output)?)
}

fn wait_started(vm: &SharedVm, workers: &[i64], marker: &str) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let guard = lock(vm, &AtomicBool::new(false))?;
        let started = workers.iter().all(|worker| {
            guard
                .guest_exists(&format!(
                    "{}/{marker}-started",
                    crate::task_worktree::worker_home(*worker)
                ))
                .unwrap()
        });
        drop(guard);
        if started {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(io::Error::other(
                "Both controlled checks did not reach the barrier.",
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn release(vm: &Sandbox, worker: i64, marker: &str) -> io::Result<()> {
    vm.guest(
        &[
            "/usr/bin/touch",
            &format!(
                "{}/{marker}-release",
                crate::task_worktree::worker_home(worker)
            ),
        ],
        &AtomicBool::new(false),
    )
}

#[test]
#[ignore = "uses a cached image and disposable VM; no provider inference or dependency downloads"]
fn shared_dispatcher_overlaps_isolated_checks_and_rejects_stale_receipts() {
    assert!(
        container()
            .args(["image", "inspect", IMAGE])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success(),
        "Build the pinned image before this no-download canary."
    );
    let (_data, store, root, contexts, sources, workers) = fixture();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> io::Result<()> {
        let flag = AtomicBool::new(false);
        let mut controller = Sandbox::prepare(&root, &flag, |_| {})?;
        let ids = sources
            .iter()
            .map(|source| crate::task_worktree::task_id(source).unwrap())
            .collect::<Vec<_>>();
        controller.prepare_tasks(&ids, &flag)?;
        let vm = Arc::new(Mutex::new(controller));
        let first = prepare_task(&vm, &contexts[0], &sources[0], &flag)?;
        let mut second = prepare_task(&vm, &contexts[1], &sources[1], &flag)?;
        assert_ne!(first.home(), second.home());
        assert_ne!(first.cwd(), second.cwd());
        let stopped = AtomicBool::new(false);
        let package_stop = AtomicBool::new(false);
        thread::scope(|scope| -> io::Result<()> {
            let a = scope.spawn(|| {
                call(
                    &vm,
                    &contexts[0],
                    &sources[0],
                    workers[0],
                    workers[1],
                    "parallel",
                    &flag,
                )
            });
            let b = scope.spawn(|| {
                call(
                    &vm,
                    &contexts[1],
                    &sources[1],
                    workers[1],
                    workers[0],
                    "parallel",
                    &stopped,
                )
            });
            // Neither command can finish until the host releases it. Both start first.
            if let Err(error) = wait_started(&vm, &workers, "parallel") {
                return Err(io::Error::other(format!(
                    "{error}; first: {:?}; second: {:?}",
                    a.join().unwrap(),
                    b.join().unwrap()
                )));
            }
            let gate = vm.lock().unwrap().environment.clone();
            let packages = scope.spawn(|| Dispatcher::shared(&contexts[0], Some(&vm), &package_stop).worker_call(crate::packages::TOOL, json!({"packages":["python3"],"reason":"Verify the package exclusion gate without installing"}), |_| {}));
            let deadline = Instant::now() + Duration::from_secs(2);
            while !gate.waiting_writer() && Instant::now() < deadline {
                thread::yield_now();
            }
            assert!(
                gate.waiting_writer(),
                "Package mutation must wait for check readers."
            );
            assert_eq!(
                gate.try_read().err().unwrap().kind(),
                io::ErrorKind::WouldBlock
            );
            package_stop.store(true, Ordering::Relaxed);
            assert_eq!(
                packages.join().unwrap().err().unwrap().kind(),
                io::ErrorKind::Interrupted
            );
            stopped.store(true, Ordering::Relaxed);
            release(&vm.lock().unwrap(), workers[0], "parallel")?;
            assert_eq!(a.join().unwrap()?["passed"], true);
            let cancelled = b.join().unwrap();
            assert!(cancelled.is_err() || cancelled.unwrap()["passed"] == false);
            Ok(())
        })?;
        let receipt_path = root.join("checks").join(format!("{}.json", ids[0]));
        let saved: Value = serde_json::from_slice(&fs::read(&receipt_path)?)?;
        assert_eq!(saved["receipt"]["passed"], true);
        assert_eq!(saved["receipt"]["source"], sources[0]);
        // A cancelled session does not block shutdown while another operation owns the controller.
        let held = vm.lock().unwrap();
        let stop = AtomicBool::new(false);
        thread::scope(|scope| {
            let (sent, received) = std::sync::mpsc::channel();
            let session = &mut second;
            let signal = &stop;
            scope.spawn(move || {
                let result = session.authorize(signal);
                sent.send(result.map_err(|e| e.kind())).unwrap();
            });
            stop.store(true, Ordering::Relaxed);
            assert_eq!(
                received
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap()
                    .err()
                    .unwrap(),
                io::ErrorKind::Interrupted
            );
        });
        drop(held);
        // Replace the attempt while the old check is in flight, then save newer evidence.
        thread::scope(|scope| -> io::Result<()> {
            let old = scope.spawn(|| {
                call(
                    &vm,
                    &contexts[0],
                    &sources[0],
                    workers[0],
                    workers[1],
                    "attempt",
                    &flag,
                )
            });
            wait_started(&vm, &workers[..1], "attempt")?;
            let held = vm.lock().unwrap();
            let newer_source = crate::store::task_source(ids[0], 2);
            store
                .0
                .execute(
                    "UPDATE task_runs SET source=?2 WHERE id=?1",
                    rusqlite::params![ids[0], newer_source],
                )
                .map_err(io::Error::other)?;
            let mut newer = saved.clone();
            newer["receipt"]["source"] = json!(newer_source);
            let bytes = serde_json::to_vec(&newer)?;
            fs::write(&receipt_path, &bytes)?;
            release(&held, workers[0], "attempt")?;
            drop(held);
            assert_eq!(
                old.join().unwrap().err().unwrap().kind(),
                io::ErrorKind::PermissionDenied
            );
            assert_eq!(fs::read(&receipt_path)?, bytes);
            store
                .0
                .execute(
                    "UPDATE task_runs SET source=?2 WHERE id=?1",
                    rusqlite::params![ids[0], sources[0]],
                )
                .map_err(io::Error::other)?;
            Ok(())
        })?;
        // The same protection also applies to a VM owner replacement.
        thread::scope(|scope| -> io::Result<()> {
            let old = scope.spawn(|| {
                call(
                    &vm,
                    &contexts[0],
                    &sources[0],
                    workers[0],
                    workers[1],
                    "owner",
                    &flag,
                )
            });
            wait_started(&vm, &workers[..1], "owner")?;
            let mut held = vm.lock().unwrap();
            held.tasks.owners.insert(ids[0], workers[1]);
            let bytes = serde_json::to_vec(&saved)?;
            fs::write(&receipt_path, &bytes)?;
            release(&held, workers[0], "owner")?;
            drop(held);
            assert_eq!(
                old.join().unwrap().err().unwrap().kind(),
                io::ErrorKind::PermissionDenied
            );
            assert_eq!(fs::read(&receipt_path)?, bytes);
            vm.lock().unwrap().tasks.owners.insert(ids[0], workers[0]);
            Ok(())
        })?;
        assert!(!root.read_dir()?.filter_map(Result::ok).any(|entry| {
            entry.file_name().to_string_lossy().starts_with("transfer-")
                || entry.file_name().to_string_lossy().starts_with("checks-")
        }));
        drop(first);
        drop(second);
        drop(vm);
        Ok(())
    }));
    delete(&root).unwrap();
    result.unwrap().unwrap();
}
