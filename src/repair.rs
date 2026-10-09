use std::collections::BTreeSet;

use rusqlite::{Connection, Result, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    plan::{Plan, Task},
    store::Store,
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Repair {
    pub task: String,
    pub files: Vec<String>,
    pub check: String,
    pub command: Vec<String>,
    pub evidence: String,
}

impl Repair {
    pub fn notice(&self) -> String {
        let first = self.evidence.lines().next().unwrap_or_default();
        let brief: String = first.chars().take(180).collect();
        format!(
            "Repair requested · {}\n{brief}{}",
            self.task,
            if first.chars().count() > 180 {
                "…"
            } else {
                ""
            }
        )
    }

    pub fn validate(&self, reporter: &Task) -> std::result::Result<(), String> {
        if self.task.trim().is_empty() {
            return Err("Repair report is missing the responsible task.".into());
        }
        if !reporter.checks.contains(&self.check) {
            return Err(format!(
                "Repair check must match the reporting task's declared checks ({}), not the owner's checks.",
                reporter.id
            ));
        }
        if self.command.is_empty()
            || !std::path::Path::new(&self.command[0]).is_absolute()
            || self.command.iter().any(|arg| arg.contains('\0'))
        {
            return Err("Repair command needs an absolute executable and valid arguments.".into());
        }
        if self.evidence.trim().is_empty() || self.evidence.len() > 16_000 {
            return Err("Repair evidence must be nonempty and at most 16,000 bytes.".into());
        }
        if self.files.is_empty()
            || self.files.len() > 32
            || self.files.iter().any(|file| {
                file.is_empty()
                    || file.contains(['\\', '*', '?', '[', ']'])
                    || std::path::Path::new(file)
                        .components()
                        .any(|part| !matches!(part, std::path::Component::Normal(_)))
            })
        {
            return Err("Repair files need 1–32 exact project-relative paths, without wildcards or parent paths.".into());
        }
        Ok(())
    }
}

pub fn schema() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{
        "task":{"type":"string"},"files":{"type":"array","items":{"type":"string"}},
        "check":{"type":"string"},"command":{"type":"array","items":{"type":"string"}},
        "evidence":{"type":"string"}},"required":["task","files","check","command","evidence"]})
}

pub fn migrate(connection: &Connection) -> Result<()> {
    for (table, column, sql) in [
        (
            "task_runs",
            "conflict",
            "ALTER TABLE task_runs ADD COLUMN conflict TEXT",
        ),
        (
            "task_runs",
            "conflict_retries",
            "ALTER TABLE task_runs ADD COLUMN conflict_retries INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "executions",
            "repair_count",
            "ALTER TABLE executions ADD COLUMN repair_count INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "task_runs",
            "repair",
            "ALTER TABLE task_runs ADD COLUMN repair TEXT",
        ),
        (
            "task_runs",
            "verification_feedback",
            "ALTER TABLE task_runs ADD COLUMN verification_feedback TEXT NOT NULL DEFAULT '[]'",
        ),
        (
            "task_runs",
            "verification_retries",
            "ALTER TABLE task_runs ADD COLUMN verification_retries INTEGER NOT NULL DEFAULT 0",
        ),
    ] {
        let columns = connection
            .prepare(&format!("PRAGMA table_info({table})"))?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>>>()?;
        if !columns.iter().any(|name| name == column) {
            connection.execute_batch(sql)?;
        }
    }
    Ok(())
}

fn affected(plan: &Plan, owner: &str, reporter: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::from([owner.to_owned(), reporter.to_owned()]);
    loop {
        let before = ids.len();
        for task in &plan.tasks {
            if task.depends_on.iter().any(|id| ids.contains(id)) {
                ids.insert(task.id.clone());
            }
        }
        if before == ids.len() {
            return ids;
        }
    }
}

impl Store {
    pub fn pause_repairs(&self, mod_id: i64) -> Result<()> {
        self.0.execute(
            "UPDATE task_runs SET status='repair_paused' WHERE mod_id=?1 AND status='repair_wait'",
            [mod_id],
        )?;
        Ok(())
    }

