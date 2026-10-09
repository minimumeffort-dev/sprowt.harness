use std::{fmt, path::Path};

use rusqlite::{Result, params};
use serde::{Deserialize, Serialize};

use crate::{plan::Task, store::Store};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MergeConflict {
    pub files: Vec<ConflictFile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConflictFile {
    pub path: String,
    pub text: bool,
    pub fingerprint: String,
    #[serde(default)]
    pub owners: Vec<String>,
}

pub fn has_markers(bytes: &[u8]) -> bool {
    bytes
        .split(|byte| *byte == b'\n')
        .any(|line| line.starts_with(b"<<<<<<< ") || line.starts_with(b">>>>>>> "))
}

impl MergeConflict {
    pub fn progress(&self, attempts: u32) -> String {
        if attempts == 0 {
            "Resolution requested".into()
        } else {
            format!("Automatic resolution attempt {attempts}/1")
        }
    }

    pub fn paths(&self) -> String {
        self.files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn pause_reason(&self, task: &Task) -> Option<String> {
        if self.files.is_empty() {
            return Some("Conflict details are missing.".into());
        }
        for file in &self.files {
            if Path::new(&file.path)
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            {
                return Some("Conflict path is not project-relative.".into());
            }
            if !task
                .files
                .iter()
                .any(|scope| Path::new(&file.path).starts_with(scope))
            {
                return Some(format!(
                    "{} is outside task {}'s file scope. Request edits to revise ownership.",
                    file.path, task.id
                ));
            }
            if !file.text {
                return Some(format!(
                    "{} needs manual resolution of a binary, rename or deletion conflict.",
                    file.path
                ));
            }
        }
        None
    }
}

impl fmt::Display for MergeConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Task changes conflict in {}. Both versions are saved.",
            self.paths()
        )
    }
}

impl std::error::Error for MergeConflict {}

