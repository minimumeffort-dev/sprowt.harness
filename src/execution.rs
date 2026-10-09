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
    pub conflict: Option<crate::conflict::MergeConflict>,
    pub conflict_retries: u32,
    pub repair: Option<crate::repair::Repair>,
    pub selection: Option<crate::router::Selection>,
    pub worker: Option<i64>,
    pub provider: Option<String>,
    pub assignment_reason: String,
    pub id: i64,
    pub task_id: String,
    pub status: String,
    pub source: String,
    pub turn: Option<String>,
    pub summary: String,
    pub checks: Vec<CheckResult>,
    pub verification_feedback: Vec<CheckResult>,
    pub review_feedback: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct CheckResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<i64>,
    pub check: String,
    pub command: Vec<String>,
    pub exit_code: Option<i64>,
    pub output: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing_runtime: Option<String>,
}

impl CheckResult {
    pub fn skipped(&self) -> bool {
        self.exit_code.is_none()
            && self.missing_runtime.is_none()
            && (self.output.is_empty() || self.output == "Not run.")
    }

    pub fn failed(&self) -> bool {
        self.exit_code != Some(0) && !self.skipped()
    }

    pub fn evidence(&self) -> String {
        let lines = self
            .output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        let start = lines
            .iter()
            .rposition(|line| {
                line.contains("Error")
                    || line.contains("FAILED")
                    || line.contains("panicked")
                    || line.contains("error:")
            })
            .unwrap_or_else(|| lines.len().saturating_sub(6));
        lines[start..]
            .iter()
            .take(8)
            .copied()
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn brief(&self) -> String {
        let evidence = self.evidence();
        let line = evidence
            .lines()
            .next()
            .filter(|line| !line.trim().is_empty())
            .unwrap_or(&self.check);
        line.chars().take(180).collect()
    }
}

impl TaskRun {
    pub fn restoring_runtime(&self) -> bool {
        self.verification_feedback
            .iter()
            .any(|check| check.missing_runtime.is_some())
    }
}

#[derive(Clone, Deserialize, Serialize)]
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
    pub fn next_task<'a>(&'a self, plan: &'a Plan, worker: i64) -> Option<&'a TaskRun> {
        self.ready_tasks(plan)
            .find(|run| run.worker.is_none_or(|id| id == worker))
    }

