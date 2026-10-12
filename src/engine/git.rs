use super::*;

impl Engine {
    pub(crate) fn create_mod(&mut self) -> Result<()> {
        let name = self.composer.clone();
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        if !git_mod::has_commit(&self.project) && self.setup_root.is_none() {
            let root = self
                .store
                .workspace_path(0)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            match workspace::create(&self.project, &root) {
                Ok(()) => {
                    self.setup_files = workspace::source_state(&root.join("before"))
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
                        .into_iter()
                        .map(|(path, _, _)| path.display().to_string())
                        .collect();
                    self.setup_root = Some(root);
                    self.view = View::ProjectSetup(false, 0);
                    self.notice = None;
                }
                Err(error) => self.notice = Some(error.to_string()),
            }
            return Ok(());
        }
        self.project_job.take();
        let mut code_mod = self.store.create_mod(self.project_id, name)?;
        {
            let root = self
                .store
                .workspace_path(code_mod.id)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            let initialize = if let Some(preview) = self.setup_root.take() {
                std::fs::rename(preview, &root)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                std::fs::write(root.join("project-setup"), b"new")
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                true
            } else {
                false
            };
            self.store.save_git_root(code_mod.id, &root)?;
            code_mod.git_root = Some(root.clone());
            let context = Context::harness(&self.project, &code_mod);
            self.git_jobs.insert(
                code_mod.id,
                Job::start(
                    if initialize {
                        "setting up Git"
                    } else {
                        "checking project branch"
                    },
                    move |cancelled| {
                        Dispatcher::new(&context, None, cancelled).execute(
                            if initialize {
                                Request::InitializeProject
                            } else {
                                Request::CreateWorktree
                            },
                            |_| {},
                        )?;
                        Ok(git_mod::Result::Prepared)
                    },
                ),
            );
        }
        self.mods.push(code_mod);
        self.active = Some(self.mods.len() - 1);
        self.visit_selected();
        self.creation_draft.clear();
        self.notice = None;

        self.view = View::Chat;
        self.restore_input();
        self.celebrate();
        Ok(())
    }

    pub(crate) fn adopt_mod(&mut self) -> Result<()> {
        let index = self.active.unwrap();
        let code_mod = &mut self.mods[index];
        let root = code_mod.execution.as_ref().unwrap().workspace.clone();
        std::fs::write(root.join("project-setup"), b"saved")
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        self.store.save_git_root(code_mod.id, &root)?;
        code_mod.git_root = Some(root);
        let id = code_mod.id;
        let context = Context::harness(&self.project, code_mod);
        self.git_jobs.insert(
            id,
            Job::start("adopting saved codemod", move |cancelled| {
                Dispatcher::new(&context, None, cancelled)
                    .execute(Request::AdoptSnapshot, |_| {})?;
                Ok(git_mod::Result::Adopted)
            }),
        );
        self.view = View::Chat;
        self.notice = None;
        self.restore_input();
        Ok(())
    }

    pub(crate) fn open_publication(&mut self) -> Result<()> {
        if !self.can_publish() {
            return Ok(());
        }
        if self.current_mod().is_some_and(|m| m.git_root.is_some())
            && git_mod::has_origin(&self.project)
        {
            self.check_target(self.active.unwrap(), true, true);
            return Ok(());
        }
        self.confirm_publication()
    }

