use super::*;

impl Engine {
    pub fn execution_busy(&self) -> bool {
        self.git_activity().is_some()
            || self.current_mod().is_some_and(|m| {
                self.workers
                    .values()
                    .any(|w| w.mod_id == m.id && (w.enabled || w.busy()))
            })
    }

    pub fn version_ready(&self) -> bool {
        !self.published()
            && !self.execution_busy()
            && self.current_mod().is_some_and(|m| {
                !m.closed
                    && m.git_root
                        .as_ref()
                        .is_none_or(|root| !root.join("main-update.json").exists())
                    && self.targets.get(&m.id).is_none_or(|target| {
                        target.pr_state.as_deref().is_none_or(|s| s == "OPEN")
                            && self
                                .git_states
                                .get(&m.id)
                                .is_none_or(|s| s.base == target.head)
                    })
                    && m.queue.is_empty()
                    && m.steering.is_empty()
                    && m.execution.as_ref().is_some_and(|e| {
                        e.complete() && matches!(e.status.as_str(), "review" | "applied")
                    })
            })
    }

    pub fn can_review(&self) -> bool {
        self.active.is_some_and(|index| self.review_ready(index))
    }

    pub(crate) fn pr_feedback_actionable(&self, m: &CodeMod) -> usize {
        self.targets.get(&m.id).map_or(0, |target| {
            target
                .pr_feedback
                .iter()
                .filter(|item| {
                    item.actionable
                        && !m
                            .agent_review
                            .as_ref()
                            .is_some_and(|r| r.pr_feedback_imported(&item.id))
                })
                .count()
        })
    }

