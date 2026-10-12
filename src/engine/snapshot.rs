use super::{Engine, View, api::AvailableAction};
use serde::Serialize;

#[derive(Serialize)]
pub struct Snapshot {
    pub project: String,
    pub mods: Vec<ModSummary>,
    pub active_mod: Option<ModDetail>,
    pub status: String,
    pub detail: String,
    pub evidence: String,
    pub error: Option<String>,
    pub actions: Vec<AvailableAction>,
    pub dialog: Option<Dialog>,
}
#[derive(Serialize)]
pub struct ModSummary {
    id: i64,
    name: String,
    closed: bool,
}
#[derive(Serialize)]
pub struct ModDetail {
    id: i64,
    name: String,
    description: String,
    closed: bool,
    user_paused: bool,
    draft: String,
    messages: Vec<Message>,
    history: Vec<Message>,
    queue: Vec<Queued>,
    steering: Vec<String>,
    plan: Option<Plan>,
    checks: Vec<Check>,
    findings: Vec<Finding>,
    workers: Vec<String>,
}
#[derive(Serialize)]
pub struct Message {
    role: String,
    body: String,
    model: Option<String>,
    effort: Option<String>,
}
#[derive(Serialize)]
pub struct Queued {
    id: i64,
    body: String,
}
#[derive(Serialize)]
pub struct Plan {
    status: String,
    summary: String,
    tasks: Vec<Task>,
}
#[derive(Serialize)]
pub struct Task {
    id: String,
    title: String,
    outcome: String,
    depends_on: Vec<String>,
    status: String,
    detail: String,
    files: Vec<String>,
    checks: Vec<String>,
}
#[derive(Serialize)]
pub struct Check {
    label: String,
    status: &'static str,
    output: String,
}
#[derive(Serialize)]
pub struct Finding {
    id: String,
    title: String,
    detail: String,
    status: String,
}
#[derive(Serialize)]
pub struct Dialog {
    pub kind: &'static str,
    title: String,
    body: String,
    pub confirm_label: Option<String>,
    request_id: Option<i64>,
    domains: Vec<String>,
    repository_slug: Option<String>,
    repository_create: bool,
}
impl Dialog {
    fn new(
        kind: &'static str,
        title: impl Into<String>,
        body: impl Into<String>,
        confirm: Option<&str>,
    ) -> Self {
        Self {
            kind,
            title: title.into(),
            body: body.into(),
            confirm_label: confirm.map(Into::into),
            request_id: None,
            domains: vec![],
            repository_slug: None,
            repository_create: false,
        }
    }
}

impl From<&crate::store::Message> for Message {
    fn from(message: &crate::store::Message) -> Self {
        Self {
            role: message.role.clone(),
            body: message.body.clone(),
            model: message.model.clone(),
            effort: message.effort.clone(),
        }
    }
}

impl Engine {
    pub fn snapshot(&self) -> Snapshot {
        let dock = self.action_dock();
        Snapshot {
            project: self.project.display().to_string(),
            mods: self
                .mods
                .iter()
                .map(|m| ModSummary {
                    id: m.id,
                    name: m.name.clone(),
                    closed: m.closed,
                })
                .collect(),
            active_mod: self.current_mod().map(|m| ModDetail {
                id: m.id,
                name: m.name.clone(),
                description: m.description.clone(),
                closed: m.closed,
                user_paused: m.user_paused,
                draft: m.draft.clone(),
                messages: m
                    .messages
                    .iter()
                    .filter(|v| {
                        m.shows_message(v)
                            && !v
                                .item_id
                                .as_deref()
                                .is_some_and(|id| id.starts_with("plan:"))
                    })
                    .map(Message::from)
                    .chain(
                        m.coordination
                            .iter()
                            .filter(|v| m.needs_answer(v))
                            .map(|v| Message {
                                role: "question".into(),
                                body: format!("{}: {}", v.from_task, v.body),
                                model: None,
                                effort: None,
                            }),
                    )
                    .collect(),
                history: m.messages.iter().map(Message::from).collect(),
                queue: m
                    .queue
                    .iter()
                    .map(|v| Queued {
                        id: v.id,
                        body: v.body.clone(),
                    })
                    .collect(),
                steering: m.steering.clone(),
                plan: m.planning.as_ref().map(|p| Plan {
                    status: p.status.clone(),
                    summary: p
                        .plan
                        .as_ref()
                        .map(|p| p.summary.clone())
                        .unwrap_or_default(),
                    tasks: p
                        .plan
                        .iter()
                        .flat_map(|p| &p.tasks)
                        .map(|task| {
                            let run = m
                                .execution
                                .as_ref()
                                .and_then(|e| e.tasks.iter().find(|r| r.task_id == task.id));
                            let inspection = run.and_then(|r| self.inspect_task(r.id));
                            Task {
                                id: task.id.clone(),
                                title: task.title.clone(),
                                outcome: task.outcome.clone(),
                                depends_on: task.depends_on.clone(),
                                status: inspection
                                    .as_ref()
                                    .map(|i| i.state.into())
                                    .or_else(|| run.map(|r| r.status.clone()))
                                    .unwrap_or_else(|| "pending".into()),
                                detail: inspection
                                    .map(|i| i.note)
                                    .or_else(|| run.map(|r| r.summary.clone()))
                                    .unwrap_or_default(),
                                files: task.files.clone(),
                                checks: task.checks.clone(),
                            }
                        })
                        .collect(),
                }),
                checks: m
                    .execution
                    .iter()
                    .flat_map(|e| {
                        e.checks
                            .iter()
                            .map(|c| ("Final checks".to_owned(), c))
                            .chain(e.tasks.iter().flat_map(|run| {
                                run.checks
                                    .iter()
                                    .map(move |c| (format!("Task {}", run.task_id), c))
                            }))
                    })
                    .map(|(owner, c)| Check {
                        label: format!("{owner} · {}", c.check),
                        status: if c.skipped() {
                            "skipped"
                        } else if c.failed() {
                            "failed"
                        } else {
                            "passed"
                        },
                        output: format!("{}\n\n{}", c.command.join(" "), c.output),
                    })
                    .collect(),
                findings: m
                    .agent_review
                    .iter()
                    .flat_map(|r| &r.issues)
                    .map(|f| Finding {
                        id: f.id.to_string(),
                        title: f.finding.title.clone(),
                        detail: format!(
                            "{}:{}\n{}\n{}",
                            f.finding.file, f.finding.line, f.finding.evidence, f.finding.fix
                        ),
                        status: f.status.clone(),
                    })
                    .collect(),
                workers: self.worker_activity(),
            }),
            status: dock.status,
            detail: dock.detail,
            evidence: dock.evidence,
            error: dock.error,
            actions: dock
                .actions
                .into_iter()
                .filter_map(|a| {
                    a.action.desktop_id().map(|(id, task_id)| AvailableAction {
                        id,
                        task_id,
                        label: a.label,
                    })
                })
                .collect(),
            dialog: self.dialog(),
        }
    }

