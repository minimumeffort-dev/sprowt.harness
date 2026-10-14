use super::*;

impl Engine {
    pub fn active_workers(&self) -> impl Iterator<Item = &Worker> {
        self.workers.values().filter(|worker| {
            self.current_mod().is_some_and(|m| m.id == worker.mod_id) && worker.busy()
        })
    }

    pub fn current_worker(&self) -> Option<&Worker> {
        let mod_id = self.current_mod()?.id;
        let workers = || {
            self.workers
                .values()
                .filter(|worker| worker.mod_id == mod_id)
        };
        workers()
            .find(|worker| worker.role == Role::Planner && worker.enabled)
            .or_else(|| workers().find(|worker| worker.role == Role::Reviewer && worker.busy()))
            .or_else(|| workers().find(|worker| worker.role == Role::Executor && worker.busy()))
            .or_else(|| {
                workers().find(|worker| worker.role == Role::Executor && worker.error.is_some())
            })
            .or_else(|| workers().find(|worker| worker.role == Role::Executor))
            .or_else(|| workers().next())
    }

    pub fn running_workers(&self) -> Vec<i64> {
        self.current_mod().map_or_else(Vec::new, |m| {
            self.workers
                .values()
                .filter(|w| {
                    w.mod_id == m.id && w.role == Role::Executor && w.status == Status::Running
                })
                .map(|w| w.id)
                .collect()
        })
    }

    pub fn worker_activity(&self) -> Vec<String> {
        self.current_mod().map_or_else(Vec::new, |m| {
            self.workers
                .values()
                .filter(|w| w.mod_id == m.id && w.role == Role::Executor && w.busy())
                .map(|w| w.activity(m))
                .collect()
        })
    }

    pub fn worker_error(&self) -> Option<&str> {
        self.current_mod()
            .and_then(|m| {
                self.workers
                    .values()
                    .filter(|w| w.mod_id == m.id)
                    .find_map(|w| w.error.as_deref())
                    .or_else(|| self.git_errors.current(m.id))
                    .or_else(|| self.worker_waits.contains(&m.id).then_some("No selected worker can continue. Open Workers to refresh models or change the team."))
            })
            .or(self.notice.as_deref())
            .or(self.poll_error.as_deref())
    }

