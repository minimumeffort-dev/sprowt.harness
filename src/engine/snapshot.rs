use super::{Engine, View, api::AvailableAction, state::Phase};
use crate::{execution::CheckResult, store::CodeMod, worker::Status};
use serde::Serialize;

#[derive(Serialize)]
pub struct Snapshot {
    pub appearance: crate::appearance::Appearance,
    pub project: String,
    pub project_id: i64,
    pub creation_draft: String,
    pub navigation: super::NavigationSnapshot,
    pub workspace_views: [super::workspace_view::WorkspaceView; 5],
    pub mods: Vec<ModSummary>,
    pub active_mod: Option<ModDetail>,
    pub status: String,
    pub phase: &'static str,
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
    status: String,
    workers: Vec<WorkerSummary>,
}
#[derive(Serialize)]
pub struct WorkerSummary {
    pub(crate) id: i64,
    pub(crate) provider: String,
    pub(crate) identity: String,
    pub(crate) role: &'static str,
    pub(crate) task_id: Option<String>,
    pub(crate) state: &'static str,
    pub(crate) activity: String,
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
    pr_feedback: Vec<Finding>,
    pr_feedback_error: Option<String>,
    workers: Vec<String>,
    worker_details: Vec<WorkerSummary>,
    timings: String,
}
#[derive(Serialize)]
pub struct Message {
    role: String,
    author: String,
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
    run_id: Option<i64>,
    identity: String,
    source: Option<String>,
    error: Option<String>,
    latest: Option<Message>,
    results: Vec<Check>,
    checks_label: &'static str,
    completed: bool,
    preparation: Option<PreparationEvidence>,
    actions: Vec<AvailableAction>,
}
#[derive(Serialize)]
pub struct Check {
    id: String,
    task_id: Option<String>,
    // check, final_check, observed_check, previous_check, preparation phases.
    kind: &'static str,
    label: String,
    status: &'static str,
    output: String,
}
#[derive(Serialize)]
pub struct PreparationEvidence {
    source: String,
    current: bool,
    stopped: bool,
    error: Option<String>,
    results: Vec<Check>,
}
#[derive(Serialize)]
pub struct Finding {
    id: String,
    title: String,
    detail: String,
    status: String,
    imported: bool,
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
    // Read-only evidence the user can click away from; it never blocks the
    // composer, actions or navigation the way a confirmation does.
    pub fn is_inspection(&self) -> bool {
        ["diff", "failure", "sync_details", "findings"].contains(&self.kind)
            && self.confirm_label.is_none()
    }

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
            author: message.author(),
            body: message.body.clone(),
            model: message.model.clone(),
            effort: message.effort.clone(),
        }
    }
}

impl Check {
    fn new(
        id: String,
        task_id: Option<String>,
        kind: &'static str,
        owner: &str,
        result: &CheckResult,
    ) -> Self {
        Self {
            id,
            task_id,
            kind,
            label: format!("{owner} · {}", result.check),
            status: if result.skipped() {
                "skipped"
            } else if result.failed() {
                "failed"
            } else {
                "passed"
            },
            output: format!("{}\n\n{}", result.command.join(" "), result.output),
        }
    }
}

impl Engine {
    pub(crate) fn worker_summaries(&self, code_mod: &CodeMod) -> Vec<WorkerSummary> {
        self.workers
            .values()
            .filter(|worker| worker.mod_id == code_mod.id)
            .map(|worker| WorkerSummary {
                id: worker.id,
                provider: worker.provider().into(),
                identity: worker.label(Some("")).trim().into(),
                role: worker.role.name(),
                task_id: code_mod.execution.as_ref().and_then(|execution| {
                    execution
                        .tasks
                        .iter()
                        .find(|run| Some(run.id) == worker.task_run_id())
                        .map(|run| run.task_id.clone())
                }),
                state: match worker.status {
                    Status::Routing => "routing",
                    Status::Connecting => "connecting",
                    Status::Ready => "ready",
                    Status::Starting => "starting",
                    Status::Running => "running",
                    Status::Stopping => "stopping",
                    Status::Failed => "failed",
                    Status::Complete => "complete",
                    Status::Checking => "checking",
                },
                activity: if worker.busy() {
                    worker
                        .progress()
                        .map_or_else(|| worker.activity(code_mod), Into::into)
                } else {
                    worker.error.clone().unwrap_or_default()
                },
            })
            .collect()
    }