impl Store {
    pub fn record_conflict(
        &mut self,
        mod_id: i64,
        source: &str,
        worker: i64,
        mut conflict: MergeConflict,
        automatic: bool,
    ) -> Result<Option<bool>> {
        let execution = self
            .execution(mod_id)?
            .ok_or(rusqlite::Error::InvalidQuery)?;
        let Some(run) = execution.tasks.iter().find(|run| {
            run.source == source
                && run.worker == Some(worker)
                && ["checking", "paused"].contains(&run.status.as_str())
        }) else {
            return Ok(None);
        };
        let plan = self
            .planning(mod_id)?
            .and_then(|p| p.plan)
            .ok_or(rusqlite::Error::InvalidQuery)?;
        let task = plan
            .tasks
            .iter()
            .find(|task| task.id == run.task_id)
            .ok_or(rusqlite::Error::InvalidQuery)?;
        for file in &mut conflict.files {
            file.owners = plan
                .tasks
                .iter()
                .filter(|owner| {
                    owner
                        .files
                        .iter()
                        .any(|scope| Path::new(&file.path).starts_with(scope))
                })
                .map(|owner| owner.id.clone())
                .collect();
        }
        let reason = conflict.pause_reason(task).or_else(|| {
            (run.conflict_retries > 0).then(|| "Automatic conflict resolution was already attempted. Inspect the conflict or request edits.".into())
        });
        let recover = automatic && run.status == "checking" && reason.is_none();
        let summary = format!(
            "{conflict} {}",
            reason.as_deref().unwrap_or(if recover {
                "Resolving automatically · attempt 1/1."
            } else {
                "Resolution paused; Ctrl+R retries."
            })
        );
        let tx = self.0.transaction()?;
        tx.execute("UPDATE task_runs SET conflict=?3,summary=?4,status=?5,checks='[]' WHERE mod_id=?1 AND source=?2",
            params![mod_id, source, serde_json::to_string(&conflict).unwrap(), summary, if recover {"pending"} else {"paused"}])?;
        if recover {
            let attempt: i64 = tx.query_row(
                "SELECT attempt FROM task_runs WHERE id=?1",
                [run.id],
                |row| row.get(0),
            )?;
            tx.execute("UPDATE task_runs SET attempt=?2,source=?3,turn_id=NULL,conflict_retries=conflict_retries+1 WHERE id=?1",
                params![run.id, attempt+1, crate::store::task_source(run.id, attempt+1)])?;
            tx.execute(
                "UPDATE workers SET pending=NULL WHERE id=?1 AND pending=?2",
                params![worker, source],
            )?;
        }
        tx.execute(
            "UPDATE executions SET status=?2,checks='[]',fingerprint=NULL WHERE mod_id=?1",
            params![mod_id, if recover { "running" } else { "blocked" }],
        )?;
        tx.commit()?;
        Ok(Some(recover))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn conflict(path: &str, text: bool) -> MergeConflict {
        MergeConflict {
            files: vec![ConflictFile {
                path: path.into(),
                text,
                fingerprint: "saved".into(),
                owners: vec![],
            }],
        }
    }

    #[test]
    fn recovery_retains_ownership_and_budget_across_restart_and_retry() {
        let (data, mut store, id, plan) =
            crate::scheduler::tests::fixture(&["codex", "muse"], false);
        let workers = store
            .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
            .unwrap();
        let runs = store.execution(id).unwrap().unwrap().tasks;
        let run = &runs[0];
        store
            .0
            .execute(
                "UPDATE task_runs SET status='checking' WHERE id=?1",
                [run.id],
            )
            .unwrap();
        assert_eq!(
            store
                .record_conflict(
                    id,
                    &run.source,
                    workers[0].id,
                    conflict("0.txt", true),
                    true
                )
                .unwrap(),
            Some(true)
        );
        assert_eq!(
            store
                .record_conflict(
                    id,
                    &run.source,
                    workers[0].id,
                    conflict("0.txt", true),
                    true
                )
                .unwrap(),
            None
        );
        drop(store);
        let mut store = data.store();
        let execution = store.execution(id).unwrap().unwrap();
        let next = &execution.tasks[0];
        assert_eq!(next.conflict_retries, 1);
        assert_eq!(next.worker, run.worker);
        assert_eq!(next.provider, run.provider);
        assert_eq!(next.status, "pending");
        assert_ne!(next.source, run.source);
        assert_eq!(next.conflict.as_ref().unwrap().files[0].owners, ["task0"]);
        assert_eq!(execution.tasks[1].source, runs[1].source);
        assert!(execution.checks.is_empty() && execution.fingerprint.is_none());
        let prompt = &store.submission(id, &next.source).unwrap().texts[0];
        assert!(prompt.contains("Preserve both tasks' intended behavior"));
        assert!(prompt.contains("Do not expand scope"));
        store
            .0
            .execute(
                "UPDATE task_runs SET status='checking' WHERE id=?1",
                [next.id],
            )
            .unwrap();
        assert_eq!(
            store
                .record_conflict(
                    id,
                    &next.source,
                    workers[0].id,
                    conflict("0.txt", true),
                    true
                )
                .unwrap(),
            Some(false)
        );
        assert!(store.retry_task(id, next.id).unwrap());
        let retried = store.execution(id).unwrap().unwrap().tasks.remove(0);
        store
            .0
            .execute(
                "UPDATE task_runs SET status='checking' WHERE id=?1",
                [next.id],
            )
            .unwrap();
        assert_eq!(
            store
                .record_conflict(
                    id,
                    &retried.source,
                    workers[0].id,
                    conflict("0.txt", true),
                    true
                )
                .unwrap(),
            Some(false)
        );
        let paused = store.execution(id).unwrap().unwrap().tasks.remove(0);
        assert_eq!(paused.conflict_retries, 1);
        assert!(paused.summary.contains("already attempted"));
        assert_eq!(paused.status, "paused");
    }

    #[test]
    fn unsafe_conflicts_and_stops_pause_without_an_automatic_attempt() {
        for (path, text, automatic) in [
            ("1.txt", true, true),
            ("0.txt", false, true),
            ("0.txt", true, false),
            ("../0.txt", true, true),
        ] {
            let (_data, mut store, id, plan) =
                crate::scheduler::tests::fixture(&["codex", "muse"], false);
            let workers = store
                .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
                .unwrap();
            let run = store.execution(id).unwrap().unwrap().tasks.remove(0);
            store
                .0
                .execute(
                    "UPDATE task_runs SET status='checking' WHERE id=?1",
                    [run.id],
                )
                .unwrap();
            assert_eq!(
                store
                    .record_conflict(id, &run.source, workers[1].id, conflict(path, text), true)
                    .unwrap(),
                None
            );
            assert_eq!(
                store
                    .record_conflict(
                        id,
                        &run.source,
                        workers[0].id,
                        conflict(path, text),
                        automatic
                    )
                    .unwrap(),
                Some(false)
            );
            let paused = store.execution(id).unwrap().unwrap().tasks.remove(0);
            assert_eq!(paused.conflict_retries, 0);
            assert_eq!(paused.status, "paused");
            if path == "1.txt" {
                assert_eq!(paused.conflict.unwrap().files[0].owners, ["task1"]);
            }
        }
    }
}