    pub(crate) fn toggle_worker(&mut self) -> Result<()> {
        let Some(active) = self.active else {
            return Ok(());
        };
        let id = self.mods[active].id;
        if self
            .git_jobs
            .get(&id)
            .is_some_and(|job| job.label != "checking target branch")
        {
            return Ok(());
        }
        let stopping = self
            .workers
            .values()
            .any(|w| w.mod_id == id && (w.enabled || w.busy()));
        self.set_paused(active, stopping)?;
        if stopping {
            self.auto_reviews.remove(&id);
            self.auto_plans.remove(&id);
            self.auto_runs.remove(&id);
        }
        if let Some(worker) = self
            .workers
            .values_mut()
            .find(|w| w.mod_id == id && w.role == Role::Reviewer && (w.enabled || w.busy()))
        {
            worker.toggle();
            if let Some(review) = &self.mods[active].agent_review {
                self.store.review_status(id, &review.source, "paused")?;
                self.mods[active].agent_review = self.store.review_state(id)?;
            }
            return Ok(());
        }
        if self.update_errors.remove(&id)
            && self.mods[active].git_root.as_ref().is_none_or(|root| {
                crate::mod_sync::load(root)
                    .ok()
                    .flatten()
                    .is_none_or(|u| !u.installed)
            })
        {
            self.check_target(active, false, false);
            return Ok(());
        }
        if self.mods[active].git_root.is_some() {
            let root = self.mods[active].git_root.clone().unwrap();
            if let Some(saved) = git_mod::setup_pending(&root) {
                let context = Context::harness(&self.project, &self.mods[active]);
                self.git_jobs.insert(
                    id,
                    Job::start(
                        if saved {
                            "adopting saved codemod"
                        } else {
                            "setting up Git"
                        },
                        move |cancelled| {
                            Dispatcher::new(&context, None, cancelled).execute(
                                if saved {
                                    Request::AdoptSnapshot
                                } else {
                                    Request::InitializeProject
                                },
                                |_| {},
                            )?;
                            Ok(if saved {
                                git_mod::Result::Adopted
                            } else {
                                git_mod::Result::Prepared
                            })
                        },
                    ),
                );
                return Ok(());
            }
            if let Some(request) = git_mod::repository_pending(&root) {
                self.start_repository(request);
                return Ok(());
            }
            match self.git_states.get(&id) {
                Some(state) if state.removing => return self.delete_mod(active),
                Some(state) if state.continuing => {
                    self.start_continuation(active);
                    return Ok(());
                }
                Some(state) if state.closing && !self.mods[active].closed => {
                    return self.finish_mod(active, state.discarding, false);
                }
                Some(_) if self.mods[active].closed => {
                    self.reopen_mod(active);
                    return Ok(());
                }
                Some(_)
                    if self.mod_published(&self.mods[active])
                        && self.mods[active].queue.is_empty() =>
                {
                    return Ok(());
                }
                state if state.is_none_or(|state| state.phase == "preparing") => {
                    let context = Context::harness(&self.project, &self.mods[active]);
                    self.git_jobs.insert(
                        id,
                        Job::start("creating worktree", move |cancelled| {
                            Dispatcher::new(&context, None, cancelled)
                                .execute(Request::CreateWorktree, |_| {})?;
                            Ok(git_mod::Result::Prepared)
                        }),
                    );
                    return Ok(());
                }
                Some(state)
                    if state.publishing
                        || !["ready", "published", "cleaned", "closed", "pruned"]
                            .contains(&state.phase.as_str()) =>
                {
                    self.start_publication(active);
                    return Ok(());
                }
                Some(state) if state.phase == "cleaned" => return Ok(()),
                _ => {}
            }
        }
        if self.mods[active].closed {
            self.reopen_mod(active);
            return Ok(());
        }
        if self.retry_verified_update(active)? {
            return Ok(());
        }
        if self.mods[active].execution.is_none()
            && self.mods[active].git_root.as_ref().is_some_and(|root| {
                !root.join("work").exists() && !root.join("snapshot-ready").exists()
            })
            && self.mods[active]
                .planning
                .as_ref()
                .is_some_and(|plan| matches!(plan.status.as_str(), "failed" | "paused"))
            && !self.execution_busy()
        {
            self.workers.retain(|_, worker| worker.mod_id != id);
            let context = Context::harness(&self.project, &self.mods[active]);
            self.git_jobs.insert(
                id,
                Job::start("refreshing starting source", move |cancelled| {
                    Dispatcher::new(&context, None, cancelled)
                        .execute(Request::RefreshWorktree, |_| {})?;
                    Ok(git_mod::Result::Refreshed)
                }),
            );
            self.notice = None;
            return Ok(());
        }
        let role = if self.mods[active]
            .planning
            .as_ref()
            .is_some_and(|p| p.status != "ready")
        {
            Role::Planner
        } else {
            Role::Executor
        };
        let mod_id = self.mods[active].id;
        if role == Role::Executor
            && self.mods[active]
                .planning
                .as_ref()
                .is_some_and(|p| p.plan.is_some())
        {
            if self.current_worker().is_some_and(|worker| {
                worker.status == Status::Stopping
                    || (!worker.enabled && worker.status == Status::Checking)
            }) {
                self.notice =
                    Some("The current command is stopping; retry when it finishes.".into());
                return Ok(());
            }
            if self.mods[active].execution.is_none()
                || self.mods[active]
                    .execution
                    .as_ref()
                    .is_some_and(|e| e.status == "planning")
            {
                return self.start_execution(active);
            }
            if self.mods[active].execution.as_ref().is_some_and(|e| {
                matches!(e.status.as_str(), "review" | "applied" | "blocked")
                    && !e.tasks.iter().any(|t| t.status == "waiting")
            }) && !self.mods[active].queue.is_empty()
                && !self.execution_busy()
            {
                self.start_continuation(active);
                return Ok(());
            }
            if self.mods[active]
                .execution
                .as_ref()
                .is_some_and(|execution| {
                    execution.backend == "local" && execution.status != "applied"
                })
            {
                self.workers
                    .retain(|_, worker| worker.mod_id != mod_id || worker.role != Role::Executor);
                self.store.move_to_vm(mod_id)?;
                self.mods[active].execution = self.store.execution(mod_id)?;
            }
            if self.current_worker().is_none_or(|worker| !worker.enabled) {
                self.store.retry_tasks(mod_id)?;
                self.mods[active].execution = self.store.execution(mod_id)?;
            }
        }
        if role == Role::Executor {
            let running = self
                .workers
                .values()
                .any(|w| w.mod_id == mod_id && (w.enabled || w.busy()));
            if running {
                self.executing_mods.remove(&mod_id);
                self.store.pause_repairs(mod_id)?;
                self.mods[active].execution = self.store.execution(mod_id)?;
                for worker in self
                    .workers
                    .values_mut()
                    .filter(|w| w.mod_id == mod_id && w.role == role && w.enabled)
                {
                    worker.toggle();
                }
                return Ok(());
            }
            self.workers
                .retain(|_, w| w.mod_id != mod_id || w.role != role);
            self.store.retry_tasks(mod_id)?;
            self.mods[active].execution = self.store.execution(mod_id)?;
            return self.start_worker(active, role);
        }
        if let Some(worker) = self
            .workers
            .values_mut()
            .find(|w| w.mod_id == mod_id && w.role == role)
            && worker.status != Status::Failed
        {
            if role == Role::Planner && worker.status == Status::Ready && !worker.enabled {
                self.store.retry_plan(mod_id)?;
                self.mods[active].planning = self.store.planning(mod_id)?;
            }
            worker.toggle();
            if role == Role::Planner && worker.enabled {
                self.auto_runs.insert(mod_id);
            }
            return Ok(());
        }
        self.start_worker(active, role)
    }

