use super::*;

impl Engine {
    pub(crate) fn check_target(&mut self, index: usize, publish: bool, explicit: bool) {
        let code_mod = &self.mods[index];
        if code_mod.closed
            || code_mod.git_root.is_none()
            || self.git_jobs.contains_key(&code_mod.id)
        {
            return;
        }
        let context = Context::harness(&self.project, code_mod);
        self.update_errors.remove(&code_mod.id);
        self.target_checks.insert(code_mod.id, Instant::now());
        self.git_jobs.insert(
            code_mod.id,
            Job::start("checking target branch", move |cancelled| {
                let target = match Dispatcher::new(&context, None, cancelled)
                    .execute(Request::CheckTarget, |_| {})?
                {
                    crate::tools::Output::Target(target) => target,
                    _ => unreachable!(),
                };
                Ok(git_mod::Result::TargetChecked {
                    target,
                    publish,
                    explicit,
                })
            }),
        );
    }

    pub(crate) fn start_update(&mut self, index: usize, target: crate::mod_sync::Target) {
        let id = self.mods[index].id;
        if self.git_jobs.contains_key(&id)
            || self
                .workers
                .values()
                .any(|w| w.mod_id == id && (w.enabled || w.busy()))
        {
            return;
        }
        let Some(plan) = self.mods[index]
            .planning
            .as_ref()
            .and_then(|p| p.plan.clone())
        else {
            return;
        };
        let context = Context::harness(&self.project, &self.mods[index]);
        let workers = self
            .workers
            .keys()
            .filter(|key| self.workers[key].mod_id == id)
            .copied()
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|key| self.workers.remove(&key))
            .collect::<Vec<_>>();
        self.executing_mods.remove(&id);
        self.git_jobs.insert(
            id,
            Job::start("updating from target branch", move |cancelled| {
                drop(workers);
                Dispatcher::new(&context, None, cancelled).execute(
                    Request::UpdateTarget {
                        target,
                        plan: Box::new(plan),
                    },
                    |_| {},
                )?;
                Ok(git_mod::Result::Updated)
            }),
        );
        if self.current_mod().is_some_and(|m| m.id == id) {
            self.review = None;
            self.publish_after_review = false;
            self.view = View::Chat;
        }
    }

    pub(crate) fn maintain_updates(&mut self) -> Result<()> {
        for index in 0..self.mods.len() {
            let code_mod = &self.mods[index];
            let id = code_mod.id;
            let Some(root) = code_mod.git_root.as_ref() else {
                continue;
            };
            if code_mod.closed || code_mod.execution.is_none() || self.git_jobs.contains_key(&id) {
                continue;
            }
            if self.update_errors.contains(&id) {
                continue;
            }
            if self.current_mod().is_some_and(|m| m.id == id)
                && !matches!(self.view, View::Chat | View::Mods(_))
            {
                continue;
            }
            let busy = self
                .workers
                .values()
                .any(|w| w.mod_id == id && (w.enabled || w.busy()));
            if let Some(update) = crate::mod_sync::load(root)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
            {
                if !busy && (!update.prepared || !update.installed) {
                    self.start_update(
                        index,
                        crate::mod_sync::Target {
                            branch: update.branch,
                            head: update.target,
                            pr_state: None,
                            pr_feedback: Vec::new(),
                            pr_feedback_error: None,
                        },
                    );
                } else if !busy
                    && code_mod
                        .execution
                        .as_ref()
                        .is_some_and(|e| e.complete() && e.status == "review")
                {
                    let context = Context::harness(&self.project, code_mod);
                    self.git_jobs.insert(
                        id,
                        Job::start("saving verified merge", move |cancelled| {
                            Dispatcher::new(&context, None, cancelled)
                                .execute(Request::FinishUpdate, |_| {})?;
                            Ok(git_mod::Result::UpdateFinished)
                        }),
                    );
                }
                continue;
            }
            if self
                .targets
                .get(&id)
                .is_some_and(|t| t.pr_state.as_deref().is_some_and(|s| s != "OPEN"))
            {
                continue;
            }
            if !busy
                && !code_mod
                    .agent_review
                    .as_ref()
                    .is_some_and(|r| r.holds_updates(code_mod))
                && code_mod.execution.as_ref().is_some_and(|e| {
                    e.complete() && matches!(e.status.as_str(), "review" | "applied")
                })
                && let Some(target) = self.targets.get(&id)
                && self
                    .git_states
                    .get(&id)
                    .is_some_and(|s| s.base != target.head)
            {
                self.start_update(index, target.clone());
            } else if self
                .target_checks
                .get(&id)
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(30))
                && self.git_states.get(&id).is_some_and(|s| {
                    ["ready", "published"].contains(&s.phase.as_str())
                        && !s.publishing
                        && !s.closing
                })
            {
                self.check_target(index, false, false);
            }
        }
        Ok(())
    }

    pub(crate) fn maintain_project(&mut self) {
        if self.project_job.is_some()
            || !self.git_jobs.is_empty()
            || self
                .project_check
                .is_some_and(|checked| checked.elapsed() < Duration::from_secs(30))
        {
            return;
        }
        self.project_check = Some(Instant::now());
        if !git_mod::has_commit(&self.project) {
            return;
        }
        let context = Context::project(&self.project, self.project_sync_root.clone());
        self.project_job = Some(Job::start("syncing project branch", move |cancelled| {
            Dispatcher::new(&context, None, cancelled).execute(Request::SyncProject, |_| {})?;
            Ok(git_mod::Result::ProjectSynced)
        }));
    }

    pub(crate) fn source_project(&self, index: usize) -> PathBuf {
        self.mods[index]
            .git_root
            .as_ref()
            .map(|root| {
                if root.join("work").exists() {
                    root.join("work")
                } else {
                    git_mod::checkout(root)
                }
            })
            .or_else(|| {
                self.mods[index]
                    .execution
                    .as_ref()
                    .map(|e| e.workspace.join("work"))
            })
            .unwrap_or_else(|| self.project.clone())
    }

    pub(crate) fn poll_git_jobs(&mut self) -> Result<()> {
        if let Some(result) = self.project_job.as_ref().and_then(Job::poll) {
            self.project_job.take();
            self.project_error = match result {
                Ok(git_mod::Result::ProjectSynced) => None,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.project_error.take()
                }
                Err(error) => Some(error.to_string()),
                _ => unreachable!(),
            };
        }
        let finished = self
            .git_jobs
            .iter()
            .filter_map(|(id, job)| job.poll().map(|result| (*id, result)))
            .collect::<Vec<_>>();
        for (id, result) in finished {
            let label = self.git_jobs.remove(&id).unwrap().label;
            let Some(index) = self.mods.iter().position(|m| m.id == id) else {
                continue;
            };
            let root = self.mods[index].git_root.clone().unwrap();
            if let Ok(state) = git_mod::load(&root) {
                if state.published()
                    && self.mods[index]
                        .execution
                        .as_ref()
                        .is_some_and(|e| e.status == "applied")
                {
                    self.store.execution_status(id, "review")?;
                    self.mods[index].execution = self.store.execution(id)?;
                }
                if let Some(url) = &state.pr {
                    let item_id = format!("pr:{id}");
                    if !self.mods[index]
                        .messages
                        .iter()
                        .any(|message| message.item_id.as_deref() == Some(&item_id))
                    {
                        let message = crate::store::Message {
                            source: None,
                            task: None,
                            item_id: Some(item_id),
                            role: "harness".into(),
                            body: format!("PR ready · {url}"),
                            model: None,
                            effort: None,
                        };
                        self.store.save_message(id, &message)?;
                        self.mods[index].messages.push(message);
                    }
                }
                self.git_states.insert(id, state);
            }
            if result.is_ok() {
                self.git_errors.clear(id, label);
                if matches!(
                    label,
                    "updating from target branch" | "saving verified merge"
                ) && !self.git_errors.contains(id, "updating from target branch")
                    && !self.git_errors.contains(id, "saving verified merge")
                {
                    self.update_errors.remove(&id);
                }
            }
            match result {
                Ok(git_mod::Result::ProjectSynced) => unreachable!(),
                Ok(git_mod::Result::PublicationDraft(draft)) => {
                    if self.current_mod().is_some_and(|m| m.id == id) && self.can_publish() {
                        self.publication = Some(draft);

                        self.view = View::Publish;
                    }
                }
                Ok(git_mod::Result::Adopted) => {
                    if self.mods[index]
                        .execution
                        .as_ref()
                        .is_some_and(|e| e.status == "applied")
                    {
                        self.store.execution_status(id, "review")?;
                        self.mods[index].execution = self.store.execution(id)?;
                    }
                    if root.join("close-request").exists() {
                        git_mod::mark_closing(&root, false, false)
                            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                        std::fs::remove_file(root.join("close-request")).ok();
                        self.finish_mod(index, false, false)?;
                    } else if self.current_mod().is_some_and(|m| m.id == id) {
                        self.open_publication()?;
                    }
                }
                Ok(git_mod::Result::RepositoryConnected) => {
                    if self.current_mod().is_some_and(|m| m.id == id) {
                        if self.git_states.get(&id).is_some_and(|state| state.closing) {
                            self.finish_mod(index, false, false)?;
                        } else if self.can_publish() {
                            self.open_publication()?;
                        }
                    }
                }
                Ok(git_mod::Result::Prepared) => {
                    if self.mods[index]
                        .planning
                        .as_ref()
                        .is_some_and(|p| p.status == "pending")
                    {
                        self.auto_plans.insert(id);
                    }
                }
                Ok(git_mod::Result::Refreshed) => {
                    self.store.restart_plan(id)?;
                    self.mods[index].planning = self.store.planning(id)?;
                    self.auto_plans.insert(id);
                }
                Ok(git_mod::Result::TargetChecked {
                    target,
                    publish,
                    explicit,
                }) => {
                    self.target_errors.remove(&id);
                    if explicit
                        && let Some(target) = &target
                        && self
                            .git_states
                            .get(&id)
                            .is_some_and(|s| s.base != target.head)
                        && self.mods[index]
                            .execution
                            .as_ref()
                            .is_some_and(|e| e.complete() && e.status == "review")
                        && target.pr_state.as_deref().is_none_or(|s| s == "OPEN")
                    {
                        self.start_update(index, target.clone());
                    }
                    if let Some(target) = target {
                        self.targets.insert(id, target);
                    } else {
                        self.targets.remove(&id);
                    }
                    if publish
                        && self.current_mod().is_some_and(|m| m.id == id)
                        && self.can_publish()
                    {
                        self.confirm_publication()?;
                    }
                }
                Ok(git_mod::Result::Updated) => {
                    let mut update = crate::mod_sync::load(&root).unwrap().unwrap();
                    self.store.install_update(id, &root, &update)?;
                    update.installed = true;
                    crate::mod_sync::save(&root, &update)
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                    let state = self.store.load_project(&self.project)?;
                    self.mods[index] = state.mods.into_iter().find(|m| m.id == id).unwrap();
                    self.start_worker(index, Role::Executor)?;
                }
                Ok(git_mod::Result::UpdateFinished) => {
                    self.target_checks.remove(&id);
                }
                Ok(git_mod::Result::Snapshot) => {
                    let plan = self.mods[index]
                        .planning
                        .as_ref()
                        .unwrap()
                        .plan
                        .as_ref()
                        .unwrap();
                    if let Err(error) = self.store.create_execution(id, &root, plan) {
                        self.preparations.cancel(id, &self.store)?;
                        return Err(error);
                    }
                    self.mods[index].execution = self.store.execution(id)?;
                    self.start_worker(index, Role::Executor)?;
                }
                Ok(git_mod::Result::Reviewed(review)) => {
                    if self.current_mod().is_some_and(|m| m.id == id) {
                        if self.mods[index].execution.as_ref().is_some_and(|e| {
                            e.status == "review"
                                && e.fingerprint.as_deref() != Some(&review.fingerprint)
                        }) {
                            self.store.execution_status(id, "blocked")?;
                            self.mods[index].execution = self.store.execution(id)?;
                            self.notice = Some(
                                "Working files changed since verification. Ctrl+R rechecks.".into(),
                            );
                        }
                        self.visit_selected();
                        self.review = Some(review);
                        self.view = View::Review(0);
                        self.restore_input();
                        if std::mem::take(&mut self.publish_after_review) && self.can_publish() {
                            self.open_publication()?;
                        }
                    }
                }
                Ok(git_mod::Result::Pruned) => {}
                Ok(git_mod::Result::Published) => {}
                Ok(git_mod::Result::Continued) => {
                    self.finish_edit(index)?;
                    git_mod::finish_continuation(&root)
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                    self.git_states.insert(id, git_mod::load(&root).unwrap());
                }
                Ok(git_mod::Result::Reopened) => {
                    self.store.reopen_mod(id)?;
                    self.mods[index].closed = false;
                    if self.current_mod().is_some_and(|m| m.id == id) {
                        self.restore_input();
                    }
                }
                Ok(git_mod::Result::Closed) => {
                    self.target_errors.remove(&id);
                    let selected = self.store.close_mod(self.project_id, id)?;
                    self.mods[index].closed = true;
                    self.mods[index].planning = self.store.planning(id)?;
                    self.mods[index].execution = self.store.execution(id)?;
                    if self.current_mod().is_some_and(|m| m.id == id) {
                        self.active = self.mods.iter().position(|m| Some(m.id) == selected);
                        self.visit_selected();
                        self.view = if self.active.is_some() {
                            View::Chat
                        } else {
                            View::NewMod
                        };
                        self.restore_input();
                    }
                }
                Ok(git_mod::Result::Removed) => {
                    let was_active = self.current_mod().is_some_and(|m| m.id == id);
                    let selected = self.store.delete_mod(self.project_id, id)?;
                    self.mods.remove(index);
                    self.navigation.forget(id);
                    self.git_errors.remove(id);
                    self.git_states.remove(&id);
                    self.target_errors.remove(&id);
                    self.active = self.mods.iter().position(|m| Some(m.id) == selected);
                    if was_active {
                        self.visit_selected();
                        self.view = if self.active.is_some() {
                            View::Chat
                        } else {
                            View::NewMod
                        };

                        self.restore_input();
                    } else if let View::Mods(focused) | View::DeleteMod(focused) = self.view {
                        self.view = View::Mods(
                            focused
                                .saturating_sub(usize::from(index < focused))
                                .min(self.mods.len()),
                        );
                    }
                    self.notice = std::fs::remove_dir_all(root)
                        .err()
                        .filter(|error| error.kind() != io::ErrorKind::NotFound)
                        .map(|error| error.to_string());
                }
                Err(error) if label == "writing commit and PR text" => {
                    if self.current_mod().is_some_and(|m| m.id == id) {
                        self.publish_after_review = false;
                    }
                    self.git_errors.record(
                        id,
                        label,
                        format!("{error} Work is retained; Ctrl+S retries."),
                    );
                }
                Err(error) if label == "checking target branch" => {
                    self.target_errors.insert(id, error.to_string());
                    if self.current_mod().is_some_and(|m| m.id == id) {
                        self.publish_after_review = false;
                    }
                }
                Err(error) => {
                    if matches!(
                        label,
                        "updating from target branch" | "saving verified merge"
                    ) {
                        self.update_errors.insert(id);
                    }
                    if self.current_mod().is_some_and(|m| m.id == id) {
                        self.publish_after_review = false;
                    }
                    let detail = error.to_string();
                    let detail = detail.trim_end_matches(" Work is retained.");
                    self.git_errors.record(
                        id,
                        label,
                        if error.kind() == io::ErrorKind::Unsupported {
                            error.to_string()
                        } else {
                            format!("{detail} Work is retained; Ctrl+R retries.")
                        },
                    );
                }
            }
        }
        Ok(())
    }
}