    pub(crate) fn confirm_publication(&mut self) -> Result<()> {
        let code_mod = &self.mods[self.active.unwrap()];
        if let Some(root) = &code_mod.git_root {
            if self
                .git_states
                .get(&code_mod.id)
                .is_some_and(|state| state.closing)
            {
                git_mod::cancel_closing(root)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                self.git_states
                    .insert(code_mod.id, git_mod::load(root).unwrap());
            }
            if let Some(request) = git_mod::repository_pending(root) {
                self.open_repository();
                self.composer.push_str(&request.slug);
                self.view = View::Repository(request.create);
            } else if !git_mod::has_origin(&self.project) {
                self.open_repository();
            } else {
                let id = code_mod.id;
                let root = root.clone();
                let description = code_mod.description.clone();
                let evidence = crate::publication::evidence(code_mod);
                self.publication = None;
                self.view = View::Chat;
                self.git_jobs.insert(
                    id,
                    Job::start("writing commit and PR text", move |cancelled| {
                        crate::publication::prepare(&root, &description, &evidence, cancelled)
                            .map(git_mod::Result::PublicationDraft)
                    }),
                );
            }
        } else {
            self.setup_files = workspace::source_state(
                &code_mod
                    .execution
                    .as_ref()
                    .unwrap()
                    .workspace
                    .join("before"),
            )
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
            .into_iter()
            .map(|(path, _, _)| path.display().to_string())
            .collect();
            self.view = View::ProjectSetup(true, 0);
        }
        Ok(())
    }

    pub(crate) fn open_repository(&mut self) {
        self.composer.clear();
        self.view = View::Repository(false);
        self.notice = None;
    }

    pub(crate) fn start_repository(&mut self, request: git_mod::RepositoryRequest) {
        let code_mod = self.current_mod().unwrap();
        let id = code_mod.id;
        let context = Context::harness(&self.project, code_mod);
        self.git_jobs.insert(
            id,
            Job::start("connecting GitHub repository", move |cancelled| {
                Dispatcher::new(&context, None, cancelled)
                    .execute(Request::ConnectRepository(request), |_| {})?;
                Ok(git_mod::Result::RepositoryConnected)
            }),
        );
        self.view = View::Chat;
        self.restore_input();
        self.notice = None;
    }

    pub(crate) fn delete_mod(&mut self, index: usize) -> Result<()> {
        self.finish_mod(index, true, true)
    }

