use rusqlite::{OptionalExtension, Result, params};

use crate::{
    plan::{Plan, Role, Task},
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
        providers: &[&str],
    ) -> Result<Vec<WorkerRecord>> {
        let mut records = self.executors(mod_id)?;
        if !records.iter().any(|r| busy.contains(&r.id)) && unavailable.is_empty() {
            self.resume_repairs(mod_id)?;
        }
        let execution = self
            .execution(mod_id)?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        let mut selected = records
            .iter()
            .filter(|r| busy.contains(&r.id))
            .map(|r| r.id)
            .collect::<Vec<_>>();
        if !matches!(execution.status.as_str(), "review" | "applied" | "planning") {
            // Reconnect saved deliveries before assigning new work.
            for record in &records {
                if selected.len() < CAPACITY
                    && !selected.contains(&record.id)
                    && !unavailable.contains(&record.id)
                    && providers.contains(&record.provider.as_str())
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
                let (provider, reason) = if let Some(owner) = run.worker {
                    let Some(record) = records.iter().find(|r| r.id == owner) else {
                        continue;
                    };
                    if !task.accepts(&record.provider)
                        || !providers.contains(&record.provider.as_str())
                    {
                        continue;
                    }
                    (record.provider.clone(), run.assignment_reason.clone())
                } else {
                    let Some(choice) = self.assign_provider(task, providers, busy, &selected)?
                    else {
                        continue;
                    };
                    choice
                };
                let worker = if let Some(owner) = run.worker {
                    Some(owner)
                } else {
                    let idle = records.iter().find(|record| {
                        record.provider == provider
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
                        let count = records.iter().filter(|r| r.provider == provider).count();
                        let record =
                            self.worker_provider(mod_id, Role::Executor, count, &provider)?;
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
                    if run.worker.is_none() && self.0.execute("UPDATE task_runs SET worker_id=?2,assignment_reason=?3,assignment_order=(SELECT COALESCE(MAX(assignment_order),0)+1 FROM task_runs) WHERE id=?1 AND status='pending' AND worker_id IS NULL", params![run.id,worker,reason])? != 1 {
                        continue;
                    }
                    selected.push(worker);
                }
            }
            if selected.is_empty() && execution.complete() {
                if let Some(record) = records.iter().find(|r| {
                    !unavailable.contains(&r.id) && providers.contains(&r.provider.as_str())
                }) {
                    selected.push(record.id);
                } else if records.is_empty() {
                    let Some(provider) = providers.first() else {
                        return Ok(vec![]);
                    };
                    let record = self.worker_provider(mod_id, Role::Executor, 0, provider)?;
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

    fn assign_provider(
        &self,
        task: &Task,
        providers: &[&str],
        busy: &[i64],
        selected: &[i64],
    ) -> Result<Option<(String, String)>> {
        let candidates = providers
            .iter()
            .copied()
            .filter(|p| task.accepts(p))
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Ok(None);
        }
        if task.worker != "auto" {
            return Ok(Some((
                task.worker.clone(),
                task.provider_reason
                    .clone()
                    .unwrap_or_else(|| "saved plan assignment".into()),
            )));
        }
        if candidates.len() == 1 {
            return Ok(Some((
                candidates[0].into(),
                "only available provider".into(),
            )));
        }
        let workers = self
            .0
            .prepare("SELECT id,provider FROM workers WHERE role='executor'")?
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>>>()?;
        let load = |provider: &str| {
            workers
                .iter()
                .filter(|(id, p)| p == provider && (busy.contains(id) || selected.contains(id)))
                .count()
        };
        let lowest = candidates.iter().map(|p| load(p)).min().unwrap();
        let tied = candidates
            .iter()
            .copied()
            .filter(|p| load(p) == lowest)
            .collect::<Vec<_>>();
        let last: Option<String> = self.0.query_row("SELECT w.provider FROM task_runs t JOIN workers w ON w.id=t.worker_id WHERE t.assignment_order>0 ORDER BY t.assignment_order DESC LIMIT 1", [], |row| row.get(0)).optional()?;
        let provider = tied
            .iter()
            .copied()
            .find(|p| Some(*p) != last.as_deref())
            .unwrap_or(tied[0]);
        let reason = if tied.len() == 1 {
            "equally suitable, lower load"
        } else {
            "equally suitable, alternating tie"
        };
        Ok(Some((provider.into(), reason.into())))
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
            "files":[format!("{i}.txt")], "worker":provider, "provider_reason":if *provider == "auto" { Some("") } else { None }, "checks":["File is correct"],
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
    fn automatic_work_balances_ready_tasks_and_assigns_integration_after_dependencies() {
        let (_data, mut store, id, plan) = fixture(&["auto", "auto", "auto"], true);
        let pool = ["codex", "muse"];
        let workers = store.schedule_workers(id, &plan, &[], &[], &pool).unwrap();
        assert_eq!(
            workers
                .iter()
                .map(|w| w.provider.as_str())
                .collect::<Vec<_>>(),
            pool
        );
        let execution = store.execution(id).unwrap().unwrap();
        assert_eq!(
            execution.tasks[0].assignment_reason,
            "equally suitable, alternating tie"
        );
        assert_eq!(
            execution.tasks[1].assignment_reason,
            "equally suitable, lower load"
        );
        assert!(execution.tasks[2].worker.is_none());
        for w in &workers {
            let input = store.task_input(id, w.id, &plan).unwrap().unwrap();
            store.pending(w.id, None).unwrap();
            store
                .finish_task(id, &input.source, "done", "Checked", &[])
                .unwrap();
        }
        let integration = store.schedule_workers(id, &plan, &[], &[], &pool).unwrap();
        assert_eq!(integration.len(), 1);
        assert_eq!(integration[0].provider, "codex");
        assert_eq!(
            store.execution(id).unwrap().unwrap().tasks[2].worker,
            Some(integration[0].id)
        );
    }

    #[test]
    fn load_across_codemods_can_select_two_of_either_provider() {
        for (busy_provider, chosen) in [("codex", "muse"), ("muse", "codex")] {
            let (_data, mut store, id, plan) = fixture(&["auto", "auto"], false);
            let project = store
                .0
                .query_row("SELECT project_id FROM code_mods WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .unwrap();
            let other = store.create_mod(project, "Other work").unwrap().id;
            let busy = (0..2)
                .map(|slot| {
                    store
                        .worker_provider(other, Role::Executor, slot, busy_provider)
                        .unwrap()
                        .id
                })
                .collect::<Vec<_>>();
            let workers = store
                .schedule_workers(id, &plan, &busy, &[], &["codex", "muse"])
                .unwrap();
            assert_eq!(workers.len(), 2);
            assert!(workers.iter().all(|w| w.provider == chosen));
            assert!(
                store
                    .execution(id)
                    .unwrap()
                    .unwrap()
                    .tasks
                    .iter()
                    .all(|t| t.assignment_reason.contains("lower load"))
            );
        }
    }

    #[test]
    fn assignments_and_tie_rotation_survive_restart_and_missing_providers() {
        let (data, mut store, id, plan) = fixture(&["auto", "auto", "auto"], true);
        assert!(
            store
                .task_input(id, store.worker_for(id, Role::Executor).unwrap().id, &plan)
                .unwrap()
                .is_none()
        );
        let pool = ["codex", "muse"];
        let workers = store.schedule_workers(id, &plan, &[], &[], &pool).unwrap();
        let owners = workers.iter().map(|w| w.id).collect::<Vec<_>>();
        let reasons = store
            .execution(id)
            .unwrap()
            .unwrap()
            .tasks
            .iter()
            .map(|t| t.assignment_reason.clone())
            .collect::<Vec<_>>();
        for w in &workers {
            store
                .save_thread(w.id, &format!("native-{}", w.id))
                .unwrap();
            let input = store.task_input(id, w.id, &plan).unwrap().unwrap();
            store.pending(w.id, None).unwrap();
            store
                .finish_task(id, &input.source, "paused", "Stopped", &[])
                .unwrap();
        }
        drop(store);
        let mut store = data.store();
        store.retry_tasks(id).unwrap();
        let only_codex = store
            .schedule_workers(id, &plan, &[], &[], &["codex"])
            .unwrap();
        assert_eq!(only_codex.len(), 1);
        assert_eq!(only_codex[0].id, owners[0]);
        assert_eq!(
            store.execution(id).unwrap().unwrap().tasks[1].worker,
            Some(owners[1])
        );
        let workers = store.schedule_workers(id, &plan, &[], &[], &pool).unwrap();
        assert_eq!(workers.iter().map(|w| w.id).collect::<Vec<_>>(), owners);
        assert!(
            workers
                .iter()
                .all(|w| w.thread_id.as_deref() == Some(format!("native-{}", w.id).as_str()))
        );
        assert_eq!(
            store
                .execution(id)
                .unwrap()
                .unwrap()
                .tasks
                .iter()
                .map(|t| t.assignment_reason.clone())
                .collect::<Vec<_>>(),
            reasons
        );
        for w in workers {
            let input = store.task_input(id, w.id, &plan).unwrap().unwrap();
            store.pending(w.id, None).unwrap();
            store
                .finish_task(id, &input.source, "done", "Checked", &[])
                .unwrap();
        }
        assert_eq!(
            store.schedule_workers(id, &plan, &[], &[], &pool).unwrap()[0].provider,
            "codex"
        );
    }

    #[test]
    fn only_available_providers_are_used_and_capability_assignments_are_respected() {
        let (_data, mut store, id, plan) = fixture(&["auto", "auto"], false);
        let workers = store
            .schedule_workers(id, &plan, &[], &[], &["codex"])
            .unwrap();
        assert_eq!(workers.len(), 2);
        assert!(workers.iter().all(|w| w.provider == "codex"));
        assert!(
            store
                .execution(id)
                .unwrap()
                .unwrap()
                .tasks
                .iter()
                .all(|t| t.assignment_reason == "only available provider")
        );
        let (_data, mut store, id, mut plan) = fixture(&["muse"], false);
        plan.tasks[0].provider_reason = Some("Requires the Muse-specific tool".into());
        assert!(
            store
                .schedule_workers(id, &plan, &[], &[], &["codex"])
                .unwrap()
                .is_empty()
        );
        assert!(
            store.execution(id).unwrap().unwrap().tasks[0]
                .worker
                .is_none()
        );
        let workers = store
            .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
            .unwrap();
        assert_eq!(workers[0].provider, "muse");
        assert_eq!(
            store.execution(id).unwrap().unwrap().tasks[0].assignment_reason,
            "Requires the Muse-specific tool"
        );
    }

    #[test]
    fn missing_provider_keeps_uncertain_delivery_and_legacy_state_migrates() {
        let (data, mut store, id, plan) = fixture(&["muse"], false);
        let owner = store
            .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
            .unwrap()
            .remove(0);
        store.save_thread(owner.id, "saved-muse-session").unwrap();
        let input = store.task_input(id, owner.id, &plan).unwrap().unwrap();
        store.0.execute_batch("ALTER TABLE task_runs DROP COLUMN assignment_reason; ALTER TABLE task_runs DROP COLUMN assignment_order; ALTER TABLE task_runs DROP COLUMN verification_feedback; ALTER TABLE task_runs DROP COLUMN verification_retries;").unwrap();
        drop(store);
        let mut store = data.store();
        assert!(
            store
                .schedule_workers(id, &plan, &[], &[], &["codex"])
                .unwrap()
                .is_empty()
        );
        let run = store.execution(id).unwrap().unwrap().tasks.remove(0);
        assert_eq!(run.worker, Some(owner.id));
        assert_eq!(run.provider.as_deref(), Some("muse"));
        assert!(run.assignment_reason.is_empty());
        assert!(run.verification_feedback.is_empty());
        let resumed = store
            .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
            .unwrap()
            .remove(0);
        assert_eq!(resumed.id, owner.id);
        assert_eq!(resumed.pending.as_deref(), Some(input.source.as_str()));
        assert_eq!(resumed.thread_id.as_deref(), Some("saved-muse-session"));
    }

    #[test]
    fn ready_work_selects_each_provider_combination() {
        for providers in [["codex", "codex"], ["codex", "muse"], ["muse", "muse"]] {
            let (_data, mut store, id, plan) = fixture(&providers, false);
            let records = store
                .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
                .unwrap();
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
        let first = store
            .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
            .unwrap();
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
                .schedule_workers(id, &plan, &occupied, &[], &["codex", "muse"])
                .unwrap()
                .len(),
            2
        );
        assert_eq!(store.executors(id).unwrap().len(), 2);
        let second = store
            .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
            .unwrap();
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
        let resumed = store
            .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
            .unwrap();
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
        let records = store
            .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
            .unwrap();
        let input = store.task_input(id, records[0].id, &plan).unwrap().unwrap();
        let resumed = store
            .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
            .unwrap();
        assert_eq!(resumed[0].pending.as_deref(), Some(input.source.as_str()));
        let active = [records[1].id];
        let resumed = store
            .schedule_workers(id, &plan, &active, &[records[0].id], &["codex", "muse"])
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
        let first = store
            .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
            .unwrap();
        assert_eq!(first.len(), 1);
        let input = store.task_input(id, first[0].id, &plan).unwrap().unwrap();
        assert_eq!(
            store
                .schedule_workers(id, &plan, &[first[0].id], &[], &["codex", "muse"])
                .unwrap()
                .len(),
            1
        );
        store.pending(first[0].id, None).unwrap();
        store
            .finish_task(id, &input.source, "done", "Done", &[])
            .unwrap();
        let second = store
            .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
            .unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].provider, "muse");
        let input = store.task_input(id, second[0].id, &plan).unwrap().unwrap();
        store.pending(second[0].id, None).unwrap();
        store
            .finish_task(id, &input.source, "done", "Done", &[])
            .unwrap();
        assert_eq!(
            store
                .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
                .unwrap()
                .len(),
            1
        );
        store.execution_status(id, "review").unwrap();
        assert!(
            store
                .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_parked_repair_owner_keeps_its_identity_ahead_of_the_verifier() {
        let (_data, mut store, id, mut plan, tasks, workers) = crate::repair::tests::fixture();
        for task in &mut plan.tasks {
            task.worker = "auto".into();
            task.provider_reason = Some(String::new());
        }
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
        // Another codemod's busy worker must not prevent repair admission here.
        let project = store
            .0
            .query_row("SELECT project_id FROM code_mods WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        let other = store.create_mod(project, "Other work").unwrap().id;
        let busy = [store
            .worker_provider(other, Role::Executor, 0, "codex")
            .unwrap()
            .id];
        let selected = store
            .schedule_workers(id, &plan, &busy, &[], &["codex", "muse"])
            .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, workers[0]);
        assert_eq!(selected[0].thread_id.as_deref(), Some("native-muse-owner"));
        let input = store.task_input(id, workers[0], &plan).unwrap().unwrap();
        store.pending(workers[0], None).unwrap();
        store
            .finish_task(id, &input.source, "done", "Repaired", &[])
            .unwrap();
        let selected = store
            .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
            .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, workers[1]);
        assert_eq!(selected[0].provider, "codex");
    }
}
