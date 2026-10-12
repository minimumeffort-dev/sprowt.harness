use crate::{
    codex::shared_vm,
    diagnostics::timing::{Observation, Timing},
    store::Store,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const RETENTION: Duration = Duration::from_secs(90);
type Owner = Box<dyn Send>;
type Backend = Arc<
    dyn Fn(&Path, &AtomicBool, &Sender<Observation>) -> io::Result<Option<Owner>> + Send + Sync,
>;

enum State {
    Dormant,
    Warming(Job),
    Healthy(Owner),
}
struct Job {
    cancelled: Arc<AtomicBool>,
    task: JoinHandle<io::Result<Option<Owner>>>,
    events: Receiver<Observation>,
}
struct Entry {
    root: PathBuf,
    state: State,
    idle_since: Option<Instant>,
}

pub(crate) fn snapshot(root: &Path, project: &Path, plan: &str) -> io::Result<()> {
    let marker = root.join("snapshot-plan");
    if root.join("work").exists() {
        if !marker.exists()
            && root.join("snapshot-ready").exists()
            && crate::workspace::review(root)?.count() == 0
            && crate::workspace::source_state(&root.join("work"))?
                == crate::workspace::project_state(project, root)?
        {
            std::fs::write(&marker, plan)?;
        }
        if std::fs::read_to_string(&marker).ok().as_deref() != Some(plan) {
            return Err(io::Error::other(
                "The retained source snapshot belongs to an earlier plan. Its files are saved; create a new codemod for the revised plan.",
            ));
        }
        return Ok(());
    }
    std::fs::create_dir_all(root)?;
    std::fs::write(marker, plan)?;
    crate::workspace::create(project, root)?;
    std::fs::write(root.join("snapshot-ready"), b"ready")
}

pub(crate) struct Preparations {
    entries: BTreeMap<i64, Entry>,
    backend: Backend,
}

impl Preparations {
    pub fn production() -> Self {
        Self::with_backend(Arc::new(|root, cancelled, events| {
            let emit = |observation| {
                let _ = events.send(observation);
            };
            shared_vm::prewarm(root, cancelled, Timing::new("", &emit))
                .map(|vm| vm.map(|vm| Box::new(vm) as Owner))
        }))
    }

    fn with_backend(backend: Backend) -> Self {
        Self {
            entries: BTreeMap::new(),
            backend,
        }
    }

    #[cfg(test)]
    pub fn fake() -> Self {
        Self::with_backend(Arc::new(|_, _, _| Ok(Some(Box::new(())))))
    }

    pub fn root(&mut self, id: i64, store: &Store, saved: Option<&Path>) -> io::Result<PathBuf> {
        if let Some(entry) = self.entries.get(&id) {
            return Ok(entry.root.clone());
        }
        let root = match saved {
            Some(root) => root.to_owned(),
            None => store.workspace_path(id)?,
        };
        self.entries.insert(
            id,
            Entry {
                root: root.clone(),
                state: State::Dormant,
                idle_since: None,
            },
        );
        Ok(root)
    }

    pub fn start(&mut self, id: i64) {
        let entry = self.entries.get_mut(&id).unwrap();
        if !matches!(entry.state, State::Dormant) {
            return;
        }
        let backend = self.backend.clone();
        let root = entry.root.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let stopped = cancelled.clone();
        let (events, inbox) = mpsc::channel();
        let task = thread::spawn(move || backend(&root, &stopped, &events));
        entry.state = State::Warming(Job {
            cancelled,
            task,
            events: inbox,
        });
        entry.idle_since = None;
    }

    pub fn poll(&mut self, store: &Store) -> rusqlite::Result<()> {
        for (id, entry) in &mut self.entries {
            if let State::Warming(job) = &entry.state {
                for observation in job.events.try_iter() {
                    store.record_timing(*id, 0, &observation)?;
                }
                if !job.task.is_finished() {
                    continue;
                }
            } else {
                continue;
            }
            let State::Warming(job) = std::mem::replace(&mut entry.state, State::Dormant) else {
                unreachable!()
            };
            match job
                .task
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("Early VM preparation panicked.")))
            {
                Ok(Some(owner)) => entry.state = State::Healthy(owner),
                Ok(None) => {}
                Err(error) => store.record_preparation_failure(*id, &error.to_string())?,
            }
            for observation in job.events.try_iter() {
                store.record_timing(*id, 0, &observation)?;
            }
        }
        Ok(())
    }

    pub fn cancel(&mut self, id: i64, store: &Store) -> rusqlite::Result<()> {
        let Some(entry) = self.entries.get_mut(&id) else {
            return Ok(());
        };
        let previous = std::mem::replace(&mut entry.state, State::Dormant);
        if let State::Warming(job) = previous {
            job.cancelled.store(true, Ordering::Relaxed);
            let _ = job.task.join();
            for observation in job.events.try_iter() {
                store.record_timing(id, 0, &observation)?;
            }
        }
        entry.idle_since = None;
        Ok(())
    }

    pub fn expired(&mut self, active: &BTreeSet<i64>, now: Instant) -> Vec<i64> {
        self.entries
            .iter_mut()
            .filter_map(|(id, entry)| {
                if active.contains(id) {
                    entry.idle_since = None;
                    return None;
                }
                let since = entry.idle_since.get_or_insert(now);
                (now.saturating_duration_since(*since) >= RETENTION).then_some(*id)
            })
            .collect()
    }
}