    pub(crate) fn finish_mod(&mut self, index: usize, discard: bool, delete: bool) -> Result<()> {
        self.preparations.cancel(self.mods[index].id, &self.store)?;
        if !delete && self.mods[index].git_root.is_none() && self.mods[index].execution.is_some() {
            self.active = Some(index);
            let root = &self.mods[index].execution.as_ref().unwrap().workspace;
            std::fs::write(root.join("close-request"), b"close")
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            self.setup_files = workspace::source_state(&root.join("before"))
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
                .into_iter()
                .map(|(path, _, _)| path.display().to_string())
                .collect();
            self.view = View::ProjectSetup(true, 0);
            return Ok(());
        }
        if let Some(root) = self.mods[index].git_root.clone() {
            let id = self.mods[index].id;
            if self.git_jobs.contains_key(&id) {
                return Ok(());
            }
            let context = Context::harness(&self.project, &self.mods[index]);
            let keys = self
                .workers
                .iter()
                .filter(|(_, worker)| worker.mod_id == id)
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            let workers = keys
                .into_iter()
                .filter_map(|key| self.workers.remove(&key))
                .collect::<Vec<_>>();
            self.auto_plans.remove(&id);
            self.auto_runs.remove(&id);
            self.auto_reviews.remove(&id);
            self.executing_mods.remove(&id);
            self.git_jobs.insert(
                id,
                Job::start(
                    if delete {
                        "deleting mod"
                    } else {
                        "closing mod"
                    },
                    move |cancelled| {
                        drop(workers);
                        let mut tools = Dispatcher::new(&context, None, cancelled);
                        if !root.join("git-mod.json").exists() {
                            if !delete {
                                return Err(io::Error::other(
                                    "Finish Git setup before closing. Saved files are retained.",
                                ));
                            }
                            if root.join("checkout").exists()
                                || (git_mod::setup_pending(&root).is_none()
                                    && ["work", "vm.json"]
                                        .iter()
                                        .any(|name| root.join(name).exists()))
                            {
                                return Err(io::Error::other(
                                    "Missing worktree metadata; files are retained.",
                                ));
                            }
                            tools.execute(Request::CleanupMod, |_| {})?;
                            if delete {
                                git_mod::remove_files(&root, cancelled)?;
                            }
                            return Ok(if delete {
                                git_mod::Result::Removed
                            } else {
                                git_mod::Result::Closed
                            });
                        }
                        git_mod::mark_closing(&root, discard, delete)?;
                        if git_mod::load(&root)?.phase == "preparing"
                            && git_mod::checkout(&root).exists()
                        {
                            tools.execute(Request::CreateWorktree, |_| {})?;
                        }
                        if !discard && crate::sandbox::Sandbox::has_bound_source(&root)? {
                            let mut vm =
                                crate::sandbox::Sandbox::prepare(&root, cancelled, |_| {})?;
                            vm.export(cancelled)?;
                        }
                        tools.execute(
                            if delete {
                                Request::CleanupMod
                            } else {
                                Request::CloseMod
                            },
                            |_| {},
                        )?;
                        if delete {
                            git_mod::remove_files(&root, cancelled)?;
                        }
                        Ok(if delete {
                            git_mod::Result::Removed
                        } else {
                            git_mod::Result::Closed
                        })
                    },
                ),
            );
            self.view = View::Chat;
            self.notice = None;
            self.restore_input();
            return Ok(());
        }
        let mod_id = self.mods[index].id;
        let previous = self.current_mod().map(|code_mod| code_mod.id);
        let workspace = self.mods[index]
            .execution
            .as_ref()
            .map(|execution| execution.workspace.clone());
        self.auto_plans.remove(&mod_id);
        self.auto_runs.remove(&mod_id);
        self.auto_reviews.remove(&mod_id);
        self.executing_mods.remove(&mod_id);
        self.workers.retain(|_, worker| worker.mod_id != mod_id);
        if workspace.is_some()
            && let Err(error) = Dispatcher::new(
                &Context::harness(&self.project, &self.mods[index]),
                None,
                &std::sync::atomic::AtomicBool::new(false),
            )
            .execute(Request::CleanupMod, |_| {})
        {
            self.notice = Some(format!(
                "Could not delete the VM: {error}. The mod is retained."
            ));
            self.view = View::Chat;
            self.restore_input();
            return Ok(());
        }
        let selected = if delete {
            self.store.delete_mod(self.project_id, mod_id)?
        } else {
            self.store.close_mod(self.project_id, mod_id)?
        };
        if delete {
            self.mods.remove(index);
            self.navigation.forget(mod_id);
        } else {
            self.mods[index].closed = true;
        }
        let cleanup_error = workspace.and_then(|path| std::fs::remove_dir_all(path).err());
        self.active = self
            .mods
            .iter()
            .position(|code_mod| Some(code_mod.id) == selected);
        if previous != selected {
            self.notice = None;
        }
        if let Some(error) = cleanup_error {
            self.notice = Some(format!(
                "Mod {}; working folder cleanup failed: {error}",
                if delete { "deleted" } else { "closed" }
            ));
        }

        self.visit_selected();
        self.view = if self.active.is_some() {
            View::Chat
        } else {
            View::NewMod
        };
        self.restore_input();
        Ok(())
    }

    pub fn has_worker(&self, mod_id: i64) -> bool {
        self.workers.values().any(|worker| worker.mod_id == mod_id)
    }

    pub fn read_only(&self) -> bool {
        self.current_mod().is_some_and(|m| m.closed)
    }

