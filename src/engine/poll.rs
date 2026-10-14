use super::*;

impl Engine {
    pub(crate) fn poll_workers(&mut self) -> Result<()> {
        self.appearance = self.store.appearance()?;
        self.preparations.poll(&self.store)?;
        self.refresh_network_requests()?;
        self.poll_git_jobs()?;
        self.maintain_project();
        for mod_id in std::mem::take(&mut self.auto_plans) {
            if let Some(index) = self.mods.iter().position(|m| m.id == mod_id) {
                self.start_worker(index, Role::Planner)?;
            }
        }
        let focused = match self.view {
            View::Queue(index) | View::EditQueue(index) => self
                .current_mod()
                .and_then(|code_mod| code_mod.queue.get(index))
                .map(|message| message.id),
            _ => None,
        };
        let editing_mod = matches!(self.view, View::EditQueue(_))
            .then(|| self.current_mod().map(|code_mod| code_mod.id))
            .flatten();
        let deleting_mod = match self.view {
            View::DeleteMod(index) | View::CloseMod(index) => Some(self.mods[index].id),
            _ => None,
        };
        let reviewing_mod = matches!(
            self.view,
            View::Review(_)
                | View::Publish
                | View::ProjectSetup(true, _)
                | View::Repository(_)
                | View::ConfirmRepository(_)
        )
        .then(|| self.current_mod().map(|m| m.id))
        .flatten();
        self.resume_network_workers()?;
        let scheduled = self
            .mods
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                self.executing_mods.contains(&m.id)
                    && !m.closed
                    && !m.user_paused
                    && !self.git_jobs.contains_key(&m.id)
                    && editing_mod != Some(m.id)
                    && deleting_mod != Some(m.id)
                    && reviewing_mod != Some(m.id)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let admitted = scheduled
            .iter()
            .map(|index| self.mods[*index].id)
            .collect::<BTreeSet<_>>();
        self.slot_waits.retain(|(id, _), _| admitted.contains(id));
        for index in scheduled {
            self.schedule_execution(index)?;
        }
        for worker in self.workers.values_mut() {
            let code_mod = self
                .mods
                .iter_mut()
                .find(|code_mod| code_mod.id == worker.mod_id)
                .unwrap();
            worker.poll(
                &mut self.store,
                code_mod,
                editing_mod != Some(code_mod.id)
                    && (worker.role != Role::Executor
                        || self.executing_mods.contains(&code_mod.id) && worker.enabled)
                    && deleting_mod != Some(code_mod.id)
                    && reviewing_mod != Some(code_mod.id)
                    && !self
                        .network_requests
                        .iter()
                        .any(|r| r.worker == worker.id && r.status == "approved")
                    && !self.git_jobs.contains_key(&code_mod.id)
                    && !code_mod.closed,
                &code_mod
                    .git_root
                    .as_ref()
                    .map(|root| git_mod::checkout(root))
                    .unwrap_or_else(|| self.project.clone()),
            )?;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        for m in &mut self.mods {
            for worker in self.workers.values().filter(|w| w.mod_id == m.id) {
                self.store
                    .record_phase(m.id, worker.id, worker.phase(m), now)?;
            }
            self.store.record_failures(m)?;
            m.diagnostics = self.store.diagnostics(m.id)?;
        }
        for index in self.ready_plans() {
            self.auto_runs.remove(&self.mods[index].id);
            self.start_execution(index)?;
        }
        self.maintain_reviews()?;
        self.maintain_updates()?;
        let active = self
            .workers
            .values()
            .filter(|worker| worker.enabled || worker.busy())
            .map(|worker| worker.mod_id)
            .collect();
        for id in self.preparations.expired(&active, Instant::now()) {
            self.workers
                .retain(|_, worker| worker.mod_id != id || worker.enabled || worker.busy());
            self.preparations.cancel(id, &self.store)?;
        }
        let edits = self
            .mods
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                !m.closed
                    && !m.user_paused
                    && !m.queue.is_empty()
                    && !self.git_jobs.contains_key(&m.id)
                    && m.git_root.as_ref().is_none_or(|root| {
                        crate::mod_sync::load(root)
                            .ok()
                            .flatten()
                            .is_none_or(|u| u.installed)
                    })
                    && self
                        .git_states
                        .get(&m.id)
                        .is_none_or(|state| !state.continuing)
                    && editing_mod != Some(m.id)
                    && deleting_mod != Some(m.id)
                    && reviewing_mod != Some(m.id)
                    && m.execution.as_ref().is_some_and(|e| {
                        matches!(e.status.as_str(), "review" | "applied" | "blocked")
                            && !e.tasks.iter().any(|t| t.status == "waiting")
                    })
                    && !self
                        .workers
                        .values()
                        .any(|w| w.mod_id == m.id && (w.enabled || w.busy()))
            })
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        for index in edits {
            self.start_continuation(index);
        }
        match self.view {
            View::Queue(index) => {
                let next = self
                    .current_mod()
                    .and_then(|code_mod| {
                        code_mod
                            .queue
                            .iter()
                            .position(|message| Some(message.id) == focused)
                    })
                    .unwrap_or(index);
                self.clamp_queue(next);
            }
            View::EditQueue(_) => {
                if let Some(index) = self.current_mod().and_then(|code_mod| {
                    code_mod
                        .queue
                        .iter()
                        .position(|message| Some(message.id) == focused)
                }) {
                    self.view = View::EditQueue(index);
                } else {
                    self.view = View::Chat;
                    self.restore_input();
                }
            }
            _ => {}
        }
        self.projects = self.store.projects()?;
        Ok(())
    }

    pub fn pending_network(&self) -> Option<&crate::network::Access> {
        let id = self.current_mod()?.id;
        self.network_requests
            .iter()
            .find(|r| r.mod_id == id && r.status == "pending")
    }

    pub(crate) fn refresh_network_requests(&mut self) -> Result<()> {
        self.network_requests.clear();
        for code_mod in self.mods.iter().filter(|m| !m.closed) {
            self.network_requests
                .extend(self.store.network_requests(code_mod.id)?);
        }
        if let View::Network(id, _) = self.view
            && !self
                .network_requests
                .iter()
                .any(|r| r.id == id && r.status == "pending")
        {
            self.view = self
                .pending_network()
                .map_or(View::Chat, |r| View::Network(r.id, 0));
        }
        Ok(())
    }

    pub(crate) fn resume_network_workers(&mut self) -> Result<()> {
        let requests = self
            .network_requests
            .iter()
            .filter(|r| r.status == "approved")
            .cloned()
            .collect::<Vec<_>>();
        for access in requests {
            if !self.executing_mods.contains(&access.mod_id)
                || self.git_jobs.contains_key(&access.mod_id)
                || self.workers.get(&access.worker).is_some_and(Worker::busy)
            {
                continue;
            }
            let Some(index) = self
                .mods
                .iter()
                .position(|m| m.id == access.mod_id && !m.closed && !m.user_paused)
            else {
                continue;
            };
            if let Some(record) = self.store.resume_network(&access)? {
                self.workers.remove(&record.id);
                self.mods[index].execution = self.store.execution(access.mod_id)?;
                self.notice = None;
            }
        }
        self.refresh_network_requests()
    }
}
