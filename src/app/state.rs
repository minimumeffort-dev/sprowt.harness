use super::{Action, App, Tone, View};
use crate::plan::Role;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    New,
    Planning,
    Building,
    Checking,
    Recovering,
    Reviewing,
    Git,
    NeedsYou,
    Paused,
    Ready,
    Published,
    Closed,
}

pub struct Summary {
    pub phase: Phase,
    pub status: String,
    pub detail: String,
    pub evidence: String,
    pub error: Option<String>,
    pub primary: Option<Action>,
}

impl Summary {
    fn new(phase: Phase, status: impl Into<String>, primary: Option<Action>) -> Self {
        Self {
            phase,
            status: status.into(),
            detail: String::new(),
            evidence: String::new(),
            error: None,
            primary,
        }
    }

    pub fn tone(&self) -> Tone {
        match self.phase {
            Phase::Planning
            | Phase::Building
            | Phase::Checking
            | Phase::Recovering
            | Phase::Reviewing
            | Phase::Git => Tone::Busy,
            Phase::NeedsYou => Tone::Attention,
            Phase::Ready | Phase::Published => Tone::Ready,
            _ => Tone::Quiet,
        }
    }
}

impl App {
    pub fn state_summary(&self) -> Summary {
        use Action::*;
        use Phase::*;
        let Some(m) = self
            .current_mod()
            .filter(|_| self.composer_view() != View::NewMod)
        else {
            let mut state = Summary::new(New, "New codemod", None);
            if let Some(activity) = self.project_activity() {
                state = Summary::new(Git, activity, None);
            }
            if let Some(error) = self.action_error() {
                state = Summary::new(NeedsYou, "Project needs attention", Some(Failure));
                state.error = Some(error.into());
            }
            return state;
        };
        let active: Vec<_> = self
            .workers
            .values()
            .filter(|w| w.mod_id == m.id && w.busy())
            .collect();
        let running = active.len();
        let execution = m.execution.as_ref();
        let final_failed = execution
            .is_some_and(|e| e.complete() && e.status == "blocked" && !e.checks.is_empty());
        let failed = self.failed_task().filter(|id| {
            !self
                .workers
                .values()
                .any(|w| w.task_run_id() == Some(*id) && w.busy())
        });
        let review = m
            .agent_review
            .as_ref()
            .filter(|r| r.status != "stale" && r.current(m));
        let target = self.targets.get(&m.id);
        let git = self
            .git_activity()
            .filter(|activity| *activity != "checking target branch");
        let changed = target.filter(|t| {
            t.pr_state.as_deref().is_none_or(|s| s == "OPEN")
                && self.git_states.get(&m.id).is_some_and(|s| s.base != t.head)
        });
        let mut state = if m.closed {
            Summary::new(Closed, "Closed · work saved", Some(Reopen))
        } else if m.question().is_some() {
            Summary::new(NeedsYou, "Needs your answer", None)
        } else if let Some(request) = self.pending_network() {
            let task = self
                .inspect_task(request.task)
                .map_or(String::new(), |t| format!(" · task {}", t.number));
            Summary::new(
                NeedsYou,
                format!("Needs network access{task}"),
                Some(Network),
            )
        } else if let Some(activity) = git {
            Summary::new(Git, activity, None)
        } else if self.git_retry_pending() {
            Summary::new(NeedsYou, "Git operation needs attention", Some(RetryGit))
        } else if m.user_paused {
            Summary::new(
                Paused,
                if running > 0 {
                    "Pausing · saving work"
                } else {
                    "Paused by you"
                },
                (running == 0).then_some(
                    if execution.is_some_and(|e| e.complete() && e.status == "review") {
                        Review
                    } else {
                        Run
                    },
                ),
            )
        } else if let Some(id) = failed.filter(|_| !final_failed) {
            let task = self.inspect_task(id);
            let number = task.as_ref().map_or(0, |t| t.number);
            let mut summary = Summary::new(
                NeedsYou,
                format!("Task {number} needs attention"),
                Some(if self.can_retry_task(id) {
                    RetryTask(id)
                } else {
                    Failure
                }),
            );
            summary.error = task.and_then(|t| {
                t.error
                    .map(str::to_owned)
                    .or_else(|| t.checks.iter().find(|c| c.failed()).map(|c| c.brief()))
            });
            summary
        } else if running > 0
            || self.auto_plans.contains(&m.id)
            || self.auto_runs.contains(&m.id)
            || self.executing_mods.contains(&m.id)
        {
            if active.iter().any(|w| w.role == Role::Planner) || self.auto_plans.contains(&m.id) {
                Summary::new(Planning, "Working · planning", None)
            } else if active.iter().any(|w| w.role == Role::Reviewer) {
                Summary::new(Reviewing, "Working · reviewing changes", None)
            } else if execution.is_some_and(|e| e.status == "verifying") {
                Summary::new(Checking, "Working · combined checks", None)
            } else if let Some((index, _)) = execution.and_then(|e| {
                e.tasks.iter().enumerate().find(|(_, t)| {
                    t.conflict.is_some()
                        && ["pending", "sending", "running", "checking"]
                            .contains(&t.status.as_str())
                })
            }) {
                Summary::new(
                    Recovering,
                    format!("Working · resolving task {} conflicts", index + 1),
                    None,
                )
            } else if m
                .agent_review
                .as_ref()
                .is_some_and(|r| r.status == "fixing")
            {
                Summary::new(Recovering, "Working · fixing review issues", None)
            } else if execution
                .is_some_and(|e| e.tasks.iter().any(|t| t.status != "done" && t.check_repair))
            {
                Summary::new(Recovering, "Working · fixing failed check", None)
            } else if execution.is_some_and(|e| {
                e.tasks
                    .iter()
                    .any(|t| t.status != "done" && t.restoring_runtime())
            }) {
                Summary::new(Recovering, "Working · restoring task environment", None)
            } else if execution
                .is_some_and(|e| e.tasks.len() == 1 && e.tasks[0].task_id == "upstream")
            {
                Summary::new(Checking, "Working · verifying target update", None)
            } else if execution.is_some_and(|e| {
                e.tasks
                    .iter()
                    .any(|t| t.status != "done" && !t.verification_feedback.is_empty())
            }) {
                Summary::new(Recovering, "Working · recovering a failed check", None)
            } else {
                Summary::new(
                    Building,
                    if running == 0 {
                        "Working · preparing workers".into()
                    } else {
                        format!(
                            "Working · {running} worker{}",
                            if running == 1 { "" } else { "s" }
                        )
                    },
                    None,
                )
            }
        } else if let Some(target) =
            target.filter(|t| matches!(t.pr_state.as_deref(), Some("MERGED" | "CLOSED")))
        {
            Summary::new(
                Published,
                if target.pr_state.as_deref() == Some("MERGED") {
                    "PR merged"
                } else {
                    "PR closed"
                },
                Some(NewMod),
            )
        } else if let Some(target) =
            changed.filter(|_| !m.agent_review.as_ref().is_some_and(|r| r.holds_updates(m)))
        {
            Summary::new(
                NeedsYou,
                format!("{} changed · checks needed", target.branch),
                Some(Update),
            )
        } else if final_failed {
            let mut s = Summary::new(
                NeedsYou,
                "Combined checks need attention",
                Some(self.failed_check_owner().map_or(Retry, FixCheck)),
            );
            s.error =
                execution.and_then(|e| e.checks.iter().find(|c| c.failed()).map(|c| c.brief()));
            s
        } else if let Some(r) = review.filter(|r| r.status == "findings") {
            let count = r.report.as_ref().map_or(0, |r| r.findings.len());
            Summary::new(
                NeedsYou,
                format!(
                    "Review · {count} open issue{}",
                    if count == 1 { "" } else { "s" }
                ),
                Some(if r.rounds < 2 { FixIssues } else { Findings }),
            )
        } else if review.is_some_and(|r| matches!(r.status.as_str(), "paused" | "blocked")) {
            Summary::new(NeedsYou, "Review needs attention", Some(Review))
        } else if self.worker_error().is_some() {
            Summary::new(NeedsYou, "Work needs attention", Some(Failure))
        } else if self.published() {
            Summary::new(Published, "PR published", None)
        } else if self.version_ready() {
            if review.is_some_and(|r| r.status == "clean") {
                Summary::new(Ready, "Ready to publish", Some(Publish))
            } else if self.auto_reviews.contains(&m.id) {
                Summary::new(Reviewing, "Checks passed · review next", None)
            } else {
                Summary::new(Paused, "Checks passed · review pending", Some(Review))
            }
        } else {
            Summary::new(Paused, "Work saved · ready to resume", Some(Run))
        };
        if state.phase == NeedsYou && running > 0 {
            state.detail = format!(
                "{running} worker{} still running",
                if running == 1 { "" } else { "s" }
            );
        } else if let Some(worker) = active.first() {
            if let Some(e) = execution {
                let done = e.tasks.iter().filter(|t| t.status == "done").count();
                state.detail = format!("{done}/{} tasks finished · ", e.tasks.len());
            }
            if let Some(progress) = worker.progress() {
                state.detail.push_str(progress);
                state.detail.push_str(" · ");
            }
            state.detail.push_str(&worker.elapsed_label());
        } else if let Some(e) = execution {
            let done = e.tasks.iter().filter(|t| t.status == "done").count();
            state.detail = if e.complete() {
                "Implementation finished".into()
            } else {
                format!("{done}/{} tasks finished", e.tasks.len())
            };
        }
        if let Some(r) = m.agent_review.as_ref().filter(|r| r.status == "fixing") {
            let findings = r
                .report
                .as_ref()
                .map_or(&[][..], |report| report.findings.as_slice());
            let checked = findings
                .iter()
                .enumerate()
                .filter(|(i, finding)| {
                    execution.is_some_and(|e| {
                        e.tasks.iter().any(|run| {
                            run.task_id == finding.owner
                                && run.status == "done"
                                && run.checks.iter().any(|c| {
                                    c.check == crate::review::regression_check(*i, finding)
                                        && c.exit_code == Some(0)
                                })
                        })
                    })
                })
                .count();
            state.detail = format!(
                "{checked}/{} issues checked · {}",
                findings.len(),
                state.detail
            );
        }
        if let Some(e) = execution {
            let checks = if e.checks.is_empty() {
                "Combined checks pending".into()
            } else {
                let passed = e.checks.iter().filter(|c| c.exit_code == Some(0)).count();
                let failed = e.checks.iter().filter(|c| c.failed()).count();
                let pending = e.checks.len() - passed - failed;
                if failed > 0 {
                    format!("Checks · {passed} passed · {failed} failed · {pending} not run")
                } else if e.status == "verifying" {
                    format!(
                        "Combined checks · {passed}/{} passed · running",
                        e.checks.len()
                    )
                } else if pending == 0
                    && e.fingerprint.is_some()
                    && e.complete()
                    && e.status == "review"
                {
                    format!("Checks {passed}/{} passed", e.checks.len())
                } else {
                    "Previous checks saved · current version unverified".into()
                }
            };
            let review_label = match review.map(|r| r.status.as_str()) {
                Some("clean") => "Review clear",
                Some("findings") => "Review has findings",
                Some("pending" | "running") => "Review running",
                Some("paused" | "blocked") => "Review incomplete",
                _ if m
                    .agent_review
                    .as_ref()
                    .is_some_and(|r| r.status == "fixing") =>
                {
                    "Review fixes in progress"
                }
                _ if m.agent_review.is_some() => "Review outdated",
                _ => "Review not started",
            };
            state.evidence = format!("{checks} · {review_label}");
        }
        if self.git_state().is_some_and(|s| s.pr.is_some())
            && !self.published()
            && target.is_none_or(|t| t.pr_state.as_deref().is_none_or(|s| s == "OPEN"))
        {
            if !state.evidence.is_empty() {
                state.evidence.push_str(" · ");
            }
            state.evidence.push_str("PR open · update pending");
        }
        if state.phase == NeedsYou && state.error.is_none() {
            state.error = self.worker_error().map(str::to_owned);
        }
        state
    }
}