    pub(crate) fn retry_verified_update(&mut self, index: usize) -> Result<bool> {
        let code_mod = &self.mods[index];
        let Some(root) = code_mod.git_root.as_ref() else {
            return Ok(false);
        };
        let Some(execution) = code_mod.execution.as_ref() else {
            return Ok(false);
        };
        let Some(last) = execution.tasks.last() else {
            return Ok(false);
        };
        if self.execution_busy()
            || !matches!(execution.status.as_str(), "review" | "blocked")
            || !matches!(last.status.as_str(), "done" | "blocked")
            || execution.tasks[..execution.tasks.len() - 1]
                .iter()
                .any(|task| task.status != "done")
            || execution.checks.is_empty()
            || execution
                .checks
                .iter()
                .any(|check| check.exit_code != Some(0))
            || execution.fingerprint.is_none()
        {
            return Ok(false);
        }
        let checked = (|| -> io::Result<bool> {
            let Some(update) = crate::mod_sync::load(root)? else {
                return Ok(false);
            };
            Ok(update.prepared
                && update.installed
                && update.imported
                && execution.fingerprint.as_ref()
                    == Some(&workspace::fingerprint(&workspace::source_state(
                        &root.join("work"),
                    )?)?))
        })()
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        if !checked {
            return Ok(false);
        }
        // Older saves marked the last passed task blocked; retain its verified result.
        let id = code_mod.id;
        self.store.task_status(id, &last.source, "done", None)?;
        self.store.execution_status(id, "review")?;
        self.mods[index].execution = self.store.execution(id)?;
        self.update_errors.remove(&id);
        self.notice = None;
        self.maintain_reviews()?;
        self.maintain_updates()?;
        Ok(true)
    }

    pub(crate) fn start_execution(&mut self, index: usize) -> Result<()> {
        let id = self.mods[index].id;
        if self.mods[index]
            .execution
            .as_ref()
            .is_some_and(|e| e.status == "planning")
        {
            let root = self.mods[index]
                .execution
                .as_ref()
                .unwrap()
                .workspace
                .clone();
            let plan = self.mods[index]
                .planning
                .as_ref()
                .unwrap()
                .plan
                .as_ref()
                .unwrap();
            self.store.create_execution(id, &root, plan)?;
            self.mods[index].execution = self.store.execution(id)?;
            return self.start_worker(index, Role::Executor);
        }
        if self.mods[index].execution.is_none() {
            if let Some(root) = self.mods[index].git_root.clone() {
                let project = self.source_project(index);
                let plan_source = self.mods[index].planning.as_ref().unwrap().source.clone();
                self.git_jobs.insert(
                    id,
                    Job::start("preparing source", move |_| {
                        preparation::snapshot(&root, &project, &plan_source)?;
                        Ok(git_mod::Result::Snapshot)
                    }),
                );
                return Ok(());
            }
            if let Err(error) = self.prepare_execution(index) {
                self.notice = Some(error.to_string());
                return Ok(());
            }
        }
        self.start_worker(index, Role::Executor)
    }

