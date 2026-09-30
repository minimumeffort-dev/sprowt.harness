use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::plan::{Plan, Task};

pub struct Execution {
    pub workspace: PathBuf,
    pub backend: String,
    pub status: String,
    pub tasks: Vec<TaskRun>,
    pub checks: Vec<CheckResult>,
    pub fingerprint: Option<String>,
}

#[derive(Clone)]
pub struct TaskRun {
    pub id: i64,
    pub task_id: String,
    pub status: String,
    pub source: String,
    pub turn: Option<String>,
    pub summary: String,
    pub checks: Vec<CheckResult>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct CheckResult {
    pub check: String,
    pub command: Vec<String>,
    pub exit_code: Option<i64>,
    pub output: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub check: String,
    pub command: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub status: String,
    pub summary: String,
    pub checks: Vec<Check>,
}

impl Execution {
    pub fn vm_cleanup_pending(&self) -> bool {
        self.status == "applied"
            && self.backend == "apple-container"
            && self.workspace.join("vm.json").exists()
    }

    pub fn next_task(&self, plan: &Plan) -> Option<&TaskRun> {
        self.tasks.iter().find(|run| {
            run.status == "pending"
                && plan
                    .tasks
                    .iter()
                    .find(|task| task.id == run.task_id)
                    .is_some_and(|task| {
                        task.depends_on.iter().all(|id| {
                            self.tasks
                                .iter()
                                .any(|run| &run.task_id == id && run.status == "done")
                        })
                    })
        })
    }

    pub fn complete(&self) -> bool {
        !self.tasks.is_empty() && self.tasks.iter().all(|task| task.status == "done")
    }
}

impl Report {
    pub fn parse(text: &str, task: &Task) -> Result<Self, String> {
        let report: Self =
            serde_json::from_str(text).map_err(|error| format!("Invalid task report: {error}"))?;
        if report.summary.trim().is_empty()
            || !["completed", "blocked"].contains(&report.status.as_str())
        {
            return Err("The task report needs a status and short summary.".into());
        }
        if report.status == "completed"
            && (report.checks.len() != task.checks.len()
                || report
                    .checks
                    .iter()
                    .zip(&task.checks)
                    .any(|(check, expected)| {
                        &check.check != expected
                            || check.command.is_empty()
                            || !std::path::Path::new(&check.command[0]).is_absolute()
                            || check.command.iter().any(|arg| arg.contains('\0'))
                    }))
        {
            return Err(
                "Each completion check needs its own runnable verification command.".into(),
            );
        }
        Ok(report)
    }
}

pub fn task_prompt(plan: &Plan, task: &Task) -> String {
    format!(
        "Execute only this task from the saved plan in the Linux VM at /workspace. Respect project rules and the declared file scope. Inspect project manifests, choose compatible runtimes and install needed dependencies using mise or the project's package manager. If OS dependencies are missing, call install_system_packages with required Debian package names and a short reason; the harness installs them in the VM and you continue. Run the completion checks; report blocked checks honestly. Return the required JSON report. For each check, provide its exact text and a repeatable command as an argument array, using an absolute guest executable path. The harness reruns these commands independently in the same VM with the same permissions and download allowlist; each has a 30-second limit. Commands must test the result and exit nonzero on failure, without changing source files. Keep the summary short.\nPlan: {}\nCurrent task: {}",
        serde_json::to_string(plan).unwrap(),
        serde_json::to_string(task).unwrap()
    )
}

pub fn schema() -> Value {
    json!({"type":"object","additionalProperties":false,
        "properties":{"status":{"type":"string","enum":["completed","blocked"]},"summary":{"type":"string"},
            "checks":{"type":"array","items":{"type":"object","additionalProperties":false,
                "properties":{"check":{"type":"string"},"command":{"type":"array","items":{"type":"string"}}},"required":["check","command"]}}},
        "required":["status","summary","checks"]})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_requires_every_declared_check_and_absolute_commands() {
        let plan = Plan::parse(r#"{"summary":"Greeting","tasks":[{"id":"one","title":"Greeting","outcome":"Print hello","files":["hello.py"],"depends_on":[],"worker":"codex","checks":["prints hello","empty input rejected"]}]}"#).unwrap();
        let task = &plan.tasks[0];
        let mut report = json!({"status":"completed","summary":"Done","checks":[{"check":"prints hello","command":["/bin/sh","-c","exit 0"]},{"check":"empty input rejected","command":["/bin/sh","-c","exit 0"]}]});
        assert!(Report::parse(&report.to_string(), task).is_ok());
        report["checks"][0]["command"][0] = json!("sh");
        assert!(Report::parse(&report.to_string(), task).is_err());
        report["checks"] = json!([]);
        assert!(Report::parse(&report.to_string(), task).is_err());
        report["status"] = json!("blocked");
        assert!(Report::parse(&report.to_string(), task).is_ok());
    }
}