    pub fn request_repair(
        &mut self,
        mod_id: i64,
        source: &str,
        repair: &Repair,
    ) -> Result<std::result::Result<(), String>> {
        let execution = self
            .execution(mod_id)?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        let run = execution
            .tasks
            .iter()
            .find(|run| run.source == source)
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        // A recovered completed turn must not queue the same request twice.
        if run.status == "repair_wait" {
            return Ok(Ok(()));
        }
        let plan = self
            .planning(mod_id)?
            .and_then(|planning| planning.plan)
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        let reporter = plan
            .tasks
            .iter()
            .find(|task| task.id == run.task_id)
            .unwrap();
        if let Err(error) = repair.validate(reporter) {
            return Ok(Err(error));
        }
        let Some(owner) = plan.tasks.iter().find(|task| task.id == repair.task) else {
            return Ok(Err("Repair owner is not in the current plan.".into()));
        };
        if affected(&plan, &reporter.id, &reporter.id).contains(&owner.id) {
            return Ok(Err(
                "Repair owner cannot depend on the task reporting the failure.".into(),
            ));
        }
        if !execution
            .tasks
            .iter()
            .any(|run| run.task_id == owner.id && run.status == "done")
            || !repair.files.iter().all(|file| {
                owner
                    .files
                    .iter()
                    .any(|scope| file == scope || file.starts_with(&format!("{scope}/")))
            })
        {
            return Ok(Err(
                "Repair must target a completed task and stay within its file ownership.".into(),
            ));
        }
        let count: i64 = self.0.query_row(
            "SELECT repair_count FROM executions WHERE mod_id=?1",
            [mod_id],
            |row| row.get(0),
        )?;
        if count >= 2 {
            return Ok(Err(format!(
                "Automatic repair limit reached (2 attempts). {} Send an edit request to revise the plan.",
                repair.evidence
            )));
        }
        let transaction = self.0.transaction()?;
        transaction.execute("UPDATE task_runs SET status='repair_wait',repair=?3,summary=?4,checks='[]' WHERE mod_id=?1 AND source=?2",
            params![mod_id,source,serde_json::to_string(repair).unwrap(),format!("Repair requested from {} · {}", owner.title,repair.evidence)])?;
        transaction.execute("UPDATE executions SET status='repairing',repair_count=repair_count+1,checks='[]',fingerprint=NULL WHERE mod_id=?1", [mod_id])?;
        transaction.commit()?;
        Ok(Ok(()))
    }

    pub fn resume_repairs(&mut self, mod_id: i64) -> Result<bool> {
        let Some(execution) = self.execution(mod_id)? else {
            return Ok(false);
        };
        let Some(reporter) = execution
            .tasks
            .iter()
            .find(|run| run.status == "repair_wait")
        else {
            return Ok(false);
        };
        if execution
            .tasks
            .iter()
            .any(|run| ["sending", "running", "checking", "waiting"].contains(&run.status.as_str()))
        {
            return Ok(false);
        }
        let repair = reporter
            .repair
            .as_ref()
            .ok_or(rusqlite::Error::InvalidQuery)?;
        let plan = self
            .planning(mod_id)?
            .and_then(|planning| planning.plan)
            .ok_or(rusqlite::Error::InvalidQuery)?;
        let ids = affected(&plan, &repair.task, &reporter.task_id);
        // Wait for a prior repair's owner; concurrent requests share the two-attempt budget.
        if !execution
            .tasks
            .iter()
            .any(|run| run.task_id == repair.task && run.status == "done")
        {
            return Ok(false);
        }
        let transaction = self.0.transaction()?;
        for run in execution.tasks.iter().filter(|run| {
            ids.contains(&run.task_id) && (run.status != "repair_wait" || run.id == reporter.id)
        }) {
            let attempt: i64 = transaction.query_row(
                "SELECT attempt FROM task_runs WHERE id=?1",
                [run.id],
                |row| row.get(0),
            )?;
            transaction.execute("UPDATE task_runs SET status='pending',attempt=?2,source=?3,turn_id=NULL,checks='[]',repair=?4 WHERE id=?1",
                params![run.id,attempt+1,crate::store::task_source(run.id,attempt+1),serde_json::to_string(repair).unwrap()])?;
            transaction.execute(
                "UPDATE workers SET pending=NULL WHERE mod_id=?1 AND pending=?2",
                params![mod_id, run.source],
            )?;
        }
        transaction.execute(
            "UPDATE executions SET status='running',checks='[]',fingerprint=NULL WHERE mod_id=?1",
            [mod_id],
        )?;
        transaction.commit()?;
        Ok(true)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        plan::Role,
        store::{task_source, test_support::TestData},
    };
    use std::path::Path;

