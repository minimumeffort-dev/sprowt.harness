use crate::{
    execution::TaskRun,
    plan::Role,
    store::{CodeMod, Store},
    worker::{Status, Worker},
};
use rusqlite::{OptionalExtension, params};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    Environment,
    Access,
    Connection,
    Conflict,
    Timeout,
    Check,
    Unknown,
}

impl Cause {
    pub fn label(self) -> &'static str {
        match self {
            Self::Environment => "Environment unavailable",
            Self::Access => "Network or authentication unavailable",
            Self::Connection => "Worker connection interrupted",
            Self::Conflict => "Source conflict",
            Self::Timeout => "Check did not finish",
            Self::Check => "Check failed · cause needs diagnosis",
            Self::Unknown => "More evidence needed",
        }
    }
    pub fn advice(self) -> &'static str {
        match self {
            Self::Environment => {
                "Restore the missing runtime or dependencies before retrying the check."
            }
            Self::Access => {
                "Restore access before resuming; code changes cannot repair connectivity."
            }
            Self::Connection => {
                "Reconcile the saved worker report before sending the instruction again."
            }
            Self::Conflict => "Resolve the saved versions together, then run the affected checks.",
            Self::Timeout => {
                "Locate the operation that did not finish before changing its time limit."
            }
            Self::Check => {
                "Reproduce the failure and identify whether code, fixture or environment is responsible."
            }
            Self::Unknown => {
                "Inspect the saved evidence and revise the approach if retrying adds no new information."
            }
        }
    }
}

pub fn cause(run: &TaskRun) -> Cause {
    let failed = run.checks.iter().find(|c| c.failed());
    let text = failed
        .map_or_else(|| run.summary.clone(), |c| c.output.clone())
        .to_lowercase();
    if run.conflict.is_some() {
        Cause::Conflict
    } else if failed.is_some_and(|c| c.missing_runtime.is_some())
        || [
            "cannot find module",
            "modulenotfounderror",
            "executable is unavailable",
            "preflight failed",
            "shared libraries",
        ]
        .iter()
        .any(|s| text.contains(s))
    {
        Cause::Environment
    } else if [
        "blocked-by-allowlist",
        "couldn't connect",
        "authentication failed",
        "could not resolve host",
    ]
    .iter()
    .any(|s| text.contains(s))
    {
        Cause::Access
    } else if [
        "transport",
        "delivery could not be confirmed",
        "no task report",
        "channel is empty",
    ]
    .iter()
    .any(|s| text.contains(s))
    {
        Cause::Connection
    } else if text.contains("timed out") || text.contains("timeout") {
        Cause::Timeout
    } else if failed.is_some() {
        Cause::Check
    } else {
        Cause::Unknown
    }
}

#[derive(Default)]
pub struct History {
    pub phases: Vec<(String, u64)>,
    pub failures: Vec<Failure>,
}
pub struct Failure {
    pub source: String,
    pub task: String,
    pub evidence: String,
    pub fingerprint: String,
}
impl History {
    pub fn repeats(&self, run: &TaskRun) -> usize {
        let evidence = evidence(run);
        self.failures
            .iter()
            .filter(|f| f.task == run.task_id && f.evidence == evidence)
            .count()
    }
    pub fn packet(&self) -> String {
        let mut lines = vec!["Observed worker time · retained across plans and restarts".into()];
        lines.extend(
            self.phases
                .iter()
                .map(|(phase, seconds)| format!("{phase}: {}m {:02}s", seconds / 60, seconds % 60)),
        );
        if !self.failures.is_empty() {
            lines.push(format!(
                "{} saved failed attempts · newest first",
                self.failures.len()
            ));
            lines.extend(self.failures.iter().rev().take(8).map(|f| {
                format!(
                    "{} · attempt {} · source {}\n{}",
                    f.task,
                    f.source,
                    if f.fingerprint.is_empty() {
                        "not verified"
                    } else {
                        &f.fingerprint
                    },
                    f.evidence
                )
            }));
        }
        lines.join("\n")
    }
}

fn evidence(run: &TaskRun) -> String {
    run.checks
        .iter()
        .find(|c| c.failed())
        .map_or_else(
            || run.summary.clone(),
            |c| format!("{}\n{}\n{}", c.check, c.command.join(" "), c.evidence()),
        )
        .chars()
        .take(1600)
        .collect()
}

impl Store {
    pub(crate) fn diagnostics_schema(&self) -> rusqlite::Result<()> {
        self.0.execute_batch("CREATE TABLE IF NOT EXISTS phase_time (
            mod_id INTEGER NOT NULL REFERENCES code_mods(id) ON DELETE CASCADE,
            worker INTEGER NOT NULL, phase TEXT NOT NULL, seconds INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY(mod_id,worker,phase));
            CREATE TABLE IF NOT EXISTS phase_clock (
            mod_id INTEGER NOT NULL REFERENCES code_mods(id) ON DELETE CASCADE,
            worker INTEGER NOT NULL, phase TEXT NOT NULL, seen INTEGER NOT NULL,
            PRIMARY KEY(mod_id,worker));
            CREATE TABLE IF NOT EXISTS run_failures (
            id INTEGER PRIMARY KEY, mod_id INTEGER NOT NULL REFERENCES code_mods(id) ON DELETE CASCADE,
            source TEXT NOT NULL, task TEXT NOT NULL, evidence TEXT NOT NULL, fingerprint TEXT NOT NULL,
            UNIQUE(mod_id,source,evidence));")
    }