    pub fn ready_tasks<'a>(&'a self, plan: &'a Plan) -> impl Iterator<Item = &'a TaskRun> {
        let repairing = self.tasks.iter().any(|run| run.status == "repair_wait");
        self.tasks.iter().filter(move |run| {
            run.status == "pending"
                && (!repairing || run.repair.is_some())
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

    pub fn needs_final_checks(&self) -> bool {
        self.complete() && matches!(self.status.as_str(), "pending" | "ready" | "running")
    }

    pub fn check_count(&self, plan: &Plan) -> usize {
        plan.tasks
            .iter()
            .map(|task| {
                self.tasks
                    .iter()
                    .find(|run| run.task_id == task.id)
                    .map_or(task.checks.len(), |run| {
                        run.checks.len().max(task.checks.len())
                    })
            })
            .sum()
    }
}

impl Report {
    pub fn parse(text: &str, task: &Task) -> Result<Self, String> {
        let mut report: Self =
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
            if report.checks.len() < task.checks.len() {
                return Err(format!(
                    "Task returned {} completion checks; expected {}.",
                    report.checks.len(),
                    task.checks.len()
                ));
            }
            for check in &report.checks {
                if !task.checks.contains(&check.check) {
                    return Err(format!(
                        "Completion check is not declared by this task: {}",
                        check.check
                    ));
                }
                if check.command.is_empty()
                    || !std::path::Path::new(&check.command[0]).is_absolute()
                    || check.command.iter().any(|arg| arg.contains('\0'))
                {
                    return Err(format!(
                        "Completion check needs an absolute executable and valid arguments: {}",
                        check.check
                    ));
                }
            }
            for expected in &task.checks {
                if !report.checks.iter().any(|check| &check.check == expected) {
                    return Err(format!(
                        "Task report is missing the declared check: {expected}"
                    ));
                }
            }
            report
                .checks
                .sort_by_key(|check| task.checks.iter().position(|name| name == &check.check));
        }
        Ok(report)
    }
}

pub fn task_prompt(plan: &Plan, task: &Task, id: i64) -> String {
    format!(
        "Execute only this task from the saved plan in the Linux VM in your task worktree at /tasks/{id}. Resume saved edits and resolve any merge conflict markers. Respect project rules and the declared file scope. The harness checkpoints source separately from generated caches and runtime databases; scope checks use the source diff, not all files created by tests. Inspect project manifests, reuse compatible installed runtimes and dependencies, and install only what is missing using mise or the project's package manager. Keep reusable browser/model downloads in XDG_CACHE_HOME (the retained worker HOME cache), not disposable test fixture directories. If OS dependencies are missing, call install_system_packages with required Debian package names and a short reason; the harness installs them in the VM and you continue. If a download or documentation request returns x-proxy-error: blocked-by-allowlist, call request_network_access with the exact blocked hostnames and a short reason, including blocked redirect hosts. Return status blocked with repair null while access is pending or denied; do not poll or bypass the policy. The harness reconnects and retries after user approval. Run the completion checks; report blocked checks honestly. If a reproducible code regression belongs to an already completed task, return status blocked and a repair object: task is the completed owner's task ID; files are exact paths in that owner's scope; check must copy the exact text of the failing check from Current task.checks, not from the owner's checks; command reproduces that failure; evidence states the observed failure. Do not fix another task's files. Use repair null for success, missing access, environment blockers, uncertainty or product decisions. The harness reopens the owner and reruns affected tasks, with at most two repair attempts per plan. Return the required JSON report. Cover every declared check using its exact text. A check may have several commands; repeat its exact text for each command, and do not add undeclared check names. Provide each repeatable command as an argument array using an absolute guest executable path. The harness reruns these commands independently in the same VM with the same permissions and download allowlist; each has a 30-second limit. Commands must test the result and exit nonzero on failure, without changing source files. Verification starts in /tasks/{id} with a fresh process environment; shell exports, running servers and privileged setup from earlier commands do not carry over. Keep temporary fixtures and module stubs in a unique directory under /tmp or your HOME, recreate them in the check, and clean up processes afterward. Browser URL paths such as /static are not writable filesystem paths: use an HTTP test server or a scoped module loader in Node, never create root-level directories. Run the reported commands with run_task_checks before returning them. Save standalone check scripts through that tool so retries and final verification can reuse them. For asynchronous browser checks, register the matching response wait before the action, await successful completion and assert rendered state. Exercise relevant stale-response and save races with controlled delays or reordered replies, not sleeps. Keep the summary short.\nPlan: {}\nCurrent task: {}\nRelevant peers (message their task IDs; read_worker_messages gives live assignments): {}",
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
    fn completion_groups_multiple_commands_without_losing_declared_coverage() {
        let plan = Plan::parse(r#"{"summary":"Verify model","tasks":[{"id":"integration","title":"Verify","outcome":"Integrated","files":["tests"],"depends_on":[],"worker":"codex","checks":["Real model","Browser flows","Suites"]}]}"#).unwrap();
        let task = &plan.tasks[0];
        let checks = [
            ("Suites", "--suites"),
            ("Real model", "--real"),
            ("Browser flows", "--flows"),
            ("Real model", "--paraphrase"),
            ("Real model", "--no-match"),
        ];
        let mut text = json!({"status":"completed","summary":"Verified","repair":null,
            "checks":checks.iter().map(|(name,flag)| json!({"check":name,"command":["/tasks/55/.venv/bin/python","tests/checks.py",flag]})).collect::<Vec<_>>()});
        let report = Report::parse(&text.to_string(), task).unwrap();
        assert_eq!(report.checks.len(), 5);
        assert_eq!(
            report
                .checks
                .iter()
                .map(|c| c.check.as_str())
                .collect::<Vec<_>>(),
            [
                "Real model",
                "Real model",
                "Real model",
                "Browser flows",
                "Suites"
            ]
        );
        assert_eq!(
            report
                .checks
                .iter()
                .map(|c| c.command[2].as_str())
                .collect::<Vec<_>>(),
            [
                "--real",
                "--paraphrase",
                "--no-match",
                "--flows",
                "--suites"
            ]
        );
        text["checks"][2]["check"] = json!("Real model");
        assert!(
            Report::parse(&text.to_string(), task)
                .err()
                .unwrap()
                .contains("missing the declared check: Browser flows")
        );
        text["checks"][2]["check"] = json!("Unplanned check");
        assert!(
            Report::parse(&text.to_string(), task)
                .err()
                .unwrap()
                .contains("not declared")
        );
        text["checks"][2]["check"] = json!("Browser flows");
        for command in [
            json!([]),
            json!(["python"]),
            json!(["/bin/sh", "bad\u{0}argument"]),
        ] {
            text["checks"][4]["command"] = command;
            assert!(Report::parse(&text.to_string(), task).is_err());
        }
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
