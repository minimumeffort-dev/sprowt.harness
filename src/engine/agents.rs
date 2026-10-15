use super::{Engine, snapshot::WorkerSummary};
use crate::{store::CodeMod, worker::Status};
use serde::Serialize;

#[derive(Serialize)]
pub struct Agents {
    pub summary: String,
    pub workers: Vec<Agent>,
    pub timings: Vec<Timing>,
}
#[derive(Serialize)]
pub struct Agent {
    pub worker: WorkerSummary,
    pub task_number: Option<usize>,
    pub title: String,
    pub state: &'static str,
    pub tone: &'static str,
    pub activity: String,
    pub activity_preview: String,
    pub worker_label: String,
    pub model: String,
    pub reasoning: String,
    pub output: String,
    pub previous_diagnostic: Option<String>,
    pub failure: Option<Failure>,
}
#[derive(Serialize)]
pub struct Failure {
    pub summary: String,
    pub detail: String,
}
#[derive(Serialize)]
pub struct Timing {
    pub label: &'static str,
    pub duration_ms: u64,
    pub duration: String,
    pub relative_duration: f64,
}

impl Agents {
    pub fn owns_error(&self, error: &str) -> bool {
        self.workers.iter().any(|agent| {
            agent
                .failure
                .as_ref()
                .is_some_and(|failure| failure.detail == error)
        })
    }
}

impl Engine {
    pub fn agents(&self) -> Option<Agents> {
        let m = self.current_mod()?;
        let mut workers: Vec<_> = self
            .worker_summaries(m)
            .into_iter()
            .map(|summary| {
                let worker = &self.workers[&summary.id];
                let run = m.execution.as_ref().and_then(|execution| {
                    execution
                        .tasks
                        .iter()
                        .find(|run| {
                            run.worker == Some(worker.id) && Some(run.id) == worker.task_run_id()
                        })
                        .or_else(|| {
                            execution
                                .tasks
                                .iter()
                                .find(|run| run.worker == Some(worker.id) && run.status != "done")
                        })
                });
                let task = run.and_then(|run| self.inspect_task(run.id));
                let failure = if worker.busy() || m.user_paused {
                    None
                } else if let Some(task) = &task {
                    if matches!(
                        task.run.status.as_str(),
                        "paused" | "blocked" | "repair_paused"
                    ) && super::tasks::visible_task_error(Some(&task.run.summary), m.user_paused)
                        .is_some()
                    {
                        Some(task.run.summary.clone())
                    } else if matches!(
                        task.check_origin,
                        super::CheckOrigin::Task | super::CheckOrigin::Final
                    ) {
                        task.checks
                            .iter()
                            .find(|check| check.failed())
                            .map(|check| check.output.clone())
                    } else {
                        None
                    }
                } else if worker.status == Status::Failed {
                    worker.error.clone()
                } else {
                    None
                }
                .filter(|error| !error.trim().is_empty())
                .map(|detail| Failure {
                    summary: failure_summary(&detail),
                    detail,
                });
                let (state, tone) = if failure.is_some() {
                    ("Needs attention", "attention")
                } else if m.user_paused && !worker.busy() {
                    ("Paused", "quiet")
                } else {
                    match worker.status {
                        Status::Routing if worker.enabled => ("Assigning", "working"),
                        Status::Routing | Status::Ready => ("Ready", "quiet"),
                        Status::Connecting => ("Connecting", "working"),
                        Status::Starting
                            if worker
                                .progress()
                                .is_some_and(|activity| activity.starts_with("Preflight · ")) =>
                        {
                            ("Preparing", "working")
                        }
                        Status::Starting => ("Starting", "working"),
                        Status::Running => ("Working", "working"),
                        Status::Stopping => ("Stopping", "quiet"),
                        Status::Checking => ("Checking", "working"),
                        Status::Complete => ("Complete", "complete"),
                        Status::Failed => ("Stopped", "quiet"),
                    }
                };
                let activity = if worker.busy() {
                    worker.progress().unwrap_or("").to_owned()
                } else {
                    String::new()
                };
                let previous_diagnostic = (!worker.busy() && failure.is_none())
                    .then(|| worker.error.clone())
                    .flatten()
                    .filter(|error| !error.trim().is_empty());
                let output = if let Some(failure) = &failure {
                    failure.detail.clone()
                } else if let Some(diagnostic) = &previous_diagnostic {
                    format!("Previous worker diagnostic\n{diagnostic}")
                } else {
                    activity.clone()
                };
                Agent {
                    task_number: task.as_ref().map(|task| task.number),
                    title: task.as_ref().map_or_else(
                        || format!("{} {}", worker.provider(), worker.role.name()),
                        |task| task.task.title.clone(),
                    ),
                    activity_preview: activity_summary(&activity),
                    activity,
                    worker_label: format!("{} · w{}", provider_label(worker.provider()), worker.id),
                    model: worker
                        .model
                        .clone()
                        .unwrap_or_else(|| "Not reported".into()),
                    reasoning: reasoning_label(worker.effort.as_deref()),
                    output,
                    previous_diagnostic,
                    state,
                    tone,
                    failure,
                    worker: summary,
                }
            })
            .collect();
        workers.sort_by_key(|agent| (agent.task_number.unwrap_or(usize::MAX), agent.worker.id));
        let mut summary = self.state_summary().status;
        let failures = workers
            .iter()
            .filter(|agent| agent.failure.is_some())
            .count();
        let running = self
            .workers
            .values()
            .filter(|worker| worker.mod_id == m.id && worker.busy())
            .count();
        if failures > 0 && running > 0 {
            summary.push_str(&format!(
                " · {running} worker{} running",
                if running == 1 { "" } else { "s" }
            ));
        }
        Some(Agents {
            summary,
            workers,
            timings: timings(m),
        })
    }
}

