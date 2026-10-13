use serde::{Deserialize, Serialize};

use super::{Action, ActionItem, Engine, View};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: u64,
    pub command: Command,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Snapshot {},
    BeginCreation {},
    GoBack {},
    GoForward {},
    SelectMod {
        mod_id: i64,
    },
    NewMod {
        description: String,
    },
    SetDraft {
        mod_id: i64,
        text: String,
    },
    Submit {
        mod_id: i64,
        text: String,
    },
    Action {
        mod_id: Option<i64>,
        action: String,
        task_id: Option<i64>,
    },
    EditQueue {
        mod_id: i64,
        message_id: i64,
        text: String,
    },
    RemoveQueue {
        mod_id: i64,
        message_id: i64,
    },
    MoveQueue {
        mod_id: i64,
        message_id: i64,
        direction: Direction,
    },
    SteerQueue {
        mod_id: i64,
        message_ids: Vec<i64>,
    },
    FixPrFeedback {
        mod_id: i64,
        feedback_ids: Vec<String>,
    },
    Confirm {
        mod_id: Option<i64>,
        kind: String,
    },
    Cancel {},
    NetworkDecision {
        mod_id: i64,
        request_id: i64,
        allow: bool,
    },
    Repository {
        mod_id: i64,
        slug: String,
        create: bool,
    },
    Shutdown {},
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Up,
    Down,
}

#[derive(Serialize)]
pub struct Response {
    pub protocol_version: u32,
    pub request_id: Option<u64>,
    pub error: Option<String>,
    pub snapshot: super::snapshot::Snapshot,
}

#[derive(Serialize)]
pub struct AvailableAction {
    pub id: &'static str,
    pub label: String,
    pub task_id: Option<i64>,
}

impl Action {
    pub fn desktop_id(self) -> Option<(&'static str, Option<i64>)> {
        Some(match self {
            Self::Run => ("run", None),
            Self::Stop => ("stop", None),
            Self::Retry => ("retry", None),
            Self::RetryGit => ("retry_git", None),
            Self::RetryTask(id) => ("retry_task", Some(id)),
            Self::FixCheck(id) => ("fix_check", Some(id)),
            Self::Reopen => ("reopen", None),
            Self::Network => ("network", None),
            Self::Failure => ("failure", None),
            Self::SyncDetails => ("sync_details", None),
            Self::Diff => ("diff", None),
            Self::Review => ("review", None),
            Self::Findings => ("findings", None),
            Self::FixIssues => ("fix_issues", None),
            Self::ReviseRepair => ("revise_repair", None),
            Self::Publish => ("publish", None),
            Self::Update => ("update", None),
            Self::Close => ("close", None),
            Self::Delete => ("delete", None),
            _ => return None,
        })
    }
}

impl Engine {
    pub(crate) fn task_desktop_actions(&self, id: i64) -> Vec<ActionItem> {
        let Some(task) = self.inspect_task(id) else {
            return Vec::new();
        };
        let mut actions = Vec::new();
        if self.can_retry_task(id) {
            actions.push(ActionItem {
                action: Action::RetryTask(id),
                label: format!("Retry task {}", task.number),
            });
        }
        if self.failed_check_owner() == Some(id) {
            actions.push(ActionItem {
                action: Action::FixCheck(id),
                label: "Fix failed check".into(),
            });
        }
        actions
    }

    pub fn response(&self, request_id: Option<u64>, error: Option<String>) -> Response {
        Response {
            protocol_version: 1,
            request_id,
            error,
            snapshot: self.snapshot(),
        }
    }

