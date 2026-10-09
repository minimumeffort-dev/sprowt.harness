use super::{Action, App, View};
use crate::{
    execution::{CheckResult, TaskRun},
    plan::Task,
    store::Message,
    worker::Status,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub struct TaskInspection<'a> {
    pub number: usize,
    pub task: &'a Task,
    pub run: &'a TaskRun,
    pub identity: String,
    pub state: &'static str,
    pub note: String,
    pub error: Option<&'a str>,
    pub latest: Option<&'a Message>,
    pub checks: Vec<&'a CheckResult>,
    pub checks_label: &'static str,
}

impl App {
    pub fn failed_check_owner(&self) -> Option<i64> {
        let m = self.current_mod()?;
        if m.closed || self.execution_busy() {
            return None;
        }
        let run = m.execution.as_ref()?.failed_check_owner()?;
        (!self
            .network_requests
            .iter()
            .any(|r| r.task == run.id && r.status == "pending"))
        .then_some(run.id)
    }

    pub(super) fn fix_failed_check(&mut self, id: i64) -> rusqlite::Result<()> {
        if self.failed_check_owner() != Some(id) {
            return Ok(());
        }
        let index = self.active.unwrap();
        let mod_id = self.mods[index].id;
        let owner = self.mods[index]
            .execution
            .as_ref()
            .unwrap()
            .failed_check_owner()
            .unwrap()
            .worker;
        if self.store.fix_failed_check(mod_id, id)? {
            self.set_paused(index, false)?;
            if let Some(owner) = owner {
                self.workers.remove(&owner);
            }
            self.mods[index].execution = self.store.execution(mod_id)?;
            self.executing_mods.insert(mod_id);
            self.notice = None;
            self.view = View::Checks(0);
        }
        Ok(())
    }

    pub fn can_retry_task(&self, id: i64) -> bool {
        self.current_mod().is_some_and(|m| {
            !m.closed
                && !self.git_jobs.contains_key(&m.id)
                && m.execution.as_ref().is_some_and(|e| {
                    e.tasks.iter().any(|r| {
                        r.id == id
                            && matches!(r.status.as_str(), "paused" | "blocked")
                            && !self
                                .workers
                                .values()
                                .any(|w| w.task_run_id() == Some(id) && w.busy())
                    })
                })
                && !self
                    .network_requests
                    .iter()
                    .any(|r| r.task == id && r.status == "pending")
        })
    }

    pub(super) fn retry_task(&mut self, id: i64) -> rusqlite::Result<()> {
        if !self.can_retry_task(id) {
            return Ok(());
        }
        let index = self.active.unwrap();
        let mod_id = self.mods[index].id;
        let owner = self.mods[index]
            .execution
            .as_ref()
            .unwrap()
            .tasks
            .iter()
            .find(|r| r.id == id)
            .and_then(|r| r.worker);
        if self.store.retry_task(mod_id, id)? {
            if let Some(index) = self.active {
                self.set_paused(index, false)?;
            }
            if let Some(owner) = owner
                && self.workers.get(&owner).is_some_and(|w| !w.busy())
            {
                self.workers.remove(&owner);
            }
            self.mods[index].execution = self.store.execution(mod_id)?;
            self.executing_mods.insert(mod_id);
            self.notice = None;
        }
        Ok(())
    }

    pub fn task_ids(&self) -> Vec<i64> {
        let Some(m) = self.current_mod() else {
            return Vec::new();
        };
        let Some(plan) = m.planning.as_ref().and_then(|p| p.plan.as_ref()) else {
            return Vec::new();
        };
        let Some(execution) = &m.execution else {
            return Vec::new();
        };
        plan.tasks
            .iter()
            .filter_map(|task| {
                execution
                    .tasks
                    .iter()
                    .find(|run| run.task_id == task.id)
                    .map(|run| run.id)
            })
            .collect()
    }

