use std::{
    collections::BTreeSet,
    path::{Component, Path},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Role {
    Planner,
    Executor,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Self::Planner => "planner",
            Self::Executor => "executor",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub summary: String,
    #[serde(default)]
    pub contracts: Vec<String>,
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub non_goals: Vec<String>,
    pub tasks: Vec<Task>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub outcome: String,
    pub files: Vec<String>,
    pub depends_on: Vec<String>,
    pub worker: String,
    pub checks: Vec<String>,
}

#[derive(Clone)]
pub struct Planning {
    pub status: String,
    pub source: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub routing: Option<String>,
    pub plan: Option<Plan>,
}

impl Plan {
    pub fn parse(text: &str) -> Result<Self, String> {
        let plan: Self = serde_json::from_str(text).map_err(|e| format!("Invalid plan: {e}"))?;
        plan.validate()?;
        Ok(plan)
    }

    pub fn validate(&self) -> Result<(), String> {
        for notes in [&self.contracts, &self.assumptions, &self.non_goals] {
            if notes.len() > 12
                || notes
                    .iter()
                    .any(|note| note.trim().is_empty() || note.len() > 4000)
            {
                return Err(
                    "Plan contracts, assumptions and exclusions must be brief and nonempty.".into(),
                );
            }
        }
        if self.summary.trim().is_empty() || self.tasks.is_empty() || self.tasks.len() > 32 {
            return Err("A plan needs a summary and 1–32 tasks.".into());
        }
        let ids = self
            .tasks
            .iter()
            .map(|task| task.id.as_str())
            .collect::<BTreeSet<_>>();
        if ids.len() != self.tasks.len() {
            return Err("Plan task IDs must be unique.".into());
        }
        for task in &self.tasks {
            if task.id.trim().is_empty()
                || task.title.trim().is_empty()
                || task.outcome.trim().is_empty()
                || task.worker != "codex"
                || task.checks.is_empty()
                || task.checks.iter().any(|check| check.trim().is_empty())
            {
                return Err(format!(
                    "Task {} needs an outcome, a connected worker and completion checks.",
                    task.id
                ));
            }
            if task
                .depends_on
                .iter()
                .any(|id| id == &task.id || !ids.contains(id.as_str()))
            {
                return Err(format!("Task {} has an invalid dependency.", task.id));
            }
            for file in &task.files {
                if file.trim().is_empty()
                    || file.contains('\\')
                    || file.contains(['*', '?', '[', ']'])
                    || Path::new(file)
                        .components()
                        .any(|part| !matches!(part, Component::Normal(_)))
                {
                    return Err(format!(
                        "Task {} has invalid file scope {file:?}; use an exact project-relative file or directory path.",
                        task.id
                    ));
                }
            }
        }
        let mut finished = BTreeSet::new();
        loop {
            let previous = finished.len();
            for task in &self.tasks {
                if task
                    .depends_on
                    .iter()
                    .all(|id| finished.contains(id.as_str()))
                {
                    finished.insert(task.id.as_str());
                }
            }
            if finished.len() == self.tasks.len() {
                break;
            }
            if previous == finished.len() {
                return Err("Plan dependencies contain a cycle.".into());
            }
        }
        for (index, first) in self.tasks.iter().enumerate() {
            for second in &self.tasks[index + 1..] {
                let overlap = first.files.iter().any(|a| {
                    second
                        .files
                        .iter()
                        .any(|b| Path::new(a).starts_with(b) || Path::new(b).starts_with(a))
                });
                if overlap
                    && !self.depends_on(first, &second.id)
                    && !self.depends_on(second, &first.id)
                {
                    return Err(format!(
                        "Tasks {} and {} share files; add a dependency.",
                        first.id, second.id
                    ));
                }
            }
        }
        Ok(())
    }

    fn depends_on(&self, task: &Task, target: &str) -> bool {
        task.depends_on.iter().any(|id| {
            id == target
                || self.depends_on(
                    self.tasks.iter().find(|task| &task.id == id).unwrap(),
                    target,
                )
        })
    }

    pub fn display(&self) -> String {
        let mut lines = vec![format!("plan · {}", self.summary)];
        for task in &self.tasks {
            lines.push(format!("{}. {} · {}", task.id, task.title, task.worker));
            lines.push(format!("   {}", task.outcome));
            if !task.files.is_empty() {
                lines.push(format!("   files: {}", task.files.join(", ")));
            }
            if !task.depends_on.is_empty() {
                lines.push(format!("   after: {}", task.depends_on.join(", ")));
            }
            lines.push(format!("   check: {}", task.checks.join("; ")));
        }
        lines.join("\n")
    }
}

pub fn schema() -> Value {
    let strings = json!({"type":"array", "items":{"type":"string"}});
    json!({"type":"object", "additionalProperties":false,
        "properties":{"summary":{"type":"string"}, "contracts":strings, "assumptions":strings, "non_goals":strings, "tasks":{"type":"array", "items":{
            "type":"object", "additionalProperties":false,
            "properties":{"id":{"type":"string"}, "title":{"type":"string"},
                "outcome":{"type":"string"}, "files":strings, "depends_on":strings,
                "worker":{"type":"string", "enum":["codex"]}, "checks":strings},
            "required":["id","title","outcome","files","depends_on","worker","checks"]}}},
        "required":["summary","contracts","assumptions","non_goals","tasks"]})
}

pub fn instructions(role: Role, description: &str, plan: Option<&Plan>, writable: bool) -> String {
    let boundary = if writable {
        "All tools execute in this codemod's Linux VM. Each assigned task has its own worktree and working directory; use the path in the task instruction. /workspace holds combined source and is read-only to task workers. Another executor may be active; use dynamically allocated loopback ports and your own runtime home. For browser checks, start and stop the app and browser in the same command so they share loopback. Follow project rules. Prepare compatible runtimes and install project dependencies inside the VM; mise is available for user-installed runtimes. Keep runtime installs in your assigned HOME (never hard-code /home/sprowt) and project dependencies in the assigned task worktree. System tools are read-only to normal commands. For missing OS libraries or tools, use install_system_packages with Debian 12 package names and a short reason. Choose only packages required by project manifests or failed checks; for Playwright inspect its installed native dependency list. The harness installs from signed official Debian repositories inside this same VM, then you continue the task. Do not run apt directly, use custom repositories or request broader permissions. Downloads use the harness domain allowlist; report blocked sources clearly. Do not access credentials, use host or external tools, commit, push, or apply changes to the original project."
    } else {
        "You are a read-only worker. Do not change files, request broader permissions, access credentials, or use external tools."
    };
    match role {
        Role::Planner => format!(
            "{boundary} Your role is planner. Inspect relevant source, manifests, docs, tests and project rules before planning. The codemod description is the user's request. Produce a concise task plan matching the output schema. Define shared contracts (interfaces, data shapes and error behavior) before dividing work. Record only material assumptions and explicit non-goals; use empty arrays when unnecessary. Give tasks short outcomes, exact project-relative file or directory paths (no globs), dependencies and 1–3 brief completion checks. Only Codex is connected; assign every task to codex. Up to two independent tasks run in parallel. Separate tasks with disjoint ownership can build against a defined contract concurrently; add dependencies only for genuine implementation prerequisites or shared files. Do not split small tasks just to use both workers. Add combined regression verification when independently changed components interact. Include only requested work. Keep the plan small. Do not implement it. Codemod: {description}"
        ),
        Role::Executor => format!(
            "{boundary} Your role is executor. Execute the task the harness assigns, or answer a queued instruction. Keep commentary short. Use the codemod goal and saved plan as context. Codemod: {description}\nPlan: {}",
            plan.map_or_else(
                || "none".into(),
                |plan| serde_json::to_string(plan).unwrap()
            )
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> Plan {
        Plan {
            summary: "Add chart".into(),
            tasks: vec![Task {
                id: "1".into(),
                title: "Chart".into(),
                outcome: "Display performance".into(),
                files: vec!["src/chart.rs".into()],
                depends_on: vec![],
                worker: "codex".into(),
                checks: vec!["Run chart tests".into()],
            }],
            ..Plan::default()
        }
    }

    #[test]
    fn rejects_bad_dependencies_paths_workers_and_parallel_file_conflicts() {
        let mut p = plan();
        assert!(p.validate().is_ok());
        let mut other = p.tasks[0].clone();
        other.id = "2".into();
        p.tasks.push(other);
        assert!(p.validate().is_err());
        p.tasks[1].depends_on = vec!["1".into()];
        assert!(p.validate().is_ok());
        p.tasks[0].depends_on = vec!["2".into()];
        assert!(p.validate().is_err());
        p.tasks[0].depends_on = vec!["missing".into()];
        assert!(p.validate().is_err());
        p.tasks[0].depends_on.clear();
        p.tasks[0].files = vec!["../secrets".into()];
        assert!(p.validate().is_err());
        p.tasks[0].files = vec!["/tmp/file".into()];
        assert!(p.validate().is_err());
        p.tasks[0].files = vec!["src/**".into()];
        assert!(p.validate().is_err());
        p.tasks[0].files = vec!["src".into()];
        assert!(p.validate().is_ok());
        p.tasks[1].depends_on.clear();
        assert!(p.validate().is_err());
        p.tasks.pop();
        p.tasks[0].worker = "muse".into();
        assert!(p.validate().is_err());
    }
}