fn compact(text: &str, limit: usize) -> String {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() <= limit {
        line.into()
    } else {
        format!("{}…", line.chars().take(limit).collect::<String>())
    }
}
fn provider_label(provider: &str) -> &str {
    match provider {
        "codex" => "Codex",
        "muse" => "Muse",
        other => other,
    }
}
fn reasoning_label(effort: Option<&str>) -> String {
    match effort {
        Some("xhigh") => "Extra high",
        Some("high") => "High",
        Some("medium") => "Medium",
        Some("low") => "Low",
        Some("minimal") => "Minimal",
        Some("none") => "None",
        Some("max") => "Maximum",
        Some("ultra") => "Ultra",
        Some(other) => other,
        None => "Not reported",
    }
    .into()
}
fn activity_summary(activity: &str) -> String {
    let summary = if activity.starts_with("Preflight · baseline · ") {
        "Checking the existing baseline"
    } else if activity.starts_with("Preflight · setup · ") {
        "Preparing dependencies"
    } else if activity.starts_with("Preflight · capabilities · ") {
        "Checking environment capabilities"
    } else {
        activity
    };
    compact(summary, 180)
}
fn failure_summary(detail: &str) -> String {
    if detail.contains("Source file exceeds") {
        return if detail.contains("ms-playwright/") {
            "Browser download exceeds the source-file limit.".into()
        } else {
            "A source file exceeds the export limit.".into()
        };
    }
    if let Some(line) = detail
        .lines()
        .rev()
        .find(|line| line.contains("Error:") || line.starts_with("Error "))
    {
        return compact(line, 180);
    }
    compact(detail, 180)
}
fn timings(m: &CodeMod) -> Vec<Timing> {
    let mut totals = std::collections::BTreeMap::new();
    for (_, observation) in &m.diagnostics.timings {
        *totals.entry(observation.phase.label()).or_insert(0_u64) += observation.duration_ms;
    }
    let mut rows: Vec<_> = totals
        .into_iter()
        .map(|(label, duration_ms)| Timing {
            label,
            duration_ms,
            duration: duration(duration_ms),
            relative_duration: 0.0,
        })
        .collect();
    rows.sort_by(|a, b| b.duration_ms.cmp(&a.duration_ms).then(a.label.cmp(b.label)));
    let longest = rows.first().map_or(0, |row| row.duration_ms);
    for row in &mut rows {
        row.relative_duration = if longest == 0 {
            0.0
        } else {
            row.duration_ms as f64 / longest as f64
        };
    }
    rows
}
fn duration(ms: u64) -> String {
    if ms >= 3_600_000 {
        format!(
            "{}h {:02}m {:02}s",
            ms / 3_600_000,
            ms / 60_000 % 60,
            ms / 1_000 % 60
        )
    } else if ms >= 60_000 {
        format!("{}m {:02}s", ms / 60_000, ms / 1_000 % 60)
    } else {
        format!("{:.1}s", ms as f64 / 1_000.0)
    }
}

#[cfg(test)]
pub(crate) mod tests;
