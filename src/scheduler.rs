use rusqlite::{Result, params};

use crate::{
    plan::{Plan, Role},
    store::{Store, WorkerRecord},
};

pub const CAPACITY: usize = 2;

impl Store {
    pub fn executors(&self, mod_id: i64) -> Result<Vec<WorkerRecord>> {
        let database = std::path::Path::new(self.0.path().unwrap()).to_owned();
        let mut query = self.0.prepare("SELECT id,provider,thread_id,pending FROM workers WHERE mod_id=?1 AND role='executor' ORDER BY id")?;
        query
            .query_map([mod_id], |row| {
                Ok(WorkerRecord {
                    database: database.clone(),
                    id: row.get(0)?,
                    provider: row.get(1)?,
                    thread_id: row.get(2)?,
                    pending: row.get(3)?,
                })
            })?
            .collect()
    }

    pub fn schedule_workers(
        &mut self,
        mod_id: i64,
        plan: &Plan,
        busy: &[i64],
        unavailable: &[i64],
    ) -> Result<Vec<WorkerRecord>> {
        if busy.is_empty() && unavailable.is_empty() {
            self.resume_repairs(mod_id)?;
        }
        let execution = self
            .execution(mod_id)?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        let mut records = self.executors(mod_id)?;
        let mut selected = busy.to_vec();
        if !matches!(execution.status.as_str(), "review" | "applied" | "planning") {
            // Reconnect saved deliveries before assigning new work.
            for record in &records {
                if selected.len() < CAPACITY
                    && !selected.contains(&record.id)
                    && !unavailable.contains(&record.id)
                    && (record.pending.is_some()
                        || execution.tasks.iter().any(|run| {
                            run.worker == Some(record.id)
                                && matches!(run.status.as_str(), "sending" | "running" | "checking")
                        }))
                {
                    selected.push(record.id);
                }
            }
            for run in execution.ready_tasks(plan) {
                if selected.len() >= CAPACITY {
                    break;
                }
                let task = plan
                    .tasks
                    .iter()
                    .find(|task| task.id == run.task_id)
                    .unwrap();
                let worker = if let Some(owner) = run.worker {
                    records
                        .iter()
                        .find(|r| r.id == owner && r.provider == task.worker)
                        .map(|r| r.id)
                } else {
                    let idle = records.iter().find(|record| {
                        record.provider == task.worker
                            && record.pending.is_none()
                            && !selected.contains(&record.id)
                            && !unavailable.contains(&record.id)
                            && !execution.tasks.iter().any(|r| {
                                r.worker == Some(record.id)
                                    && matches!(
                                        r.status.as_str(),
                                        "sending"
                                            | "running"
                                            | "checking"
                                            | "blocked"
                                            | "paused"
                                            | "waiting"
                                            | "repair_wait"
                                    )
                            })
                    });
                    if let Some(record) = idle {
                        Some(record.id)
                    } else {
                        let count = records.iter().filter(|r| r.provider == task.worker).count();
                        let record =
                            self.worker_provider(mod_id, Role::Executor, count, &task.worker)?;
                        let id = record.id;
                        records.push(record);
                        Some(id)
                    }
                };
                if let Some(worker) = worker
                    && !selected.contains(&worker)
                    && !unavailable.contains(&worker)
                {
                    // Reserve ownership, not delivery. Pending receipts start at submission.
                    self.0.execute("UPDATE task_runs SET worker_id=?2 WHERE id=?1 AND status='pending' AND worker_id IS NULL", params![run.id,worker])?;
                    selected.push(worker);
                }
            }
            if selected.is_empty() && execution.complete() {
                if let Some(record) = records.iter().find(|r| !unavailable.contains(&r.id)) {
                    selected.push(record.id);
                } else if records.is_empty() {
                    let record =
                        self.worker_provider(mod_id, Role::Executor, 0, &plan.tasks[0].worker)?;
                    selected.push(record.id);
                    records.push(record);
                }
            }
        }
        Ok(records
            .into_iter()
            .filter(|record| selected.contains(&record.id))
            .collect())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::store::test_support::TestData;
    use serde_json::json;

    pub(crate) fn fixture(providers: &[&str], waves: bool) -> (TestData, Store, i64, Plan) {
        let data = TestData::new();
        let mut store = data.store();
        let project = store.load_project(&data.0.join("project")).unwrap().id;
        let m = store.create_mod(project, "Independent work").unwrap();
        let tasks = providers.iter().enumerate().map(|(i, provider)| json!({
            "id":format!("task{i}"), "title":format!("Task {}", i+1), "outcome":"Write the task file",
            "files":[format!("{i}.txt")], "worker":provider, "checks":["File is correct"],
            "depends_on":if waves && i >= 2 { vec!["task0", "task1"] } else { vec![] }
        })).collect::<Vec<_>>();
        let plan =
            Plan::parse(&json!({"summary":"Independent work", "tasks":tasks}).to_string()).unwrap();
        store
            .save_plan(m.id, &m.planning.as_ref().unwrap().source, &plan)
            .unwrap();
        store
            .create_execution(m.id, &data.0.join("workspace"), &plan)
            .unwrap();
        (data, store, m.id, plan)
    }

    #[test]
    fn ready_work_selects_each_provider_combination() {
        for providers in [["codex", "codex"], ["codex", "muse"], ["muse", "muse"]] {
            let (_data, mut store, id, plan) = fixture(&providers, false);
            let records = store.schedule_workers(id, &plan, &[], &[]).unwrap();
            assert_eq!(
                records
                    .iter()
                    .map(|r| r.provider.as_str())
                    .collect::<Vec<_>>(),
                providers
            );
            assert_ne!(records[0].id, records[1].id);
            assert!(records.iter().all(|r| r.pending.is_none()));
            for record in &records {
                assert!(store.task_input(id, record.id, &plan).unwrap().is_some());
            }
            let execution = store.execution(id).unwrap().unwrap();
            assert!(
                execution
                    .tasks
                    .iter()
                    .all(|r| r.status == "sending" && r.worker.is_some())
            );
        }
    }

    #[test]
    fn provider_waves_fill_capacity_and_keep_owned_retries() {
        let (data, mut store, id, plan) = fixture(&["codex", "codex", "muse", "muse"], true);
        let first = store.schedule_workers(id, &plan, &[], &[]).unwrap();
        assert_eq!(first.len(), 2);
        assert!(first.iter().all(|r| r.provider == "codex"));
        for record in &first {
            let input = store.task_input(id, record.id, &plan).unwrap().unwrap();
            store.pending(record.id, None).unwrap();
            store
                .finish_task(id, &input.source, "done", "Done", &[])
                .unwrap();
        }
        // Connecting and finalizing workers still occupy their slots.
        let occupied = first.iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(
            store
                .schedule_workers(id, &plan, &occupied, &[])
                .unwrap()
                .len(),
            2
        );
        assert_eq!(store.executors(id).unwrap().len(), 2);
        let second = store.schedule_workers(id, &plan, &[], &[]).unwrap();
        assert_eq!(second.len(), 2);
        assert!(second.iter().all(|r| r.provider == "muse"));
        for record in &second {
            store
                .save_thread(record.id, &format!("native-{}", record.id))
                .unwrap();
            let input = store.task_input(id, record.id, &plan).unwrap().unwrap();
            store
                .finish_task(id, &input.source, "paused", "Stopped", &[])
                .unwrap();
        }
        drop(store);
        let mut store = data.store();
        store.retry_tasks(id).unwrap();
        let resumed = store.schedule_workers(id, &plan, &[], &[]).unwrap();
        assert_eq!(
            resumed.iter().map(|r| r.id).collect::<Vec<_>>(),
            second.iter().map(|r| r.id).collect::<Vec<_>>()
        );
        assert!(
            resumed
                .iter()
                .all(|r| r.thread_id.as_deref() == Some(format!("native-{}", r.id).as_str()))
        );
        let execution = store.execution(id).unwrap().unwrap();
        assert!(execution.tasks[..2].iter().all(|r| r.status == "done"));
        assert!(
            execution.tasks[2..]
                .iter()
                .all(|r| r.worker.is_some() && r.status == "pending")
        );
    }

    #[test]
    fn uncertain_delivery_has_priority_and_failed_workers_are_not_replaced() {
        let (_data, mut store, id, plan) = fixture(&["codex", "muse", "codex"], false);
        let records = store.schedule_workers(id, &plan, &[], &[]).unwrap();
        let input = store.task_input(id, records[0].id, &plan).unwrap().unwrap();
        let resumed = store.schedule_workers(id, &plan, &[], &[]).unwrap();
        assert_eq!(resumed[0].pending.as_deref(), Some(input.source.as_str()));
        let active = [records[1].id];
        let resumed = store
            .schedule_workers(id, &plan, &active, &[records[0].id])
            .unwrap();
        assert!(resumed.iter().all(|r| r.id != records[0].id));
        assert_eq!(
            store.execution(id).unwrap().unwrap().tasks[0].worker,
            Some(records[0].id)
        );
    }

    #[test]
    fn dependencies_wait_and_final_checks_use_one_existing_worker() {
        let (_data, mut store, id, mut plan) = fixture(&["codex", "muse"], false);
        plan.tasks[1].depends_on = vec![plan.tasks[0].id.clone()];
        let first = store.schedule_workers(id, &plan, &[], &[]).unwrap();
        assert_eq!(first.len(), 1);
        let input = store.task_input(id, first[0].id, &plan).unwrap().unwrap();
        assert_eq!(
            store
                .schedule_workers(id, &plan, &[first[0].id], &[])
                .unwrap()
                .len(),
            1
        );
        store.pending(first[0].id, None).unwrap();
        store
            .finish_task(id, &input.source, "done", "Done", &[])
            .unwrap();
        let second = store.schedule_workers(id, &plan, &[], &[]).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].provider, "muse");
        let input = store.task_input(id, second[0].id, &plan).unwrap().unwrap();
        store.pending(second[0].id, None).unwrap();
        store
            .finish_task(id, &input.source, "done", "Done", &[])
            .unwrap();
        assert_eq!(
            store.schedule_workers(id, &plan, &[], &[]).unwrap().len(),
            1
        );
        store.execution_status(id, "review").unwrap();
        assert!(
            store
                .schedule_workers(id, &plan, &[], &[])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_parked_repair_owner_keeps_its_identity_ahead_of_the_verifier() {
        let (_data, mut store, id, plan, tasks, workers) = crate::repair::tests::fixture();
        store
            .worker_provider(id, Role::Executor, 1, "codex")
            .unwrap();
        store.save_thread(workers[0], "native-muse-owner").unwrap();
        let repair = crate::repair::Repair {
            task: "runtime".into(),
            files: vec!["app/runtime.js".into()],
            check: "Retry works".into(),
            command: vec!["/usr/bin/node".into(), "/tasks/3/tests/retry.mjs".into()],
            evidence: "Rejected download leaves Loading disabled.".into(),
        };
        assert!(
            store
                .request_repair(id, &crate::store::task_source(tasks[2], 1), &repair)
                .unwrap()
                .is_ok()
        );
        store.pause_repairs(id).unwrap();
        drop(store);
        let mut store = _data.store();
        store.retry_tasks(id).unwrap();
        let selected = store.schedule_workers(id, &plan, &[], &[]).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, workers[0]);
        assert_eq!(selected[0].thread_id.as_deref(), Some("native-muse-owner"));
        let input = store.task_input(id, workers[0], &plan).unwrap().unwrap();
        store.pending(workers[0], None).unwrap();
        store
            .finish_task(id, &input.source, "done", "Repaired", &[])
            .unwrap();
        let selected = store.schedule_workers(id, &plan, &[], &[]).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, workers[1]);
        assert_eq!(selected[0].provider, "codex");
    }
}