    pub fn command(&mut self, command: Command) -> Result<(), String> {
        match command {
            Command::Snapshot {} | Command::Shutdown {} => Ok(()),
            Command::Cancel {} => {
                if self.dialog().is_none() {
                    return Err("This dialog is no longer available.".into());
                }
                self.cancel_dialog().map_err(|e| e.to_string())
            }
            Command::BeginCreation {} => self.begin_creation().map_err(|e| e.to_string()),
            Command::GoBack {} => self.navigate(true).map_err(|e| e.to_string()),
            Command::GoForward {} => self.navigate(false).map_err(|e| e.to_string()),
            Command::SelectMod { mod_id } => {
                self.require_no_confirmation()?;
                let index = self
                    .mods
                    .iter()
                    .position(|m| m.id == mod_id)
                    .ok_or("Codemod no longer exists.")?;
                self.select_mod(index).map_err(|e| e.to_string())
            }
            Command::NewMod { description } => {
                self.require_no_confirmation()?;
                if description.trim().is_empty() {
                    return Err("Describe the codemod first.".into());
                }
                self.begin_creation().map_err(|e| e.to_string())?;
                self.composer = description;
                self.create_mod().map_err(|e| e.to_string())
            }
            Command::SetDraft { mod_id, text } => {
                let index = self.require_mod(Some(mod_id))?;
                if self.mods[index].closed {
                    return Err("Reopen the codemod before editing.".into());
                }
                self.store
                    .save_draft(mod_id, &text)
                    .map_err(|e| e.to_string())?;
                self.mods[index].draft = text.clone();
                if self.view == View::Chat {
                    self.composer = text;
                }
                Ok(())
            }
            Command::Submit { mod_id, text } => {
                self.require_mod(Some(mod_id))?;
                self.require_no_confirmation()?;
                if text.trim().is_empty() {
                    return Err("Write an instruction first.".into());
                }
                if let Some(error) = self.submission_error(&text) {
                    return Err(error.into());
                }
                self.composer = text;
                self.submit().map_err(|e| e.to_string())?;
                self.save_draft().map_err(|e| e.to_string())
            }
            Command::Action {
                mod_id,
                action,
                task_id,
            } => {
                self.require_context(mod_id)?;
                self.require_no_confirmation()?;
                let selected = self
                    .action_dock()
                    .actions
                    .into_iter()
                    .chain(
                        task_id
                            .into_iter()
                            .flat_map(|id| self.task_desktop_actions(id)),
                    )
                    .find(|a| a.action.desktop_id() == Some((action.as_str(), task_id)))
                    .ok_or("This action is no longer available. Review the updated state.")?;
                self.perform_action(selected.action)
                    .map_err(|e| e.to_string())
            }
            Command::EditQueue {
                mod_id,
                message_id,
                text,
            } => {
                self.require_mod(Some(mod_id))?;
                self.require_no_confirmation()?;
                self.edit_queued(message_id, &text)
                    .map_err(|e| e.to_string())
            }
            Command::RemoveQueue { mod_id, message_id } => {
                self.require_mod(Some(mod_id))?;
                self.require_no_confirmation()?;
                self.remove_queued(message_id).map_err(|e| e.to_string())
            }
            Command::MoveQueue {
                mod_id,
                message_id,
                direction,
            } => {
                self.require_mod(Some(mod_id))?;
                self.require_no_confirmation()?;
                self.move_queued(message_id, matches!(direction, Direction::Up))
                    .map_err(|e| e.to_string())
            }
            Command::SteerQueue {
                mod_id,
                message_ids,
            } => {
                self.require_mod(Some(mod_id))?;
                self.require_no_confirmation()?;
                self.steer_queued(&message_ids, None)
                    .map_err(|e| e.to_string())
            }
            Command::FixPrFeedback {
                mod_id,
                feedback_ids,
            } => {
                self.require_mod(Some(mod_id))?;
                self.require_no_confirmation()?;
                if feedback_ids.is_empty() {
                    return Err("Select PR feedback first.".into());
                }
                if !self
                    .fix_pr_feedback(&feedback_ids)
                    .map_err(|e| e.to_string())?
                {
                    return Err("That PR feedback is no longer available to send to review. It may already be in the review ledger, or the current review is not clean.".into());
                }
                Ok(())
            }
            Command::Confirm { mod_id, kind } => {
                self.require_context(mod_id)?;
                let dialog = self
                    .dialog()
                    .ok_or("This confirmation is no longer open.")?;
                if dialog.kind != kind || dialog.confirm_label.is_none() {
                    return Err("This confirmation is no longer available.".into());
                }
                self.confirm_dialog().map_err(|e| e.to_string())
            }
            Command::NetworkDecision {
                mod_id,
                request_id,
                allow,
            } => {
                self.require_mod(Some(mod_id))?;
                if self.view != View::Network(request_id, 0)
                    || !self
                        .network_requests
                        .iter()
                        .any(|r| r.mod_id == mod_id && r.id == request_id && r.status != "approved")
                {
                    return Err("This network request is no longer open.".into());
                }
                self.decide_network(request_id, allow)
                    .map_err(|e| e.to_string())
            }
            Command::Repository {
                mod_id,
                slug,
                create,
            } => {
                self.require_mod(Some(mod_id))?;
                if !matches!(self.view, View::Repository(_)) {
                    return Err("Repository setup is no longer open.".into());
                }
                if !crate::git_mod::valid_slug(slug.trim()) {
                    return Err("Use owner/repository.".into());
                }
                self.composer = slug.trim().into();
                self.view = View::ConfirmRepository(create);
                Ok(())
            }
        }
    }

    fn require_context(&self, id: Option<i64>) -> Result<(), String> {
        if self.current_mod().map(|m| m.id) == id {
            Ok(())
        } else {
            Err("The selected codemod changed. Review it before continuing.".into())
        }
    }

    fn require_mod(&self, id: Option<i64>) -> Result<usize, String> {
        self.require_context(id)?;
        self.active.ok_or_else(|| "Select a codemod first.".into())
    }

    fn require_no_confirmation(&self) -> Result<(), String> {
        if self.dialog().is_some() {
            Err("Finish or cancel the open dialog first.".into())
        } else {
            Ok(())
        }
    }
}