    pub fn record_phase(
        &self,
        mod_id: i64,
        worker: i64,
        phase: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        let previous: Option<(String, i64)> = self
            .0
            .query_row(
                "SELECT phase,seen FROM phase_clock WHERE mod_id=?1 AND worker=?2",
                params![mod_id, worker],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if previous
            .as_ref()
            .is_some_and(|(p, t)| p == phase && *t == now)
        {
            return Ok(());
        }
        let tx = self.0.unchecked_transaction()?;
        if let Some((old, seen)) = previous {
            // Gaps are unobserved time, not work. Parallel workers are labelled worker time.
            let elapsed = now.saturating_sub(seen);
            if !old.is_empty() && (1..=2).contains(&elapsed) {
                tx.execute("INSERT INTO phase_time(mod_id,worker,phase,seconds) VALUES(?1,?2,?3,?4) ON CONFLICT(mod_id,worker,phase) DO UPDATE SET seconds=seconds+excluded.seconds", params![mod_id,worker,old,elapsed])?;
            }
        }
        tx.execute("INSERT INTO phase_clock(mod_id,worker,phase,seen) VALUES(?1,?2,?3,?4) ON CONFLICT(mod_id,worker) DO UPDATE SET phase=excluded.phase,seen=excluded.seen", params![mod_id,worker,phase,now])?;
        tx.commit()
    }

    pub fn record_failures(&self, m: &CodeMod) -> rusqlite::Result<()> {
        if let Some(e) = &m.execution {
            for run in e.tasks.iter().filter(|r| r.status == "blocked") {
                let evidence = evidence(run);
                if !evidence.is_empty() {
                    self.0.execute("INSERT OR IGNORE INTO run_failures(mod_id,source,task,evidence,fingerprint) VALUES(?1,?2,?3,?4,?5)", params![m.id,run.source,run.task_id,evidence,e.fingerprint.as_deref().unwrap_or("")])?;
                }
            }
        }
        Ok(())
    }

    pub fn diagnostics(&self, id: i64) -> rusqlite::Result<History> {
        Ok(History {
            phases: self.0.prepare("SELECT phase,SUM(seconds) FROM phase_time WHERE mod_id=?1 GROUP BY phase ORDER BY SUM(seconds) DESC")?.query_map([id], |r| Ok((r.get(0)?,r.get::<_, i64>(1)? as u64)))?.collect::<rusqlite::Result<_>>()?,
            failures: self.0.prepare("SELECT source,task,evidence,fingerprint FROM run_failures WHERE mod_id=?1 ORDER BY id")?.query_map([id], |r| Ok(Failure {source:r.get(0)?,task:r.get(1)?,evidence:r.get(2)?,fingerprint:r.get(3)?}))?.collect::<rusqlite::Result<_>>()?,
        })
    }
}

impl Worker {
    pub fn phase(&self, m: &CodeMod) -> &'static str {
        if !self.busy() {
            return "";
        }
        if self.role == Role::Planner {
            "Planning"
        } else if self.role == Role::Reviewer {
            "Review"
        } else if self.status == Status::Connecting
            || self
                .progress()
                .is_some_and(|p| p.contains("preparing") || p.contains("Preflight"))
        {
            "Preparation"
        } else if self.status == Status::Checking
            || self.progress().is_some_and(|p| p.starts_with("Checking"))
        {
            "Checks"
        } else if m.execution.as_ref().is_some_and(|e| {
            e.tasks.iter().any(|r| {
                Some(r.id) == self.task_run_id()
                    && (r.check_repair
                        || r.repair.is_some()
                        || r.review_feedback.is_some()
                        || !r.verification_feedback.is_empty())
            })
        }) {
            "Repair"
        } else {
            "Implementation"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timing_survives_restart_and_excludes_unobserved_time() {
        let data = crate::store::test_support::TestData::new();
        let mut store = data.store();
        let p = store.load_project(&data.0.join("project")).unwrap();
        let m = store.create_mod(p.id, "Timing").unwrap();
        for (phase, at) in [
            ("Planning", 100),
            ("Planning", 101),
            ("Checks", 102),
            ("Checks", 105),
            ("", 106),
        ] {
            store.record_phase(m.id, 1, phase, at).unwrap();
        }
        drop(store);
        let store = data.store();
        store.record_phase(m.id, 1, "Checks", 1000).unwrap();
        store.record_phase(m.id, 1, "Checks", 1001).unwrap();
        let h = store.diagnostics(m.id).unwrap();
        assert!(h.phases.contains(&("Planning".into(), 2)));
        assert!(h.phases.contains(&("Checks".into(), 2)));
    }
    #[test]
    fn failures_deduplicate_polling_but_keep_attempts_after_replanning() {
        let (_data, store, mut m, _) = crate::review::tests::fixture();
        let run = &mut m.execution.as_mut().unwrap().tasks[0];
        run.status = "blocked".into();
        run.summary = "No task report".into();
        run.checks.clear();
        store.record_failures(&m).unwrap();
        store.record_failures(&m).unwrap();
        m.execution.as_mut().unwrap().tasks[0].source = "next-plan".into();
        store.record_failures(&m).unwrap();
        let h = store.diagnostics(m.id).unwrap();
        assert_eq!(h.repeats(&m.execution.as_ref().unwrap().tasks[0]), 2);
        assert_eq!(
            cause(&m.execution.as_ref().unwrap().tasks[0]),
            Cause::Connection
        );
    }
}
