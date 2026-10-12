use super::*;

fn invalid(message: &str) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(io::Error::other(message.to_owned())))
}

impl Engine {
    pub(crate) fn select_mod(&mut self, index: usize) -> Result<()> {
        self.store
            .select_mod(self.project_id, self.mods[index].id)?;
        self.active = Some(index);
        self.notice = None;
        self.view = View::Chat;
        self.restore_input();
        Ok(())
    }

    fn queued_index(&self, message_id: i64) -> Result<(usize, usize)> {
        let active = self
            .active
            .ok_or_else(|| invalid("Select a codemod first."))?;
        let code_mod = &self.mods[active];
        if code_mod.closed {
            return Err(invalid("Reopen the codemod before editing its queue."));
        }
        let index = code_mod
            .queue
            .iter()
            .position(|m| m.id == message_id)
            .ok_or_else(|| invalid("This queued instruction is no longer available."))?;
        Ok((active, index))
    }

    fn editable_queued(&self, message_id: i64) -> Result<(usize, usize)> {
        let (active, index) = self.queued_index(message_id)?;
        if self
            .store
            .is_pending(self.mods[active].id, &source_id(message_id, false))?
        {
            return Err(invalid(
                "This instruction has already been sent to a worker.",
            ));
        }
        Ok((active, index))
    }

    pub(crate) fn edit_queued(&mut self, message_id: i64, body: &str) -> Result<()> {
        if body.trim().is_empty() {
            return Err(invalid("Queued instructions cannot be empty."));
        }
        let (active, index) = self.editable_queued(message_id)?;
        let code_mod = &mut self.mods[active];
        self.store.edit_queued(code_mod.id, message_id, body)?;
        code_mod.queue[index].body = body.into();
        self.celebrate();
        Ok(())
    }

    pub(crate) fn remove_queued(&mut self, message_id: i64) -> Result<()> {
        let (active, index) = self.editable_queued(message_id)?;
        let code_mod = &mut self.mods[active];
        self.store.remove_queued(code_mod.id, message_id)?;
        code_mod.queue.remove(index);
        self.clamp_queue(index);
        Ok(())
    }

    pub(crate) fn move_queued(&mut self, message_id: i64, up: bool) -> Result<()> {
        let (active, index) = self.queued_index(message_id)?;
        let code_mod = &mut self.mods[active];
        let target = if up {
            index.saturating_sub(1)
        } else {
            (index + 1).min(code_mod.queue.len() - 1)
        };
        if target != index {
            self.store
                .swap_queued(code_mod.id, message_id, code_mod.queue[target].id)?;
            code_mod.queue.swap(index, target);
            if matches!(self.view, View::Queue(_)) {
                self.view = View::Queue(target);
            }
        }
        Ok(())
    }

    pub(crate) fn steer_queued(&mut self, ids: &[i64], target: Option<i64>) -> Result<()> {
        if ids.is_empty() {
            return Err(invalid("Choose at least one queued instruction."));
        }
        let active = self.editable_queued(ids[0])?.0;
        for id in ids {
            self.editable_queued(*id)?;
        }
        let running = self.running_workers();
        let targets = if let Some(id) = target {
            if !running.contains(&id) {
                return Err(invalid(
                    "That worker has stopped. Select a running worker or all.",
                ));
            }
            vec![id]
        } else {
            running
        };
        let mod_id = self.mods[active].id;
        let messages = self.store.steer_to(mod_id, ids, &targets)?;
        self.mods[active]
            .queue
            .retain(|message| !ids.contains(&message.id));
        self.mods[active].steering.extend(messages);
        self.clamp_queue(0);
        Ok(())
    }

    pub(crate) fn decide_network(&mut self, id: i64, allow: bool) -> Result<()> {
        let Some(index) = self.active else {
            return Ok(());
        };
        let mod_id = self.mods[index].id;
        self.store.decide_network(mod_id, id, allow)?;
        if allow {
            self.set_paused(index, false)?;
            self.executing_mods.insert(mod_id);
        }
        self.refresh_network_requests()
    }

    pub(crate) fn confirm_dialog(&mut self) -> Result<()> {
        match self.view {
            View::ProjectSetup(saved, _) => {
                if saved {
                    self.adopt_mod()?;
                } else {
                    self.create_mod()?;
                }
            }
            View::Publish if self.can_publish() => self.start_publication(self.active.unwrap()),
            View::CloseMod(index) | View::DeleteMod(index) => {
                let delete = matches!(self.view, View::DeleteMod(_));
                let action = if delete {
                    Action::Delete
                } else {
                    Action::Close
                };
                if Some(index) != self.active
                    || !self
                        .action_dock()
                        .actions
                        .iter()
                        .any(|a| a.action == action)
                {
                    return Err(invalid(
                        "The codemod changed. Review the updated state before continuing.",
                    ));
                }
                self.action_origin = None;
                self.finish_mod(index, delete, delete)?;
            }
            View::ConfirmRepository(create) if git_mod::valid_slug(self.composer.trim()) => {
                self.start_repository(git_mod::RepositoryRequest {
                    slug: self.composer.trim().into(),
                    create,
                });
            }
            _ => return Err(invalid("This confirmation is no longer available.")),
        }
        Ok(())
    }

    pub(crate) fn cancel_dialog(&mut self) -> Result<()> {
        match self.view {
            View::ProjectSetup(saved, _) => {
                if let Some(root) = self.setup_root.take() {
                    let _ = std::fs::remove_dir_all(root);
                }
                let closing = self
                    .current_mod()
                    .and_then(|m| m.execution.as_ref())
                    .is_some_and(|e| e.workspace.join("close-request").exists());
                if closing {
                    if let Some(e) = self.current_mod().and_then(|m| m.execution.as_ref()) {
                        let _ = std::fs::remove_file(e.workspace.join("close-request"));
                    }
                    self.view = View::Chat;
                } else {
                    self.view = if saved { View::Review(0) } else { View::NewMod };
                }
            }
            View::Repository(_) | View::ConfirmRepository(_) | View::Publish => {
                self.view = View::Review(0);
                self.restore_input();
            }
            _ => {
                self.view = if self.active.is_some() {
                    View::Chat
                } else {
                    View::NewMod
                };
                self.review = None;
                self.publish_after_review = false;
                self.restore_input();
            }
        }
        Ok(())
    }
}