    pub(crate) fn fixture() -> (TestData, Store, i64, Plan, Vec<i64>, Vec<i64>) {
        let data = TestData::new();
        let mut store = data.store();
        let project = store.load_project(Path::new("/repair-test")).unwrap();
        let m = store.create_mod(project.id, "Build two parts").unwrap();
        let plan = Plan::parse(&json!({"summary":"Build", "tasks":[
            {"id":"runtime","title":"Runtime","outcome":"Loads","files":["app/runtime.js"],"depends_on":[],"worker":"muse","checks":["Loads"]},
            {"id":"ui","title":"UI","outcome":"Works","files":["app/ui.js"],"depends_on":[],"worker":"codex","checks":["Works"]},
            {"id":"integration","title":"Integration","outcome":"Verified","files":["tests"],"depends_on":["runtime","ui"],"worker":"codex","checks":["Retry works"]},
            {"id":"docs","title":"Docs","outcome":"Documented","files":["README.md"],"depends_on":["integration"],"worker":"codex","checks":["Docs match"]}
        ]}).to_string()).unwrap();
        store
            .save_plan(m.id, &m.planning.unwrap().source, &plan)
            .unwrap();
        store
            .create_execution(m.id, &data.0.join("workspace"), &plan)
            .unwrap();
        let workers = vec![
            store
                .worker_provider(m.id, Role::Executor, 0, "muse")
                .unwrap()
                .id,
            store.worker_at(m.id, Role::Executor, 0).unwrap().id,
        ];
        let ids = store
            .execution(m.id)
            .unwrap()
            .unwrap()
            .tasks
            .iter()
            .map(|run| run.id)
            .collect::<Vec<_>>();
        for (i, id) in ids.iter().enumerate().take(3) {
            store
                .0
                .execute(
                    "UPDATE task_runs SET worker_id=?2,status=?3 WHERE id=?1",
                    params![
                        id,
                        workers[usize::from(i != 0)],
                        if i == 2 { "running" } else { "done" }
                    ],
                )
                .unwrap();
        }
        (data, store, m.id, plan, ids, workers)
    }

    fn request() -> Repair {
        Repair {
            task: "runtime".into(),
            files: vec!["app/runtime.js".into()],
            check: "Retry works".into(),
            command: vec!["/usr/bin/node".into(), "/tasks/3/tests/retry.mjs".into()],
            evidence: "Rejected weight download leaves Loading disabled.".into(),
        }
    }

    #[test]
    fn repair_check_belongs_to_the_reporter_not_the_owner() {
        let (_data, mut store, mod_id, plan, ids, _) = fixture();
        let mut report =
            json!({"status":"blocked","summary":"Retry is broken","checks":[],"repair":request()});
        report["repair"]["check"] = json!(plan.tasks[0].checks[0]);
        let error = crate::execution::Report::parse(&report.to_string(), &plan.tasks[2])
            .err()
            .unwrap();
        assert!(error.contains("reporting task") && error.contains("integration"));
        report["repair"]["check"] = json!(plan.tasks[2].checks[0]);
        let report = crate::execution::Report::parse(&report.to_string(), &plan.tasks[2]).unwrap();
        assert!(
            store
                .request_repair(
                    mod_id,
                    &task_source(ids[2], 1),
                    report.repair.as_ref().unwrap()
                )
                .unwrap()
                .is_ok()
        );
        assert!(store.resume_repairs(mod_id).unwrap());
        let execution = store.execution(mod_id).unwrap().unwrap();
        assert_eq!(execution.tasks[0].status, "pending");
        assert_eq!(execution.tasks[1].status, "done");
        assert_eq!(execution.tasks[2].status, "pending");
    }

