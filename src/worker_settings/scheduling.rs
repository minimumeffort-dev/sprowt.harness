use super::{ModelChoice, WorkerSettings};
use crate::{
    plan::{Plan, Role},
    store::{Store, WorkerRecord},
};
use rusqlite::{Result, params};

impl Store {
    pub(crate) fn schedule_team(
        &mut self,
        mod_id: i64,
        plan: &Plan,
        busy: &[i64],
        unavailable: &[i64],
        providers: &[&str],
        settings: &WorkerSettings,
    ) -> Result<Vec<WorkerRecord>> {
        let quota_workers = self
            .quota_workers(mod_id)?
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
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
        if matches!(execution.status.as_str(), "review" | "applied" | "planning") {
            return Ok(records
                .into_iter()
                .filter(|r| selected.contains(&r.id))
                .collect());
        }
        for record in &records {
            // Recovery retains ownership even after a pool edit or a quota signal.
            if selected.len() < settings.capacity()
                && !selected.contains(&record.id)
                && !unavailable.contains(&record.id)
                && providers.contains(&record.provider.as_str())
                && (record.pending.is_some()
                    || execution.tasks.iter().any(|r| {
                        r.worker == Some(record.id)
                            && matches!(r.status.as_str(), "sending" | "running" | "checking")
                    }))
            {
                selected.push(record.id);
            }
        }
        for run in execution.ready_tasks(plan) {
            if selected.len() >= settings.capacity() {
                break;
            }
            let task = plan.tasks.iter().find(|t| t.id == run.task_id).unwrap();
            if let Some(owner) = run.worker {
                if quota_workers.contains(&owner) {
                    continue;
                }
                if let Some(record) = records.iter().find(|r| r.id == owner)
                    && !selected.contains(&owner)
                    && !unavailable.contains(&owner)
                    && providers.contains(&record.provider.as_str())
                    && task.accepts(&record.provider)
                {
                    selected.push(owner);
                }
                continue;
            }
            let choices = selected
                .iter()
                .map(|id| self.worker_choice(*id))
                .collect::<Result<Vec<_>>>()?;
            let eligible = |choice: &ModelChoice| -> Result<bool> {
                Ok(providers.contains(&choice.provider.name())
                    && task.accepts(choice.provider.name())
                    && !self.quota_blocked(choice)?)
            };
            let mut choice = None;
            for allocation in &settings.executors {
                if choices
                    .iter()
                    .filter(|c| c.as_ref() == Some(&allocation.choice))
                    .count()
                    < allocation.count
                    && eligible(&allocation.choice)?
                {
                    choice = Some(allocation.choice.clone());
                    break;
                }
            }
            // Only quota or unavailable providers permit substituting a requested slot.
            if choice.is_none() {
                let mut needs_substitute = false;
                for allocation in &settings.executors {
                    needs_substitute |= !providers.contains(&allocation.choice.provider.name())
                        || self.quota_blocked(&allocation.choice)?;
                }
                if needs_substitute {
                    for candidate in settings.choices() {
                        if eligible(candidate)? {
                            choice = Some(candidate.clone());
                            break;
                        }
                    }
                }
            }
            let Some(choice) = choice else {
                continue;
            };
            let mut idle = None;
            for record in &records {
                if !selected.contains(&record.id)
                    && !unavailable.contains(&record.id)
                    && record.pending.is_none()
                    && self.worker_choice(record.id)?.as_ref() == Some(&choice)
                    && !execution.tasks.iter().any(|t| {
                        t.worker == Some(record.id)
                            && !matches!(t.status.as_str(), "done" | "superseded")
                    })
                {
                    idle = Some(record.id);
                    break;
                }
            }
            let worker = if let Some(id) = idle {
                id
            } else {
                let record = self.new_model_worker(mod_id, Role::Executor, &choice)?;
                let id = record.id;
                records.push(record);
                id
            };
            if self.0.execute("UPDATE task_runs SET worker_id=?2,assignment_reason='selected worker team',assignment_order=(SELECT COALESCE(MAX(assignment_order),0)+1 FROM task_runs) WHERE id=?1 AND status='pending' AND worker_id IS NULL",params![run.id,worker])?==1{selected.push(worker);}
        }
        if selected.is_empty()
            && execution.needs_final_checks()
            && let Some(record) = records
                .iter()
                .find(|r| !unavailable.contains(&r.id) && providers.contains(&r.provider.as_str()))
        {
            selected.push(record.id);
        }
        Ok(records
            .into_iter()
            .filter(|r| selected.contains(&r.id))
            .collect())
    }

    /// The client has stopped and all delivery receipts are known before this transaction.
    pub(crate) fn release_quota_worker(
        &mut self,
        worker: i64,
        mod_id: i64,
        role: Role,
    ) -> Result<()> {
        let tx = self.0.transaction()?;
        let pending: Option<String> =
            tx.query_row("SELECT pending FROM workers WHERE id=?1", [worker], |r| {
                r.get(0)
            })?;
        if pending.is_some() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        match role {
            Role::Executor => {
                // Reuse the same task attempt and checkpoint, preserving accepted mail/steering.
                tx.execute("UPDATE task_runs SET status='pending',worker_id=NULL,turn_id=NULL,assignment_reason='quota replacement' WHERE mod_id=?1 AND worker_id=?2 AND status IN ('pending','sending','running','paused','blocked')",params![mod_id,worker])?;
                tx.execute("UPDATE executions SET status='running' WHERE mod_id=?1 AND status NOT IN ('applied','review')",[mod_id])?;
            }
            Role::Planner => {
                tx.execute(
                    "UPDATE plans SET status='pending' WHERE mod_id=?1",
                    [mod_id],
                )?;
            }
            Role::Reviewer => {
                tx.execute(
                    "UPDATE reviews SET status='pending' WHERE mod_id=?1",
                    [mod_id],
                )?;
            }
        }
        tx.execute(
            "DELETE FROM worker_role_assignments WHERE worker_id=?1",
            [worker],
        )?;
        tx.execute("DELETE FROM quota_workers WHERE worker_id=?1", [worker])?;
        tx.commit()
    }
}