impl Drop for Preparations {
    fn drop(&mut self) {
        for entry in self.entries.values_mut() {
            match std::mem::replace(&mut entry.state, State::Dormant) {
                State::Warming(job) => {
                    job.cancelled.store(true, Ordering::Relaxed);
                    let _ = job.task.join();
                }
                State::Healthy(owner) => drop(owner),
                State::Dormant => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, atomic::AtomicUsize};

    #[test]
    fn speculative_failure_is_activity_evidence_and_does_not_fail_the_plan() {
        let data = crate::store::test_support::TestData::new();
        let mut store = data.store();
        let project = store.load_project(Path::new("/preparation-test")).unwrap();
        let code_mod = store.create_mod(project.id, "Change source").unwrap();
        let mut preparations = Preparations::with_backend(Arc::new(|_, _, _| {
            Err(io::Error::other("VM capacity unavailable"))
        }));
        preparations.root(code_mod.id, &store, None).unwrap();
        preparations.start(code_mod.id);
        let deadline = Instant::now() + Duration::from_secs(1);
        while matches!(preparations.entries[&code_mod.id].state, State::Warming(_))
            && Instant::now() < deadline
        {
            preparations.poll(&store).unwrap();
            thread::yield_now();
        }
        let history = store.diagnostics(code_mod.id).unwrap();
        assert_eq!(history.failures[0].evidence, "VM capacity unavailable");
        assert_eq!(
            store.planning(code_mod.id).unwrap().unwrap().status,
            "pending"
        );
        assert_eq!(
            store
                .load_project(Path::new("/preparation-test"))
                .unwrap()
                .mods[0]
                .messages
                .len(),
            1
        );
    }

    #[test]
    fn execution_record_failure_joins_preparation_and_retry_keeps_accepted_source() {
        let data = crate::store::test_support::TestData::new();
        let project = data.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("source.txt"), "accepted").unwrap();
        let mut store = data.store();
        let project_id = store.load_project(&project).unwrap().id;
        let code_mod = store.create_mod(project_id, "Change source").unwrap();
        let plan = crate::plan::Plan::parse(r#"{"summary":"Edit source","tasks":[{"id":"one","title":"Edit","outcome":"Update source","files":["source.txt"],"depends_on":[],"worker":"codex","checks":["expected source"]}]}"#).unwrap();
        store
            .save_plan(
                code_mod.id,
                &code_mod.planning.as_ref().unwrap().source,
                &plan,
            )
            .unwrap();
        let ended = Arc::new(AtomicBool::new(false));
        let joined = ended.clone();
        let preparations = Preparations::with_backend(Arc::new(move |_, cancelled, _| {
            while !cancelled.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(1));
            }
            joined.store(true, Ordering::Relaxed);
            Ok(None)
        }));
        let mut app =
            super::super::Engine::load_with_preparations(project.clone(), store, preparations)
                .unwrap();
        let root = app
            .preparations
            .root(code_mod.id, &app.store, None)
            .unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("vm.json"), "recoverable VM metadata").unwrap();
        app.preparations.start(code_mod.id);
        app.store.0.execute_batch("CREATE TRIGGER fail_execution BEFORE INSERT ON executions BEGIN SELECT RAISE(ABORT, 'record failure'); END;").unwrap();
        assert!(
            app.prepare_execution(0)
                .unwrap_err()
                .to_string()
                .contains("record failure")
        );
        assert!(ended.load(Ordering::Relaxed));
        assert_eq!(
            std::fs::read_to_string(root.join("vm.json")).unwrap(),
            "recoverable VM metadata"
        );
        std::fs::write(project.join("source.txt"), "later host edit").unwrap();
        app.store
            .0
            .execute_batch("DROP TRIGGER fail_execution;")
            .unwrap();
        app.prepare_execution(0).unwrap();
        assert_eq!(app.mods[0].execution.as_ref().unwrap().workspace, root);
        assert_eq!(
            std::fs::read_to_string(root.join("work/source.txt")).unwrap(),
            "accepted"
        );
    }

    #[test]
    fn snapshot_retry_preserves_source_and_rejects_a_different_plan() {
        let data = crate::store::test_support::TestData::new();
        let project = data.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("source.txt"), "accepted").unwrap();
        let root = data.0.join("warm-root");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("vm.json"), "retained metadata").unwrap();
        snapshot(&root, &project, "plan-1").unwrap();
        std::fs::write(project.join("source.txt"), "later host edit").unwrap();
        snapshot(&root, &project, "plan-1").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("work/source.txt")).unwrap(),
            "accepted"
        );
        assert!(
            snapshot(&root, &project, "plan-2")
                .unwrap_err()
                .to_string()
                .contains("earlier plan")
        );
        assert_eq!(
            std::fs::read_to_string(root.join("vm.json")).unwrap(),
            "retained metadata"
        );
    }

    #[test]
    fn intact_legacy_snapshot_is_adopted_but_changed_work_is_retained() {
        let data = crate::store::test_support::TestData::new();
        let project = data.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("source.txt"), "starting source").unwrap();
        let root = data.0.join("legacy-root");
        crate::workspace::create(&project, &root).unwrap();
        std::fs::write(root.join("snapshot-ready"), b"ready").unwrap();
        snapshot(&root, &project, "accepted-plan").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("snapshot-plan")).unwrap(),
            "accepted-plan"
        );
        std::fs::remove_file(root.join("snapshot-plan")).unwrap();
        std::fs::write(root.join("work/source.txt"), "recoverable edit").unwrap();
        assert!(snapshot(&root, &project, "revised-plan").is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("work/source.txt")).unwrap(),
            "recoverable edit"
        );
    }

    #[test]
    fn captured_root_is_stable_and_pause_joins_before_retry() {
        let data = crate::store::test_support::TestData::new();
        let store = data.store();
        let starts = Arc::new(Mutex::new(Vec::new()));
        let ended = Arc::new(AtomicUsize::new(0));
        let seen = starts.clone();
        let finished = ended.clone();
        let mut preparations = Preparations::with_backend(Arc::new(move |root, cancelled, _| {
            seen.lock().unwrap().push(root.to_owned());
            while !cancelled.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(1));
            }
            finished.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }));
        let root = preparations.root(7, &store, None).unwrap();
        assert_eq!(preparations.root(7, &store, None).unwrap(), root);
        preparations.start(7);
        while starts.lock().unwrap().is_empty() {
            thread::yield_now();
        }
        preparations.cancel(7, &store).unwrap();
        assert_eq!(ended.load(Ordering::SeqCst), 1);
        preparations.start(7);
        while starts.lock().unwrap().len() < 2 {
            thread::yield_now();
        }
        preparations.cancel(7, &store).unwrap();
        assert_eq!(*starts.lock().unwrap(), vec![root.clone(), root]);
        assert_eq!(ended.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn enabled_waiters_keep_owners_and_idle_retirement_releases_them() {
        let data = crate::store::test_support::TestData::new();
        let store = data.store();
        let mut preparations = Preparations::fake();
        preparations.root(1, &store, None).unwrap();
        preparations.start(1);
        let deadline = Instant::now() + Duration::from_secs(1);
        while matches!(preparations.entries[&1].state, State::Warming(_))
            && Instant::now() < deadline
        {
            preparations.poll(&store).unwrap();
            thread::yield_now();
        }
        let now = Instant::now();
        assert!(
            preparations
                .expired(&BTreeSet::from([1]), now + RETENTION)
                .is_empty()
        );
        assert!(preparations.expired(&BTreeSet::new(), now).is_empty());
        assert_eq!(
            preparations.expired(&BTreeSet::new(), now + RETENTION),
            vec![1]
        );
        preparations.cancel(1, &store).unwrap();
        assert!(matches!(preparations.entries[&1].state, State::Dormant));
    }
}