    fn final_check(&self, code_mod: &CodeMod, index: usize, result: &CheckResult) -> Check {
        let execution = code_mod.execution.as_ref().unwrap();
        Check::new(
            format!(
                "final:{}:{index}",
                execution.fingerprint.as_deref().unwrap_or("unverified")
            ),
            result.task.and_then(|id| {
                execution
                    .tasks
                    .iter()
                    .find(|run| run.id == id)
                    .map(|run| run.task_id.clone())
            }),
            "final_check",
            "Final checks",
            result,
        )
    }

    fn snapshot_checks(&self, code_mod: &CodeMod) -> Vec<Check> {
        code_mod
            .execution
            .iter()
            .flat_map(|execution| {
                execution
                    .checks
                    .iter()
                    .enumerate()
                    .map(|(index, result)| self.final_check(code_mod, index, result))
                    .chain(execution.tasks.iter().flat_map(|run| {
                        run.checks.iter().enumerate().map(move |(index, result)| {
                            Check::new(
                                format!("task:{}:{}:check:{index}", run.id, run.source),
                                Some(run.task_id.clone()),
                                "check",
                                &format!("Task {}", run.task_id),
                                result,
                            )
                        })
                    }))
                    .chain(execution.tasks.iter().flat_map(|run| {
                        self.inspect_task(run.id)
                            .filter(|task| task.check_origin == super::CheckOrigin::Observed)
                            .map(|task| self.task_results(code_mod, &task))
                            .unwrap_or_default()
                    }))
            })
            .collect()
    }

    fn task_results(
        &self,
        code_mod: &CodeMod,
        task: &super::tasks::TaskInspection<'_>,
    ) -> Vec<Check> {
        if task.check_origin == super::CheckOrigin::Final {
            return code_mod
                .execution
                .as_ref()
                .unwrap()
                .checks
                .iter()
                .enumerate()
                .filter(|(_, result)| result.task == Some(task.run.id))
                .map(|(index, result)| self.final_check(code_mod, index, result))
                .collect();
        }
        let kind = match task.check_origin {
            super::CheckOrigin::Previous => "previous_check",
            super::CheckOrigin::Observed => "observed_check",
            _ => "check",
        };
        task.checks
            .iter()
            .enumerate()
            .map(|(index, result)| {
                Check::new(
                    format!("task:{}:{}:{kind}:{index}", task.run.id, task.run.source),
                    Some(task.task.id.clone()),
                    kind,
                    &format!("Task {}", task.task.id),
                    result,
                )
            })
            .collect()
    }