    pub fn inspect_task(&self, id: i64) -> Option<TaskInspection<'_>> {
        let m = self.current_mod()?;
        let execution = m.execution.as_ref()?;
        let plan = m.planning.as_ref()?.plan.as_ref()?;
        let run = execution.tasks.iter().find(|run| run.id == id)?;
        let (index, task) = plan
            .tasks
            .iter()
            .enumerate()
            .find(|(_, task)| task.id == run.task_id)?;
        let worker = run
            .worker
            .and_then(|id| self.workers.get(&id))
            .filter(|worker| {
                worker.task_run_id() == Some(run.id)
                    || worker.task_run_id().is_none()
                        && (worker.status == Status::Connecting
                            && execution
                                .tasks
                                .iter()
                                .find(|r| {
                                    r.worker == Some(worker.id)
                                        && matches!(
                                            r.status.as_str(),
                                            "sending" | "running" | "checking"
                                        )
                                })
                                .or_else(|| {
                                    execution
                                        .ready_tasks(plan)
                                        .find(|r| r.worker == Some(worker.id))
                                })
                                .is_some_and(|r| r.id == run.id)
                            || worker.status == Status::Failed
                                && run.status != "done"
                                && !run.summary.is_empty()
                                && worker.error.as_deref() == Some(run.summary.as_str()))
            });
        let latest = m.messages.iter().rev().find(|message| {
            message.task == Some(id)
                && (message.role.starts_with("codex:") || message.role.starts_with("muse:"))
        });
        let provider = run.provider.as_deref().unwrap_or(&task.worker);
        let mut identity = if provider == "auto" {
            "Worker not assigned".into()
        } else {
            provider.to_owned()
        };
        if let Some(worker) = run.worker {
            identity.push_str(&format!(" · w{worker}"));
        }
        let model = worker
            .and_then(|w| w.model.as_deref())
            .or_else(|| latest.and_then(|message| message.model.as_deref()));
        if let Some(model) = model {
            let effort = worker
                .and_then(|w| w.effort.as_deref())
                .or_else(|| latest.and_then(|message| message.effort.as_deref()))
                .unwrap_or("effort unknown");
            identity.push_str(&format!(" · {model} · {effort}"));
        } else if let Some(selection) = &run.selection {
            identity.push_str(&format!(" · {} · {}", selection.model, selection.effort));
        }
        let mut checks: Vec<_> = execution
            .checks
            .iter()
            .filter(|check| check.task == Some(id))
            .collect();
        let mut checks_label = "Final checks";
        if checks.is_empty() {
            checks = run.checks.iter().collect();
            checks_label = "Checks";
        }
        let error = worker.and_then(|worker| worker.error.as_deref());
        let busy = worker.is_some_and(|w| w.busy());
        let dependencies = task
            .depends_on
            .iter()
            .filter_map(|dependency| {
                let unfinished = execution
                    .tasks
                    .iter()
                    .any(|r| &r.task_id == dependency && r.status != "done");
                unfinished
                    .then(|| {
                        plan.tasks
                            .iter()
                            .position(|t| &t.id == dependency)
                            .map(|i| (i + 1).to_string())
                    })
                    .flatten()
            })
            .collect::<Vec<_>>();
        let question = m
            .coordination
            .iter()
            .find(|message| message.task == Some(id) && m.needs_answer(message));
        let network = self
            .network_requests
            .iter()
            .find(|access| access.task == id && access.status == "pending");
        let (state, note) = if let Some(question) = question {
            (
                "Needs your answer",
                format!("Question #{} · {}", question.id, question.body),
            )
        } else if network.is_some() {
            (
                "Needs network access",
                "Review the requested domains with Ctrl+N.".into(),
            )
        } else if busy {
            if worker.is_some_and(|w| w.status == Status::Stopping) {
                ("Stopping", String::new())
            } else if worker.is_some_and(|w| w.status == Status::Checking) {
                ("Checking", String::new())
            } else if worker.is_some_and(|w| w.status == Status::Connecting) {
                ("Connecting worker", String::new())
            } else if let Some(conflict) = &run.conflict {
                (
                    "Resolving conflicts",
                    conflict.progress(run.conflict_retries),
                )
            } else if run.check_repair {
                (
                    if worker
                        .is_some_and(|w| matches!(w.status, Status::Starting | Status::Routing))
                    {
                        "Starting repair"
                    } else {
                        "Fixing failed check"
                    },
                    String::new(),
                )
            } else if run.restoring_runtime() {
                (
                    "Restoring environment",
                    "The worker is preparing the missing runtime.".into(),
                )
            } else if worker.is_some_and(|w| matches!(w.status, Status::Starting | Status::Routing))
            {
                ("Starting", String::new())
            } else {
                ("Working", String::new())
            }
        } else if checks.iter().any(|check| check.failed()) {
            (
                if checks_label == "Final checks" {
                    "Final checks failed"
                } else {
                    "Check failed"
                },
                String::new(),
            )
        } else if worker.is_some_and(|w| w.status == Status::Failed) {
            ("Worker stopped", String::new())
        } else if run.status == "done" {
            ("Done", String::new())
        } else if !dependencies.is_empty() {
            (
                "Waiting",
                format!(
                    "Waiting for task{} {}.",
                    if dependencies.len() == 1 { "" } else { "s" },
                    dependencies.join(", ")
                ),
            )
        } else {
            match run.status.as_str() {
                "waiting" => ("Waiting for a reply", run.summary.clone()),
                "repair_wait" => (
                    "Waiting for repair",
                    run.repair
                        .as_ref()
                        .map(|repair| format!("Waiting for {} to be repaired.", repair.task))
                        .unwrap_or_default(),
                ),
                "blocked" | "repair_paused" => ("Blocked", run.summary.clone()),
                "pending" if self.executing_mods.contains(&m.id) => {
                    ("Waiting for a worker", String::new())
                }
                _ => ("Paused", run.summary.clone()),
            }
        };
        if checks.is_empty() && !run.verification_feedback.is_empty() {
            checks = run.verification_feedback.iter().collect();
            checks_label = "Previous failed checks";
        }
        Some(TaskInspection {
            number: index + 1,
            task,
            run,
            identity,
            state,
            note,
            error,
            latest,
            checks,
            checks_label,
        })
    }

    pub fn failed_task(&self) -> Option<i64> {
        let m = self.current_mod()?;
        let execution = m.execution.as_ref()?;
        execution
            .checks
            .iter()
            .filter(|check| check.failed())
            .find_map(|check| check.task)
            .filter(|id| execution.tasks.iter().any(|run| run.id == *id))
            .or_else(|| {
                self.task_ids().into_iter().find(|id| {
                    self.inspect_task(*id).is_some_and(|task| {
                        task.error.is_some()
                            || task.run.checks.iter().any(|check| check.failed())
                            || matches!(
                                task.run.status.as_str(),
                                "paused" | "blocked" | "repair_paused"
                            )
                    })
                })
            })
    }

    pub(super) fn tasks_key(&mut self, key: KeyEvent, selected: usize) -> rusqlite::Result<()> {
        let ids = self.task_ids();
        let selected = selected.min(ids.len().saturating_sub(1));
        match key.code {
            KeyCode::Esc => self.view = View::Chat,
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.view = View::Chat
            }
            KeyCode::Up => self.view = View::Tasks(selected.saturating_sub(1)),
            KeyCode::Down => {
                self.view = View::Tasks((selected + 1).min(ids.len().saturating_sub(1)))
            }
            KeyCode::PageUp => {
                self.view = View::Tasks(selected.saturating_sub(self.page_size as usize))
            }
            KeyCode::PageDown => {
                self.view = View::Tasks(
                    (selected + self.page_size as usize).min(ids.len().saturating_sub(1)),
                )
            }
            KeyCode::Enter => {
                if let Some(id) = ids.get(selected) {
                    self.view = View::Task(*id, 0);
                }
            }
            KeyCode::Char('r') if key.modifiers.is_empty() => {
                if let Some(id) = ids.get(selected) {
                    self.retry_task(*id)?;
                }
            }
            KeyCode::Char('h') if key.modifiers.is_empty() => {
                self.history_origin = Some(View::Tasks(selected));
                self.view = View::History(0);
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn task_key(
        &mut self,
        key: KeyEvent,
        id: i64,
        scroll: u16,
        history: bool,
    ) -> rusqlite::Result<()> {
        let view = |offset| {
            if history {
                View::TaskHistory(id, offset)
            } else {
                View::Task(id, offset)
            }
        };
        match key.code {
            KeyCode::Esc => {
                self.view = if history {
                    View::Task(id, 0)
                } else {
                    View::Tasks(
                        self.task_ids()
                            .iter()
                            .position(|task| *task == id)
                            .unwrap_or(0),
                    )
                }
            }
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.view = View::Chat
            }
            KeyCode::Up => self.view = view(scroll.saturating_sub(1)),
            KeyCode::Down => self.view = view(scroll.saturating_add(1)),
            KeyCode::PageUp => self.view = view(scroll.saturating_sub(self.page_size)),
            KeyCode::PageDown => self.view = view(scroll.saturating_add(self.page_size)),
            KeyCode::Char('r')
                if !history
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.can_retry_task(id) =>
            {
                self.retry_task(id)?;
            }
            KeyCode::Char('r' | 'n')
                if !history && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let action = self
                    .action_dock()
                    .actions
                    .into_iter()
                    .find(|item| {
                        matches!(
                            item.action,
                            Action::Run | Action::Retry | Action::RetryGit | Action::Network
                        ) && Some(key.code) == item.action.shortcut().map(KeyCode::Char)
                    })
                    .map(|item| item.action);
                if let Some(action) = action {
                    self.view = View::Chat;
                    self.perform_action(action)?;
                }
            }
            KeyCode::Char('a') if history && key.modifiers.is_empty() => {
                self.history_origin = Some(View::TaskHistory(id, scroll));
                self.view = View::History(0);
            }
            KeyCode::Char('h') if !history && key.modifiers.is_empty() => {
                self.view = View::TaskHistory(id, 0)
            }
            _ => {}
        }
        Ok(())
    }
}
