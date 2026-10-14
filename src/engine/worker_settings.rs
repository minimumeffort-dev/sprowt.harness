use super::*;
use crate::worker_settings::{
    WorkerSettings,
    catalog::{Catalog, Discovery},
};

#[derive(Default)]
pub(crate) struct Models {
    pub catalog: Catalog,
    pending: Option<Discovery>,
}
impl Models {
    #[cfg(test)]
    pub(crate) fn set_fixture(&mut self, catalog: Catalog) {
        let (_, pending) = std::sync::mpsc::channel();
        *self = Self {
            catalog,
            pending: Some(pending),
        };
    }
    fn refresh(&mut self, muse: bool) {
        if self.pending.is_none() {
            self.catalog.loading = true;
            self.catalog.errors.clear();
            self.pending = Some(crate::worker_settings::catalog::discover(muse));
        }
    }
    fn poll(&mut self, store: &Store) -> Result<()> {
        let Some(pending) = &self.pending else {
            return Ok(());
        };
        loop {
            match pending.try_recv() {
                Ok((provider, Ok(result))) => {
                    store.observe_quota(provider.name(), &result.limits)?;
                    self.catalog.models.retain(|m| m.provider != provider);
                    self.catalog.models.extend(result.models);
                }
                Ok((provider, Err(error))) => self
                    .catalog
                    .errors
                    .push(format!("{}: {error}", provider.name())),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.catalog.loading = false;
                    break;
                }
            }
        }
        Ok(())
    }
}
impl Engine {
    pub(crate) fn refresh_models(&mut self) {
        self.models.refresh(self.muse);
    }
    pub(crate) fn settings_for(&self, id: Option<i64>) -> Result<WorkerSettings> {
        Ok(match id {
            Some(id) => self.store.worker_settings(id)?,
            None => self.store.team_draft(self.project_id)?,
        }
        .unwrap_or_default())
    }
    pub(crate) fn save_settings(
        &mut self,
        id: Option<i64>,
        settings: WorkerSettings,
    ) -> std::result::Result<(), String> {
        settings.validate(&self.models.catalog)?;
        self.store
            .save_worker_settings(id, self.project_id, &settings)
            .map_err(|e| e.to_string())
    }
    pub(crate) fn maintain_worker_settings(&mut self) -> Result<()> {
        self.models.poll(&self.store)?;
        let mut exhausted = Vec::new();
        for m in &self.mods {
            for (id, role) in self.store.quota_workers(m.id)? {
                exhausted.push((id, m.id, role));
            }
        }
        for (id, mod_id, role) in exhausted {
            let index = self.mods.iter().position(|m| m.id == mod_id).unwrap();
            let admitted = self.workers.contains_key(&id)
                || self.executing_mods.contains(&mod_id)
                || self.auto_runs.contains(&mod_id)
                || self.auto_reviews.contains(&mod_id);
            if !admitted
                || self.mods[index].closed
                || self.mods[index].user_paused
                || self.git_jobs.contains_key(&mod_id)
                || self
                    .git_states
                    .get(&mod_id)
                    .is_some_and(|g| g.closing || g.removing || g.publishing)
                || self.dialog().is_some_and(|d| !d.is_inspection())
            {
                continue;
            }

            let Some(settings) = self.store.worker_settings(mod_id)? else {
                continue;
            };
            let mut available = false;
            for choice in settings.choices() {
                let compatible = role != Role::Executor
                    || self.mods[index].execution.as_ref().is_none_or(|e| {
                        e.tasks.iter().filter(|t| t.worker == Some(id)).all(|run| {
                            self.mods[index]
                                .planning
                                .as_ref()
                                .and_then(|p| p.plan.as_ref())
                                .and_then(|p| p.tasks.iter().find(|t| t.id == run.task_id))
                                .is_some_and(|t| t.accepts(choice.provider.name()))
                        })
                    });
                if compatible
                    && (self.muse || choice.provider == crate::worker_settings::Provider::Codex)
                    && !self.store.quota_blocked(choice)?
                {
                    available = true;
                    break;
                }
            }
            if !available {
                self.worker_waits.insert(mod_id);
                if let Some(worker) = self.workers.get_mut(&id) {
                    worker.error=Some("No selected worker can continue this assignment. Refresh models or change the worker team.".into());
                }
                continue;
            }
            self.worker_waits.remove(&mod_id);
            self.store.release_quota_worker(id, mod_id, role)?;
            self.workers.remove(&id);
            let index = self.mods.iter().position(|m| m.id == mod_id).unwrap();
            self.mods[index].planning = self.store.planning(mod_id)?;
            self.mods[index].execution = self.store.execution(mod_id)?;
            self.mods[index].agent_review = self.store.review_state(mod_id)?;
            match role {
                Role::Executor => {
                    self.executing_mods.insert(mod_id);
                    self.schedule_execution(index)?;
                }
                _ => {
                    if let Some(record) = self.store.role_worker(mod_id, role, self.muse)? {
                        self.start_worker_record(index, role, record)?;
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker_settings::tests::team;
    #[test]
    fn known_exhaustion_before_allocation_is_visible_and_refresh_recovers() {
        let (data, store, id, plan) = crate::scheduler::tests::fixture(&["auto"], false);
        store.save_worker_settings(Some(id), 1, &team()).unwrap();
        for provider in ["codex", "muse"] {
            store
                .save_quota(
                    provider,
                    &crate::quota::Exhausted {
                        bucket: "subscription".into(),
                        model: None,
                        resets_at_ms: None,
                    },
                )
                .unwrap();
        }
        let project = data.0.join("project");
        let mut engine = Engine::load(project.clone(), store).unwrap();
        engine.muse = true;
        let index = engine.mods.iter().position(|m| m.id == id).unwrap();
        engine
            .command(crate::engine::api::Command::SelectMod { mod_id: id })
            .unwrap();
        engine.start_worker(index, Role::Executor).unwrap();
        assert!(engine.workers.is_empty());
        let snapshot = serde_json::to_value(engine.snapshot()).unwrap();
        assert!(
            snapshot["error"].as_str().unwrap().contains("Open Workers"),
            "{snapshot}"
        );
        engine
            .store
            .observe_quota(
                "codex",
                &[crate::quota::Observation {
                    bucket: "subscription".into(),
                    model: None,
                    exhausted: false,
                    resets_at_ms: None,
                    observed_at_ms: crate::worker_settings::now_ms() + 1,
                }],
            )
            .unwrap();
        let records = engine
            .store
            .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
            .unwrap();
        assert_eq!(records.len(), 1);
        for record in records {
            let (mut worker, _, _) =
                Worker::recording(&project, &engine.mods[index], record).unwrap();
            worker.role = Role::Executor;
            engine.workers.insert(worker.id, worker);
        }
        engine.schedule_execution(index).unwrap();
        assert!(!engine.worker_waits.contains(&id));
        assert!(
            !engine
                .store
                .execution(id)
                .unwrap()
                .unwrap()
                .tasks
                .iter()
                .any(|t| t.worker.is_none())
        );
    }

    #[test]
    fn repeated_planner_retries_retain_active_pin_after_team_edit() {
        for legacy in [false, true] {
            let (_data, mut engine, id, _) = crate::engine::tests::session();
            engine.muse = true;
            if !legacy {
                engine
                    .store
                    .save_worker_settings(Some(id), engine.project_id, &team())
                    .unwrap();
            }
            let index = engine.mods.iter().position(|m| m.id == id).unwrap();
            engine.start_worker(index, Role::Planner).unwrap();
            let worker = *engine.workers.keys().next().unwrap();
            let mut changed = team();
            changed.planner = changed.reviewer.clone();
            engine
                .models
                .set_fixture(crate::worker_settings::tests::catalog());
            engine.save_settings(Some(id), changed).unwrap();
            for _ in 0..3 {
                engine.store.planning_status(id, "paused").unwrap();
                engine.mods[index].planning = engine.store.planning(id).unwrap();
                engine.start_worker(index, Role::Planner).unwrap();
                assert_eq!(engine.workers.len(), 1);
                assert!(engine.workers.contains_key(&worker));
                assert_eq!(
                    engine.workers[&worker].selection.as_ref().unwrap().model,
                    if legacy {
                        "gpt-6-astra"
                    } else {
                        "muse-wonder-1.0"
                    }
                );
            }
        }
    }

    #[test]
    fn quota_recovery_respects_pause_and_closed_and_survives_all_exhausted_restart() {
        let (data, mut engine, id, _) = crate::engine::tests::session();
        engine.muse = true;
        engine
            .store
            .save_worker_settings(Some(id), engine.project_id, &team())
            .unwrap();
        let old = engine
            .store
            .role_worker(id, Role::Planner, true)
            .unwrap()
            .unwrap();
        engine.store.mark_quota_worker(old.id).unwrap();
        for provider in ["muse", "codex"] {
            engine
                .store
                .save_quota(
                    provider,
                    &crate::quota::Exhausted {
                        bucket: "subscription".into(),
                        model: None,
                        resets_at_ms: None,
                    },
                )
                .unwrap();
        }
        let index = engine.mods.iter().position(|m| m.id == id).unwrap();
        engine.mods[index].user_paused = true;
        engine.maintain_worker_settings().unwrap();
        assert!(engine.workers.is_empty());
        engine.mods[index].user_paused = false;
        engine.mods[index].closed = true;
        engine.maintain_worker_settings().unwrap();
        assert!(engine.workers.is_empty());
        let project = engine.project.clone();
        drop(engine);
        let mut engine = Engine::load(project, data.store()).unwrap();
        engine.muse = true;
        engine.maintain_worker_settings().unwrap();
        assert!(engine.workers.is_empty());
        assert_eq!(
            engine.store.quota_workers(id).unwrap(),
            [(old.id, Role::Planner)]
        );
        engine
            .store
            .0
            .execute("DELETE FROM quota_cooldowns WHERE provider='codex'", [])
            .unwrap();
        engine.maintain_worker_settings().unwrap();
        assert!(
            engine.workers.is_empty(),
            "Reopen must not admit new work automatically"
        );
        let index = engine.mods.iter().position(|m| m.id == id).unwrap();
        engine.start_worker(index, Role::Planner).unwrap();
        assert!(engine.store.quota_workers(id).unwrap().is_empty());
        let worker = engine.workers.values().find(|w| w.mod_id == id).unwrap();
        assert_ne!(worker.id, old.id);
        assert_eq!(worker.provider(), "codex");
    }
}