    pub(crate) fn fix_pr_feedback(&mut self, ids: &[String]) -> Result<bool> {
        let Some(index) = self.active else {
            return Ok(false);
        };
        let id = self.mods[index].id;
        let items: Vec<_> = self
            .targets
            .get(&id)
            .map(|target| {
                target
                    .pr_feedback
                    .iter()
                    .filter(|item| ids.contains(&item.id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if items.is_empty() || self.store.import_pr_feedback(&self.mods[index], &items)? == 0 {
            self.mods[index].agent_review = self.store.review_state(id)?;
            return Ok(false);
        }
        self.mods[index].agent_review = self.store.review_state(id)?;
        self.begin_agent_review(index)?;
        self.view = View::Findings(0);
        Ok(true)
    }

    pub(crate) fn review_ready(&self, index: usize) -> bool {
        let m = &self.mods[index];
        !m.closed
            && m.queue.is_empty()
            && m.steering.is_empty()
            && !self.git_jobs.contains_key(&m.id)
            && m.git_root
                .as_ref()
                .is_none_or(|root| !root.join("main-update.json").exists())
            && self
                .targets
                .get(&m.id)
                .is_none_or(|t| t.pr_state.as_deref().is_none_or(|s| s == "OPEN"))
            && !self
                .workers
                .values()
                .any(|w| w.mod_id == m.id && (w.enabled || w.busy()))
            && m.execution
                .as_ref()
                .is_some_and(|e| e.complete() && e.status == "review" && e.fingerprint.is_some())
    }

    pub fn can_publish(&self) -> bool {
        self.version_ready()
            && self
                .current_mod()
                .is_some_and(|m| m.agent_review.as_ref().is_some_and(|r| r.clear(m)))
            && self
                .current_mod()
                .is_some_and(|m| !m.closed && m.queue.is_empty() && m.steering.is_empty())
            && self.current_mod().is_none_or(|m| {
                m.git_root
                    .as_ref()
                    .is_none_or(|root| git_mod::setup_pending(root).is_none())
            })
            && !self.execution_busy()
            && self.review.as_ref().is_some_and(|review| {
                review.count() > 0 || self.git_state().is_some_and(|s| s.pr.is_some())
            })
            && self
                .current_mod()
                .and_then(|m| m.execution.as_ref())
                .is_some_and(|execution| {
                    execution.complete()
                        && matches!(execution.status.as_str(), "review" | "applied")
                        && self.review.as_ref().is_some_and(|review| {
                            execution.fingerprint.as_deref() == Some(&review.fingerprint)
                        })
                })
    }

    pub(crate) fn open_review(&mut self) -> Result<()> {
        let Some(execution) = self.current_mod().and_then(|m| m.execution.as_ref()) else {
            return Ok(());
        };
        if self.execution_busy() {
            self.notice = Some("Pause the executor before reviewing its changes.".into());
            return Ok(());
        }
        if self.current_mod().is_some_and(|m| m.git_root.is_some()) {
            let id = self.current_mod().unwrap().id;
            let root = execution.workspace.clone();
            self.git_jobs.insert(
                id,
                Job::start("loading changes", move |_| {
                    workspace::review(&root).map(git_mod::Result::Reviewed)
                }),
            );
            return Ok(());
        }
        match workspace::review(&execution.workspace) {
            Ok(review) => {
                if execution.status == "review"
                    && execution.fingerprint.as_deref() != Some(&review.fingerprint)
                {
                    self.notice = Some(
                        "Working files changed since verification. Esc, then Ctrl+R to recheck."
                            .into(),
                    );
                    let index = self.active.unwrap();
                    self.store
                        .execution_status(self.mods[index].id, "blocked")?;
                    self.mods[index].execution.as_mut().unwrap().status = "blocked".into();
                } else {
                    self.notice = None;
                }
                self.review = Some(review);
                self.view = View::Review(0);
            }
            Err(error) => self.notice = Some(error.to_string()),
        }
        Ok(())
    }

    pub(crate) fn start_publication(&mut self, index: usize) {
        let code_mod = &self.mods[index];
        let root = code_mod.git_root.clone().unwrap();
        let context = Context::harness(&self.project, code_mod);
        let id = code_mod.id;
        if let Err(error) = crate::publication::load(&root, &code_mod.description) {
            self.git_errors.record(
                id,
                "writing commit and PR text",
                format!("{error} Preview the PR again before publishing."),
            );
            self.view = View::Chat;
            return;
        }
        if let Err(error) = git_mod::mark_publishing(&root) {
            self.git_errors
                .record(id, "publishing PR", error.to_string());
            return;
        }
        let keys = self
            .workers
            .iter()
            .filter(|(_, worker)| worker.mod_id == id)
            .map(|(key, _)| *key)
            .collect::<Vec<_>>();
        let workers = keys
            .into_iter()
            .filter_map(|key| self.workers.remove(&key))
            .collect::<Vec<_>>();
        self.executing_mods.remove(&id);
        self.auto_reviews.remove(&id);
        self.git_jobs.insert(
            id,
            Job::start("publishing PR", move |cancelled| {
                drop(workers);
                let mut tools = Dispatcher::new(&context, None, cancelled);
                tools.execute(Request::PublishPr { draft: false }, |_| {})?;
                Ok(git_mod::Result::Published)
            }),
        );
        self.review = None;
        self.view = View::Chat;
        self.notice = None;
    }

    pub(crate) fn set_paused(&mut self, index: usize, paused: bool) -> Result<()> {
        if paused {
            let id = self.mods[index].id;
            self.preparations.cancel(id, &self.store)?;
            self.slot_waits.retain(|(mod_id, _), _| *mod_id != id);
        }
        self.store.set_user_paused(self.mods[index].id, paused)?;
        self.mods[index].user_paused = paused;
        Ok(())
    }

    pub(crate) fn begin_agent_review(&mut self, index: usize) -> Result<()> {
        if !self.review_ready(index) {
            return Ok(());
        }
        let code_mod = &self.mods[index];
        let execution = code_mod.execution.as_ref().unwrap();
        let fingerprint = workspace::review(&execution.workspace)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
            .fingerprint;
        if execution.fingerprint.as_deref() != Some(&fingerprint) {
            self.store.execution_status(code_mod.id, "blocked")?;
            self.mods[index].execution = self.store.execution(self.mods[index].id)?;
            self.notice =
                Some("Source changed after checks. Ctrl+R reruns checks before review.".into());
            return Ok(());
        }
        self.store.begin_review(code_mod.id, &fingerprint)?;
        self.set_paused(index, false)?;
        let id = self.mods[index].id;
        self.mods[index].agent_review = self.store.review_state(id)?;
        if self.mods[index]
            .agent_review
            .as_ref()
            .is_none_or(|r| r.auto_remaining == 0)
        {
            self.auto_reviews.remove(&id);
        }
        let record = self.store.worker_provider(id, Role::Reviewer, 0, "codex")?;
        self.start_worker_record(index, Role::Reviewer, record)
    }

    pub(crate) fn maintain_reviews(&mut self) -> Result<()> {
        for index in 0..self.mods.len() {
            let m = &self.mods[index];
            let id = m.id;
            if m.closed || m.user_paused || !m.queue.is_empty() || !m.steering.is_empty() {
                self.auto_reviews.remove(&id);
            }
            if let Some(r) = m.agent_review.as_ref() {
                let same_plan = m
                    .planning
                    .as_ref()
                    .is_some_and(|p| p.source == r.plan_source);
                if r.status != "stale"
                    && (!same_plan
                        || !m.queue.is_empty()
                        || !m.steering.is_empty()
                        || (r.status != "fixing" && !r.current(m)))
                {
                    self.store
                        .0
                        .execute("UPDATE reviews SET status='stale' WHERE mod_id=?1", [id])?;
                    self.mods[index].agent_review = self.store.review_state(id)?;
                    for worker in self
                        .workers
                        .values_mut()
                        .filter(|w| w.mod_id == id && w.role == Role::Reviewer && w.enabled)
                    {
                        worker.toggle();
                    }
                    continue;
                }
            }
            if self.auto_reviews.contains(&id)
                && self.review_ready(index)
                && !m.user_paused
                && self.mods[index].agent_review.as_ref().is_some_and(|r| {
                    r.current(&self.mods[index]) && r.repairable() && r.auto_remaining > 0
                })
                && !(self.active == Some(index)
                    && matches!(
                        self.view,
                        View::Publish
                            | View::DeleteMod(_)
                            | View::CloseMod(_)
                            | View::Review(_)
                            | View::ProjectSetup(_, _)
                            | View::Repository(_)
                            | View::ConfirmRepository(_)
                    ))
            {
                if self.store.review_fixes(&self.mods[index])? {
                    self.mods[index].planning = self.store.planning(id)?;
                    self.mods[index].execution = self.store.execution(id)?;
                    self.mods[index].agent_review = self.store.review_state(id)?;
                    self.executing_mods.insert(id);
                }
            } else if self.review_due(index) {
                self.begin_agent_review(index)?;
            }
        }
        Ok(())
    }

    pub(crate) fn review_due(&self, index: usize) -> bool {
        let m = &self.mods[index];
        let confirming = self.current_mod().is_some_and(|current| current.id == m.id)
            && matches!(
                self.view,
                View::Review(_)
                    | View::Publish
                    | View::DeleteMod(_)
                    | View::CloseMod(_)
                    | View::ProjectSetup(_, _)
                    | View::Repository(_)
                    | View::ConfirmRepository(_)
            );
        self.review_ready(index)
            && !m.user_paused
            && (!self.mod_published(m)
                || m.agent_review.as_ref().is_some_and(|r| r.fixes_verified(m)))
            && !confirming
            && match m.agent_review.as_ref() {
                Some(r) if r.status == "fixing" => self.auto_reviews.contains(&m.id),
                Some(r) if r.status != "stale" && r.current(m) => false,
                _ => self.auto_reviews.contains(&m.id),
            }
    }
}