    fn preparation_evidence(&self, id: i64) -> Option<PreparationEvidence> {
        let task = self.inspect_task(id)?;
        let preparation = self.task_preparation(id)?;
        let source = &preparation.receipt.source;
        let task_id = &task.task.id;
        let results = [
            ("preparation_setup", "Setup", &preparation.receipt.setup),
            (
                "preparation_capabilities",
                "Capabilities",
                &preparation.receipt.capabilities,
            ),
            (
                "preparation_baseline",
                "Baseline",
                &preparation.receipt.baseline,
            ),
        ]
        .into_iter()
        .flat_map(|(kind, label, results)| {
            results.iter().enumerate().map(move |(index, result)| {
                Check::new(
                    format!("task:{id}:{source}:{kind}:{index}"),
                    Some(task_id.clone()),
                    kind,
                    label,
                    result,
                )
            })
        })
        .collect();
        Some(PreparationEvidence {
            stopped: preparation.stopped(),
            source: preparation.receipt.source,
            current: preparation.current,
            error: preparation.receipt.error,
            results,
        })
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut dock = self.action_dock();
        dock.actions
            .sort_by_key(|item| Some(item.action) != dock.primary);
        let phase = self.state_summary().phase;
        Snapshot {
            appearance: self.appearance,
            workspace_views: super::workspace_view::WorkspaceView::ALL,
            project: self.project.display().to_string(),
            project_id: self.project_id,
            creation_draft: self.creation_draft.clone(),
            navigation: self.navigation_snapshot(),
            mods: self
                .mods
                .iter()
                .map(|m| ModSummary {
                    id: m.id,
                    name: m.name.clone(),
                    closed: m.closed,
                    status: if self.navigation.current() != super::Destination::NewCodemod
                        && self.current_mod().is_some_and(|active| active.id == m.id)
                    {
                        dock.status.clone()
                    } else if m.closed {
                        "Closed · work saved".into()
                    } else if m.user_paused {
                        "Paused by you".into()
                    } else if let Some(execution) = &m.execution {
                        format!("Execution · {}", execution.status)
                    } else if let Some(planning) = &m.planning {
                        format!("Planning · {}", planning.status)
                    } else {
                        "Work saved".into()
                    },
                    workers: self.worker_summaries(m),
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
                    .filter(|v| m.shows_conversation_message(v))
                    .map(Message::from)
                    .chain(
                        m.coordination
                            .iter()
                            .filter(|v| m.needs_answer(v))
                            .map(|v| Message {
                                role: "question".into(),
                                author: "question".into(),
                                body: format!("{}: {}", v.from_task, v.body),
                                model: None,
                                effort: None,
                            }),
                    )
                    .collect(),
                history: m
                    .messages
                    .iter()
                    .filter(|v| !m.shows_conversation_message(v))
                    .map(Message::from)
                    .collect(),
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
                                    .as_ref()
                                    .map(|i| i.note.clone())
                                    .or_else(|| run.map(|r| r.summary.clone()))
                                    .unwrap_or_default(),
                                files: task.files.clone(),
                                checks: task.checks.clone(),
                                run_id: run.map(|r| r.id),
                                identity: inspection
                                    .as_ref()
                                    .map(|i| i.identity.clone())
                                    .unwrap_or_else(|| "Worker not assigned".into()),
                                source: run.map(|r| r.source.clone()),
                                error: inspection.as_ref().and_then(|i| i.error.map(Into::into)),
                                latest: inspection
                                    .as_ref()
                                    .and_then(|i| i.latest.map(Message::from)),
                                results: inspection
                                    .as_ref()
                                    .map(|i| self.task_results(m, i))
                                    .unwrap_or_default(),
                                checks_label: inspection
                                    .as_ref()
                                    .map_or("Checks", |i| i.check_origin.label()),
                                completed: run.is_some_and(|r| r.status == "done"),
                                preparation: run.and_then(|r| self.preparation_evidence(r.id)),
                                actions: run
                                    .into_iter()
                                    .flat_map(|r| self.task_desktop_actions(r.id))
                                    .filter_map(|a| {
                                        a.action.desktop_id().map(|(id, task_id)| AvailableAction {
                                            id,
                                            task_id,
                                            label: a.label,
                                        })
                                    })
                                    .collect(),
                            }
                        })
                        .collect(),
                }),
                checks: self.snapshot_checks(m),
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
                        imported: false,
                    })
                    .collect(),
                pr_feedback: self
                    .targets
                    .get(&m.id)
                    .map(|target| {
                        target
                            .pr_feedback
                            .iter()
                            .map(|item| Finding {
                                id: item.id.clone(),
                                title: item.title.clone(),
                                detail: item.detail.clone(),
                                status: item.status.clone(),
                                imported: m
                                    .agent_review
                                    .as_ref()
                                    .is_some_and(|r| r.pr_feedback_imported(&item.id)),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                pr_feedback_error: self
                    .targets
                    .get(&m.id)
                    .and_then(|target| target.pr_feedback_error.clone()),
                workers: self.worker_activity(),
                worker_details: self.worker_summaries(m),
                timings: m.diagnostics.timing_packet(None),
            }),
            status: dock.status,
            phase: match phase {
                Phase::New => "new",
                Phase::Planning => "planning",
                Phase::Building => "building",
                Phase::Checking => "checking",
                Phase::Recovering => "recovering",
                Phase::Reviewing => "reviewing",
                Phase::Git => "git",
                Phase::NeedsYou => "needs_you",
                Phase::Paused => "paused",
                Phase::Ready => "ready",
                Phase::Published => "published",
                Phase::Closed => "closed",
            },
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
                Dialog::new(
                    "findings",
                    if review.partial() {
                        review.label()
                    } else {
                        "Review findings".into()
                    },
                    body,
                    None,
                )
            }
            View::Publish => {
                let draft = self.publication.as_ref()?;
                Dialog::new(
                    "publication",
                    "Publish pull request",
                    format!(
                        "Commit: {}\n\n{}\n\n{}",
                        draft.text.commit_title,
                        draft.text.pr_title,
                        draft.body()
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
