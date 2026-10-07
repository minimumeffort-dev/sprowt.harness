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
    pub repair: Option<crate::repair::Repair>,
    pub selection: Option<crate::router::Selection>,
    pub worker: Option<i64>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<i64>,
    pub check: String,
    pub command: Vec<String>,
    pub exit_code: Option<i64>,
    pub output: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    #[serde(skip)]
    pub task: Option<i64>,
    pub check: String,
    pub command: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub status: String,
    pub summary: String,
    pub checks: Vec<Check>,
    #[serde(default)]
    pub repair: Option<crate::repair::Repair>,
}

impl Execution {
    pub fn next_task(&self, plan: &Plan, worker: i64) -> Option<&TaskRun> {
        self.tasks.iter().find(|run| {
            run.status == "pending"
                && run.worker.is_none_or(|id| id == worker)
                && run.repair.as_ref().is_none_or(|repair| {
                    run.task_id == repair.task
                        || self
                            .tasks
                            .iter()
                            .any(|owner| owner.task_id == repair.task && owner.status == "done")
                })
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
            || (report.status == "completed" && report.repair.is_some())
        {
            return Err("The task report needs a status and short summary.".into());
        }
        if let Some(repair) = &report.repair {
            repair.validate(task)?;
        }
        if report.status == "completed" {
            if report.checks.len() != task.checks.len() {
                return Err(format!(
                    "Task returned {} completion checks; expected {}.",
                    report.checks.len(),
                    task.checks.len()
                ));
            }
            for (check, expected) in report.checks.iter().zip(&task.checks) {
                if &check.check != expected {
                    return Err(format!(
                        "Completion check must match the task's declared text: {expected}"
                    ));
                }
                if check.command.is_empty()
                    || !std::path::Path::new(&check.command[0]).is_absolute()
                    || check.command.iter().any(|arg| arg.contains('\0'))
                {
                    return Err(format!(
                        "Completion check needs an absolute executable and valid arguments: {expected}"
                    ));
                }
            }
        }
        Ok(report)
    }
}

pub fn task_prompt(plan: &Plan, task: &Task, id: i64) -> String {
    format!(
        "Execute only this task from the saved plan in the Linux VM in your task worktree at /tasks/{id}. Resume saved edits and resolve any merge conflict markers. Respect project rules and the declared file scope. The harness checkpoints source separately from generated caches and runtime databases; scope checks use the source diff, not all files created by tests. Inspect project manifests, choose compatible runtimes and install needed dependencies using mise or the project's package manager. If OS dependencies are missing, call install_system_packages with required Debian package names and a short reason; the harness installs them in the VM and you continue. If a download or documentation request returns x-proxy-error: blocked-by-allowlist, call request_network_access with the exact blocked hostnames and a short reason, including blocked redirect hosts. Return status blocked with repair null while access is pending or denied; do not poll or bypass the policy. The harness reconnects and retries after user approval. Run the completion checks; report blocked checks honestly. If a reproducible code regression belongs to an already completed task, return status blocked and a repair object: task is the completed owner's task ID; files are exact paths in that owner's scope; check must copy the exact text of the failing check from Current task.checks, not from the owner's checks; command reproduces that failure; evidence states the observed failure. Do not fix another task's files. Use repair null for success, missing access, environment blockers, uncertainty or product decisions. The harness reopens the owner and reruns affected tasks, with at most two repair attempts per plan. Return the required JSON report. For each check, provide its exact text and a repeatable command as an argument array, using an absolute guest executable path. The harness reruns these commands independently in the same VM with the same permissions and download allowlist; each has a 30-second limit. Commands must test the result and exit nonzero on failure, without changing source files. Keep the summary short.\nPlan: {}\nCurrent task: {}\nRelevant peers (message their task IDs; read_worker_messages gives live assignments): {}",
        serde_json::to_string(plan).unwrap(),
        serde_json::to_string(task).unwrap(),
        serde_json::to_string(&plan.peers(task)).unwrap()
    )
}

pub fn schema() -> Value {
    json!({"type":"object","additionalProperties":false,
        "properties":{"status":{"type":"string","enum":["completed","blocked"]},"summary":{"type":"string"},
            "repair":{"anyOf":[{"type":"null"},crate::repair::schema()]},
            "checks":{"type":"array","items":{"type":"object","additionalProperties":false,
                "properties":{"check":{"type":"string"},"command":{"type":"array","items":{"type":"string"}}},"required":["check","command"]}}},
        "required":["status","summary","checks","repair"]})
}

pub fn task_schema(context: Option<&Value>) -> Value {
    let mut schema = schema();
    if let Some(checks) = context.and_then(|state| state["task"]["checks"].as_array())
        && !checks.is_empty()
    {
        schema["properties"]["checks"]["items"]["properties"]["check"]["enum"] = json!(checks);
        schema["properties"]["repair"]["anyOf"][1]["properties"]["check"]["enum"] = json!(checks);
    }
    schema
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_checks_keep_task_identity_and_read_legacy_results() {
        let legacy = r#"{"check":"passes","command":["/bin/true"],"exit_code":0,"output":""}"#;
        let mut result: CheckResult = serde_json::from_str(legacy).unwrap();
        assert_eq!(result.task, None);
        result.task = Some(42);
        let restored: CheckResult =
            serde_json::from_str(&serde_json::to_string(&result).unwrap()).unwrap();
        assert_eq!(restored.task, Some(42));
        assert!(
            serde_json::from_str::<Check>(
                r#"{"task":42,"check":"passes","command":["/bin/true"]}"#
            )
            .is_err()
        );
    }

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

    #[test]
    fn repair_reports_keep_original_task_checks_in_the_provider_schema() {
        let state = json!({"task":{"checks":["Original task check"]},"repair":{"check":"Integration regression"}});
        let schema = task_schema(Some(&state));
        assert_eq!(
            schema["properties"]["checks"]["items"]["properties"]["check"]["enum"],
            json!(["Original task check"])
        );
        assert_eq!(
            schema["properties"]["repair"]["anyOf"][1]["properties"]["check"]["enum"],
            json!(["Original task check"])
        );
        assert_eq!(schema["properties"]["repair"]["anyOf"][0]["type"], "null");
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("repair"))
        );
    }
}