    pub(crate) fn ready_plans(&self) -> Vec<usize> {
        self.mods
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                !m.closed
                    && !m.user_paused
                    && self.auto_runs.contains(&m.id)
                    && !self.git_jobs.contains_key(&m.id)
                    && m.planning.as_ref().is_some_and(|p| p.status == "ready")
                    && m.execution.as_ref().is_none_or(|e| e.status == "planning")
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub(crate) fn start_worker(&mut self, index: usize, role: Role) -> Result<()> {
        if self
            .store
            .quota_workers(self.mods[index].id)?
            .iter()
            .any(|(_, r)| *r == role)
        {
            match role {
                Role::Executor => {
                    self.executing_mods.insert(self.mods[index].id);
                }
                Role::Planner => {
                    self.auto_runs.insert(self.mods[index].id);
                }
                Role::Reviewer => {
                    self.auto_reviews.insert(self.mods[index].id);
                }
            }
            return self.maintain_worker_settings();
        }

        let plan = self.mods[index]
            .planning
            .as_ref()
            .and_then(|p| p.plan.as_ref());
        let uses_muse = role == Role::Executor
            && (plan.is_some_and(|p| p.tasks.iter().any(|t| t.worker == "muse"))
                || self.mods[index].execution.as_ref().is_some_and(|e| {
                    e.tasks
                        .iter()
                        .any(|t| t.provider.as_deref() == Some("muse"))
                }));
        if uses_muse && !self.muse {
            self.notice = Some(format!(
                "This plan needs Muse {}. Install it, run muse login and restart the harness.",
                crate::muse::VERSION
            ));
            return Ok(());
        }
        let mod_id = self.mods[index].id;
        if role == Role::Executor {
            self.executing_mods.insert(mod_id);
            return self.schedule_execution(index);
        }
        let Some(record) = self.store.role_worker(mod_id, role, self.muse)? else {
            self.notice=Some("All selected worker providers are out of quota. Refresh models or choose another team.".into());
            return Ok(());
        };
        self.start_worker_record(index, role, record)
    }

    pub(crate) fn schedule_execution(&mut self, index: usize) -> Result<()> {
        let mod_id = self.mods[index].id;
        for record in self.store.executors(mod_id)? {
            if !self.workers.get(&record.id).is_some_and(Worker::busy) {
                self.store.resume_mail(mod_id, record.id)?;
            }
        }
        let busy = self
            .workers
            .values()
            .filter(|w| w.role == Role::Executor && w.busy())
            .map(|w| w.id)
            .collect::<Vec<_>>();
        let unavailable = self
            .workers
            .values()
            .filter(|w| w.mod_id == mod_id && w.status == Status::Failed)
            .map(|w| w.id)
            .chain(
                self.network_requests
                    .iter()
                    .filter(|r| r.mod_id == mod_id && r.status == "approved")
                    .map(|r| r.worker),
            )
            .collect::<Vec<_>>();
        if let (Some(execution), Some(plan)) = (
            &self.mods[index].execution,
            self.mods[index]
                .planning
                .as_ref()
                .and_then(|p| p.plan.as_ref()),
        ) {
            let eligible = execution
                .ready_tasks(plan)
                .filter(|run| run.worker.is_none_or(|id| !unavailable.contains(&id)))
                .map(|run| run.source.clone())
                .collect::<BTreeSet<_>>();
            self.slot_waits
                .retain(|(id, source), _| *id != mod_id || eligible.contains(source));
            for source in eligible {
                self.slot_waits
                    .entry((mod_id, source))
                    .or_insert_with(Instant::now);
            }
        }
        let plan = self.mods[index]
            .planning
            .as_ref()
            .unwrap()
            .plan
            .as_ref()
            .unwrap();
        let records = self.store.schedule_workers(
            mod_id,
            plan,
            &busy,
            &unavailable,
            if self.muse {
                &["codex", "muse"]
            } else {
                &["codex"]
            },
        )?;
        let waiting = records.is_empty()
            && self.store.worker_settings(mod_id)?.is_some()
            && self.mods[index]
                .execution
                .as_ref()
                .is_some_and(|e| e.ready_tasks(plan).next().is_some());
        if waiting {
            self.worker_waits.insert(mod_id);
        } else {
            self.worker_waits.remove(&mod_id);
        }
        self.mods[index].execution = self.store.execution(mod_id)?;
        for worker in self
            .workers
            .values_mut()
            .filter(|w| w.mod_id == mod_id && w.role == Role::Executor)
        {
            if !worker.busy() && !records.iter().any(|r| r.id == worker.id) {
                worker.enabled = false;
            }
        }
        for record in records {
            if let Some(worker) = self.workers.get_mut(&record.id) {
                if !worker.enabled && matches!(worker.status, Status::Ready | Status::Complete) {
                    worker.toggle();
                }
            } else {
                self.start_worker_record(index, Role::Executor, record)?;
            }
        }
        if let (Some(execution), Some(plan)) = (
            &self.mods[index].execution,
            self.mods[index]
                .planning
                .as_ref()
                .and_then(|p| p.plan.as_ref()),
        ) {
            for run in execution.ready_tasks(plan) {
                if let Some(worker) = run.worker.and_then(|id| self.workers.get_mut(&id))
                    && let Some(started) = self.slot_waits.get(&(mod_id, run.source.clone()))
                {
                    worker.eligible(
                        &run.source,
                        *started,
                        execution.fingerprint.as_deref().unwrap_or(""),
                    );
                }
            }
        }
        if self.mods[index]
            .execution
            .as_ref()
            .is_some_and(|e| matches!(e.status.as_str(), "review" | "applied"))
        {
            self.executing_mods.remove(&mod_id);
            self.auto_reviews.insert(mod_id);
        }
        if self.mods[index]
            .execution
            .as_ref()
            .is_some_and(|e| e.complete() && e.status == "blocked")
        {
            self.executing_mods.remove(&mod_id);
        }
        Ok(())
    }

    pub(crate) fn start_worker_record(
        &mut self,
        index: usize,
        role: Role,
        record: WorkerRecord,
    ) -> Result<()> {
        let project = self.source_project(index);
        let mod_id = self.mods[index].id;
        let saved = self.mods[index]
            .execution
            .as_ref()
            .map(|e| e.workspace.as_path())
            .or(self.mods[index].git_root.as_deref());
        match self.preparations.root(mod_id, &self.store, saved) {
            Ok(_) if role == Role::Planner && self.mods[index].execution.is_none() => {
                self.preparations.start(mod_id)
            }
            Ok(_) => {}
            Err(error) => self
                .store
                .record_preparation_failure(mod_id, &error.to_string())?,
        }
        self.workers.remove(&record.id);
        let chosen = self.store.worker_choice(record.id)?;
        let selection = if role == Role::Planner {
            if record.pending.is_none()
                && self.mods[index]
                    .planning
                    .as_ref()
                    .is_some_and(|p| p.status == "failed" || p.status == "paused")
            {
                self.store.retry_plan(mod_id)?;
                self.mods[index].planning = self.store.planning(mod_id)?;
            }
            Some(
                chosen
                    .as_ref()
                    .map_or_else(crate::router::Selection::planner, |c| c.selection()),
            )
        } else {
            chosen.as_ref().map(|c| c.selection())
        };
        self.store.bind_role_worker(record.id, mod_id, role)?;
        match Worker::start_with_muse(
            &project,
            &self.mods[index],
            record,
            role,
            selection,
            self.muse,
        ) {
            Ok(worker) => {
                self.workers.insert(worker.id, worker);
                if role == Role::Planner {
                    self.auto_runs.insert(mod_id);
                }
                self.notice = None;
            }
            Err(error) => self.notice = Some(error.to_string()),
        }
        Ok(())
    }

    pub(crate) fn prepare_execution(&mut self, index: usize) -> io::Result<()> {
        let code_mod = &self.mods[index];
        let root =
            self.preparations
                .root(code_mod.id, &self.store, code_mod.git_root.as_deref())?;
        preparation::snapshot(
            &root,
            &self.project,
            &code_mod.planning.as_ref().unwrap().source,
        )?;
        let plan = code_mod.planning.as_ref().unwrap().plan.as_ref().unwrap();
        if let Err(error) = self.store.create_execution(code_mod.id, &root, plan) {
            self.preparations
                .cancel(code_mod.id, &self.store)
                .map_err(io::Error::other)?;
            return Err(io::Error::other(error));
        }
        self.workers
            .retain(|_, worker| worker.mod_id != code_mod.id || worker.role != Role::Executor);
        self.mods[index].execution = self
            .store
            .execution(code_mod.id)
            .map_err(io::Error::other)?;
        Ok(())
    }
}
