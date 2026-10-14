use serde::Serialize;

/// Shared workspace destinations. Selection and keyboard focus belong to each shell.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceView {
    #[default]
    Conversation,
    Changes,
    Checks,
    Findings,
    Agents,
}

impl WorkspaceView {
    pub const ALL: [Self; 5] = [
        Self::Conversation,
        Self::Changes,
        Self::Checks,
        Self::Findings,
        Self::Agents,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Conversation => "Conversation",
            Self::Changes => "Changes",
            Self::Checks => "Checks",
            Self::Findings => "Findings",
            Self::Agents => "Agents",
        }
    }
}

#[derive(serde::Serialize)]
pub struct CheckActivity {
    pub task_id: Option<String>,
    pub label: String,
}

impl super::Engine {
    pub fn workspace_views(&self) -> Vec<WorkspaceView> {
        use WorkspaceView::*;
        let Some(m) = self.current_mod() else {
            return vec![Conversation];
        };
        let changes = self.review.as_ref().map_or_else(
            || {
                m.execution.as_ref().is_some_and(|execution| {
                    std::fs::metadata(execution.workspace.join("review.patch"))
                        .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
                })
            },
            |review| review.count() > 0,
        );
        let checks = !self.check_activity().is_empty()
            || m.execution.as_ref().is_some_and(|execution| {
                !execution.checks.is_empty()
                    || execution.tasks.iter().any(|run| {
                        self.inspect_task(run.id).is_some_and(|task| {
                            !task.checks.is_empty() || self.task_preparation(run.id).is_some()
                        })
                    })
            });
        let findings = m.agent_review.as_ref().is_some_and(|review| {
            !review.issues.is_empty()
                || review
                    .report
                    .as_ref()
                    .is_some_and(|report| !report.findings.is_empty())
        }) || self.targets.get(&m.id).is_some_and(|target| {
            !target.pr_feedback.is_empty() || target.pr_feedback_error.is_some()
        });
        let agents = self.workers.values().any(|worker| worker.mod_id == m.id)
            || !m.diagnostics.timing_packet(None).is_empty();
        [
            (Conversation, true),
            (Changes, changes),
            (Checks, checks),
            (Findings, findings),
            (Agents, agents),
        ]
        .into_iter()
        .filter_map(|(view, available)| available.then_some(view))
        .collect()
    }

    pub(crate) fn check_activity(&self) -> Vec<CheckActivity> {
        let Some(m) = self.current_mod() else {
            return vec![];
        };
        let mut activity = Vec::new();
        if m.execution
            .as_ref()
            .is_some_and(|execution| execution.status == "verifying")
        {
            activity.push(CheckActivity {
                task_id: None,
                label: "Combined checks are running.".into(),
            });
        }
        for worker in self.workers.values().filter(|worker| {
            worker.mod_id == m.id && worker.status == crate::worker::Status::Checking
        }) {
            let task = worker.task_run_id().and_then(|id| self.inspect_task(id));
            let label = task.as_ref().map_or_else(
                || "Worker checks".into(),
                |task| format!("Task {} checks", task.number),
            );
            activity.push(CheckActivity {
                task_id: task.map(|task| task.task.id.clone()),
                label: format!("{label} are running."),
            });
        }
        activity
    }
}