    pub(crate) fn dialog(&self) -> Option<Dialog> {
        Some(match self.view {
            View::ProjectSetup(saved, _) => Dialog::new(
                "project_setup",
                if saved {
                    "Adopt saved codemod"
                } else {
                    "Set up Git"
                },
                format!(
                    "Create a local Git baseline from these files before continuing:\n\n{}",
                    self.setup_files.join("\n")
                ),
                Some("Continue"),
            ),
            View::CloseMod(index) => Dialog::new(
                "close",
                "Close codemod",
                format!(
                    "Close “{}” and retain its saved history and work?",
                    self.mods[index].name
                ),
                Some("Close codemod"),
            ),
            View::DeleteMod(index) => Dialog::new(
                "delete",
                "Delete codemod",
                format!(
                    "Delete “{}”, its conversation, queued instructions and local working folder?",
                    self.mods[index].name
                ),
                Some("Delete codemod"),
            ),
            View::Network(id, _) => {
                let request = self
                    .network_requests
                    .iter()
                    .find(|r| r.id == id && Some(r.mod_id) == self.current_mod().map(|m| m.id))?;
                let mut dialog =
                    Dialog::new("network", "Review network access", &request.reason, None);
                dialog.request_id = Some(id);
                dialog.domains = request.domains.clone();
                dialog
            }
            View::Failure(_) => Dialog::new(
                "failure",
                "Failure details",
                self.action_error().unwrap_or("No current error."),
                None,
            ),
            View::Task(id, _) => {
                let task = self.inspect_task(id)?;
                Dialog::new(
                    "failure",
                    format!("Task {} · {}", task.number, task.task.title),
                    format!(
                        "{}\n{}\n\n{}",
                        task.note,
                        task.error.unwrap_or(""),
                        task.checks
                            .iter()
                            .map(|c| format!("{}\n{}\n{}", c.check, c.command.join(" "), c.output))
                            .collect::<Vec<_>>()
                            .join("\n\n")
                    ),
                    None,
                )
            }
            View::Sync(_) => Dialog::new(
                "sync_details",
                "Sync details",
                self.sync_errors()
                    .into_iter()
                    .map(|(name, error)| format!("{name}\n{error}"))
                    .collect::<Vec<_>>()
                    .join("\n\n"),
                None,
            ),
            View::Review(_) => {
                let review = self.review.as_ref()?;
                Dialog::new(
                    "diff",
                    "Review changes",
                    format!("{}\n\n{}", review.summary, review.patch),
                    None,
                )
            }
            View::Findings(_) => {
                let review = self.current_mod()?.agent_review.as_ref()?;
                let body = review
                    .report
                    .as_ref()
                    .map(|r| r.summary.clone())
                    .unwrap_or_else(|| review.progress());
                Dialog::new("findings", "Review findings", body, None)
            }
            View::Publish => {
                let draft = self.publication.as_ref()?;
                Dialog::new(
                    "publication",
                    "Publish pull request",
                    format!(
                        "Commit: {}\n\n{}\n\n{}\n\n{}",
                        draft.text.commit_title,
                        draft.text.pr_title,
                        draft.text.summary,
                        draft.evidence
                    ),
                    self.can_publish().then_some("Publish PR"),
                )
            }
            View::Repository(create) | View::ConfirmRepository(create) => {
                let confirming = matches!(self.view, View::ConfirmRepository(_));
                let title = if create {
                    "Create GitHub repository"
                } else {
                    "Connect GitHub repository"
                };
                let mut dialog = Dialog::new(
                    if confirming {
                        "repository_confirmation"
                    } else {
                        "repository"
                    },
                    title,
                    if confirming {
                        format!("{title}: {}", self.composer)
                    } else {
                        "Enter owner/repository.".into()
                    },
                    Some(if !confirming {
                        "Continue"
                    } else if create {
                        "Create repository"
                    } else {
                        "Connect repository"
                    }),
                );
                dialog.repository_slug = Some(self.composer.clone());
                dialog.repository_create = create;
                dialog
            }
            _ => return None,
        })
    }
}