    #[test]
    fn repair_survives_restart_keeps_owners_and_invalidates_dependents() {
        let (data, mut store, mod_id, plan, ids, workers) = fixture();
        let original = task_source(ids[2], 1);
        assert!(
            store
                .request_repair(mod_id, &original, &request())
                .unwrap()
                .is_ok()
        );
        assert!(
            store
                .request_repair(mod_id, &original, &request())
                .unwrap()
                .is_ok()
        );
        let count: i64 = store
            .0
            .query_row("SELECT repair_count FROM executions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        // No more dispatch while an independent worker is finishing.
        store
            .0
            .execute(
                "UPDATE task_runs SET status='checking' WHERE id=?1",
                [ids[1]],
            )
            .unwrap();
        assert!(!store.resume_repairs(mod_id).unwrap());
        assert!(
            store
                .task_input(mod_id, workers[1], &plan)
                .unwrap()
                .is_none()
        );
        store
            .finish_task(mod_id, &task_source(ids[1], 1), "done", "UI verified", &[])
            .unwrap();
        store.pause_repairs(mod_id).unwrap();
        assert!(!store.resume_repairs(mod_id).unwrap());
        store.retry_tasks(mod_id).unwrap();
        drop(store);
        let mut store = data.store();
        assert!(store.resume_repairs(mod_id).unwrap());
        assert!(!store.resume_repairs(mod_id).unwrap());
        let execution = store.execution(mod_id).unwrap().unwrap();
        assert_eq!(execution.tasks[1].status, "done");
        for i in [0, 2, 3] {
            assert_eq!(execution.tasks[i].status, "pending");
            assert!(execution.tasks[i].checks.is_empty());
        }
        assert!(execution.checks.is_empty() && execution.fingerprint.is_none());
        assert_eq!(execution.tasks[0].worker, Some(workers[0]));
        assert!(
            store
                .task_input(mod_id, workers[1], &plan)
                .unwrap()
                .is_none()
        );
        let fix = store
            .task_input(mod_id, workers[0], &plan)
            .unwrap()
            .unwrap();
        assert!(fix.texts[0].contains(&request().evidence));
        assert!(fix.texts[0].contains("Fix this regression"));
        assert_eq!(crate::task_worktree::task_id(&fix.source).unwrap(), ids[0]);
        store
            .finish_task(mod_id, &fix.source, "done", "Fixed", &[])
            .unwrap();
        let verify = store
            .task_input(mod_id, workers[1], &plan)
            .unwrap()
            .unwrap();
        assert_eq!(
            crate::task_worktree::task_id(&verify.source).unwrap(),
            ids[2]
        );
        assert!(verify.texts[0].contains("Rerun this affected task"));
    }

    #[test]
    fn repair_rejects_invalid_ownership_and_stops_after_two_attempts() {
        let (_data, mut store, mod_id, plan, ids, workers) = fixture();
        for (task, file, command) in [
            ("missing", "app/runtime.js", "/bin/false"),
            ("runtime", "app/ui.js", "/bin/false"),
            ("runtime", "../runtime.js", "/bin/false"),
            ("runtime", "app/runtime.js", "node"),
        ] {
            let mut repair = request();
            repair.task = task.into();
            repair.files = vec![file.into()];
            repair.command = vec![command.into()];
            assert!(
                store
                    .request_repair(mod_id, &task_source(ids[2], 1), &repair)
                    .unwrap()
                    .is_err()
            );
        }
        for _ in 0..2 {
            let source = store.execution(mod_id).unwrap().unwrap().tasks[2]
                .source
                .clone();
            assert!(
                store
                    .request_repair(mod_id, &source, &request())
                    .unwrap()
                    .is_ok()
            );
            assert!(store.resume_repairs(mod_id).unwrap());
            let fix = store
                .task_input(mod_id, workers[0], &plan)
                .unwrap()
                .unwrap();
            store
                .finish_task(mod_id, &fix.source, "done", "Fixed", &[])
                .unwrap();
            store
                .task_input(mod_id, workers[1], &plan)
                .unwrap()
                .unwrap();
        }
        let source = store.execution(mod_id).unwrap().unwrap().tasks[2]
            .source
            .clone();
        let error = store
            .request_repair(mod_id, &source, &request())
            .unwrap()
            .unwrap_err();
        assert!(error.contains("2 attempts") && error.contains(&request().evidence));
        // Explicit retries never replenish the automatic budget.
        store.retry_tasks(mod_id).unwrap();
        assert!(
            store
                .request_repair(mod_id, &source, &request())
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn concurrent_requests_do_not_drop_evidence_or_deadlock_the_owner() {
        let (_data, mut store, mod_id, mut plan, ids, workers) = fixture();
        plan.tasks[3].depends_on = vec!["runtime".into()];
        let planning = store.planning(mod_id).unwrap().unwrap();
        store.save_plan(mod_id, &planning.source, &plan).unwrap();
        store
            .0
            .execute(
                "UPDATE task_runs SET status='running',worker_id=?2 WHERE id=?1",
                params![ids[3], workers[1]],
            )
            .unwrap();
        let mut second = request();
        second.check = "Docs match".into();
        second.evidence = "A second runtime regression".into();
        assert!(
            store
                .request_repair(mod_id, &task_source(ids[2], 1), &request())
                .unwrap()
                .is_ok()
        );
        assert!(
            store
                .request_repair(mod_id, &task_source(ids[3], 1), &second)
                .unwrap()
                .is_ok()
        );
        assert!(store.resume_repairs(mod_id).unwrap());
        let fix = store
            .task_input(mod_id, workers[0], &plan)
            .unwrap()
            .expect("Owner must run with another request queued");
        assert!(!store.resume_repairs(mod_id).unwrap());
        store
            .finish_task(mod_id, &fix.source, "done", "First fix", &[])
            .unwrap();
        assert!(store.resume_repairs(mod_id).unwrap());
        let fix = store
            .task_input(mod_id, workers[0], &plan)
            .unwrap()
            .unwrap();
        assert!(fix.texts[0].contains(&second.evidence));
        store
            .finish_task(mod_id, &fix.source, "done", "Second fix", &[])
            .unwrap();
        assert!(
            store
                .task_input(mod_id, workers[1], &plan)
                .unwrap()
                .is_some()
        );
    }
}