    pub(crate) fn reopen_mod(&mut self, index: usize) {
        if self.git_jobs.contains_key(&self.mods[index].id) {
            return;
        }
        let context = Context::harness(&self.project, &self.mods[index]);
        if self.mods[index].git_root.is_none() {
            self.notice =
                Some("This older codemod has no saved branch. Start a new codemod.".into());
            return;
        }
        if (self.active != Some(index) || self.view == View::NewMod)
            && let Err(error) = self.save_before_selection()
        {
            self.notice = Some(error.to_string());
            return;
        }
        if let Err(error) = self.store.select_mod(self.project_id, self.mods[index].id) {
            self.notice = Some(error.to_string());
            return;
        }
        self.active = Some(index);
        self.visit_selected();
        self.git_jobs.insert(
            self.mods[index].id,
            Job::start("reopening codemod", move |cancelled| {
                Dispatcher::new(&context, None, cancelled).execute(Request::ReopenMod, |_| {})?;
                Ok(git_mod::Result::Reopened)
            }),
        );
        self.view = View::Chat;
        self.restore_input();
    }

    pub(crate) fn start_continuation(&mut self, index: usize) {
        let id = self.mods[index].id;
        if let Err(error) = self.preparations.cancel(id, &self.store) {
            self.notice = Some(error.to_string());
            return;
        }
        self.auto_reviews.remove(&id);
        self.executing_mods.remove(&id);
        self.workers.retain(|_, worker| worker.mod_id != id);
        if self.mods[index].git_root.is_none() {
            if let Err(error) = self.finish_edit(index) {
                self.notice = Some(error.to_string());
            }
            return;
        }
        let context = Context::harness(&self.project, &self.mods[index]);
        self.git_jobs.insert(
            id,
            Job::start("preparing edits", move |cancelled| {
                Dispatcher::new(&context, None, cancelled)
                    .execute(Request::PrepareEdits, |_| {})?;
                Ok(git_mod::Result::Continued)
            }),
        );
    }

    pub(crate) fn finish_edit(&mut self, index: usize) -> Result<()> {
        let id = self.mods[index].id;
        self.preparations.cancel(id, &self.store)?;
        if self.mods[index]
            .execution
            .as_ref()
            .is_some_and(|e| e.status == "planning")
        {
            self.auto_plans.insert(id);
            return Ok(());
        }
        let Some(message) = self.mods[index].queue.first().cloned() else {
            return Ok(());
        };
        self.store.follow_up(id, &message.body, Some(message.id))?;
        let state = self.store.load_project(&self.project)?;
        self.mods[index] = state.mods.into_iter().find(|m| m.id == id).unwrap();
        self.auto_plans.insert(id);
        if self.current_mod().is_some_and(|m| m.id == id) {
            self.review = None;
        }
        Ok(())
    }

    pub fn git_state(&self) -> Option<&GitMod> {
        self.current_mod().and_then(|m| self.git_states.get(&m.id))
    }

    pub fn git_activity(&self) -> Option<&str> {
        self.current_mod()
            .and_then(|m| self.git_jobs.get(&m.id))
            .map(|job| job.label)
    }

    pub fn project_activity(&self) -> Option<&str> {
        self.project_job.as_ref().map(|job| job.label)
    }

    pub fn git_retry_pending(&self) -> bool {
        self.current_mod().is_some_and(|m| {
            self.update_errors.contains(&m.id)
                || m.git_root.as_ref().is_some_and(|root| {
                    git_mod::setup_pending(root).is_some()
                        || git_mod::repository_pending(root).is_some()
                })
                || m.git_root.is_some()
                    && self.git_states.get(&m.id).is_none_or(|state| {
                        state.removing
                            || (state.closing && !m.closed)
                            || state.continuing
                            || state.publishing
                            || !["ready", "published", "cleaned", "closed", "pruned"]
                                .contains(&state.phase.as_str())
                    })
        })
    }

    pub fn published(&self) -> bool {
        self.current_mod().is_some_and(|m| self.mod_published(m))
    }

    pub(crate) fn mod_published(&self, m: &CodeMod) -> bool {
        self.git_states.get(&m.id).is_some_and(|git| {
            git.published()
                && m.execution
                    .as_ref()
                    .is_none_or(|e| e.complete() && git.published_version(e.fingerprint.as_deref()))
        })
    }
}
