use std::{io, path::Path, time::Instant};

use serde_json::Value;

use crate::{
    codex::{Action, Client, Event, Resume},
    execution::{Check, Report},
    plan::{Plan, Role, Task},
    router::Selection,
    store::{CodeMod, Message, Store, Submission, WorkerRecord},
    tools::Context,
    workspace,
};

#[derive(Clone, Copy, PartialEq)]
pub enum Status {
    Routing,
    Connecting,
    Ready,
    Starting,
    Running,
    Stopping,
    Failed,
    Complete,
    Checking,
}

pub struct Worker {
    pub id: i64,
    pub mod_id: i64,
    pub role: Role,
    provider: String,
    muse: bool,
    pub selection: Option<Selection>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub status: Status,
    pub error: Option<String>,
    pub enabled: bool,
    client: Option<Client>,
    thread_id: Option<String>,
    final_plan: Option<String>,
    turn: Option<String>,
    pending: Option<Submission>,
    recovery: Option<String>,
    task_source: Option<String>,
    task_report: Option<String>,
    task_mail: bool,
    task_summary: String,
    writable: bool,
    verification: Vec<Check>,
    verify_before: Option<workspace::Snapshot>,
    preparing: Option<String>,
    activity_started: Instant,
}

impl Worker {
    #[cfg(test)]
    pub fn start(
        project: &Path,
        code_mod: &CodeMod,
        record: WorkerRecord,
        role: Role,
        selection: Option<Selection>,
    ) -> io::Result<Self> {
        Self::start_with_muse(project, code_mod, record, role, selection, false)
    }

    pub fn start_with_muse(
        project: &Path,
        code_mod: &CodeMod,
        record: WorkerRecord,
        role: Role,
        selection: Option<Selection>,
        muse: bool,
    ) -> io::Result<Self> {
        let workspace = code_mod
            .execution
            .as_ref()
            .filter(|execution| execution.status != "applied")
            .map(|execution| execution.workspace.join("work"));
        let mut context = Context::worker(code_mod, record.id, role).with_mailbox(&record.database);
        context.provider = record.provider.clone();
        context.muse = muse;
        let client = if role != Role::Planner {
            Some(Client::start(
                project,
                record
                    .thread_id
                    .clone()
                    .filter(|_| role != Role::Reviewer)
                    .map(|id| Resume {
                        id,
                        accepted_instructions: code_mod
                            .messages
                            .iter()
                            .filter(|message| message.role == "user")
                            .map(|message| message.body.clone())
                            .collect(),
                        restart_if_missing: record.pending.is_none()
                            && code_mod.execution.as_ref().is_none_or(|execution| {
                                !execution.tasks.iter().any(|run| {
                                    run.worker == Some(record.id)
                                        && ["sending", "running", "checking"]
                                            .contains(&run.status.as_str())
                                })
                            }),
                    }),
                role,
                &code_mod.description,
                code_mod.planning.as_ref().and_then(|p| p.plan.as_ref()),
                if role == Role::Reviewer {
                    Some(Selection::reviewer())
                } else {
                    None
                },
                context,
            )?)
        } else {
            None
        };
        Ok(Self {
            id: record.id,
            mod_id: code_mod.id,
            role,
            provider: record.provider,
            muse,
            selection,
            model: None,
            effort: None,
            status: if role == Role::Planner {
                Status::Routing
            } else {
                Status::Connecting
            },
            error: None,
            enabled: true,
            client,
            thread_id: record.thread_id,
            final_plan: None,
            turn: None,
            pending: None,
            recovery: record.pending,
            task_source: None,
            task_report: None,
            task_mail: false,
            task_summary: String::new(),
            writable: workspace.is_some() && role == Role::Executor,
            verification: Vec::new(),
            verify_before: None,
            preparing: None,
            activity_started: Instant::now(),
        })
    }

    pub fn busy(&self) -> bool {
        match self.status {
            Status::Routing => self.enabled,
            Status::Connecting
            | Status::Starting
            | Status::Running
            | Status::Stopping
            | Status::Checking => true,
            _ => false,
        }
    }

    pub fn label(&self, activity: Option<&str>) -> String {
        let glyph = activity.unwrap_or(if self.role == Role::Planner {
            "▤"
        } else {
            "◆"
        });
        format!(
            "{glyph} {} · {}{}{}",
            self.provider,
            if self.role == Role::Executor {
                format!("executor · w{}", self.id)
            } else {
                self.role.name().into()
            },
            self.model
                .as_ref()
                .map_or(String::new(), |model| format!(" · {model}")),
            self.effort
                .as_deref()
                .or_else(|| self.model.as_ref().map(|_| "effort unknown"))
                .map_or(String::new(), |effort| format!(" · {effort}")),
        )
    }

    pub fn toggle(&mut self) {
        if self.enabled {
            self.enabled = false;
            if self.status == Status::Checking
                || (self.role != Role::Planner && self.status == Status::Connecting)
            {
                self.client.as_ref().unwrap().cancel_checks();
            }
            if let Some(turn) = &self.turn {
                match self
                    .client
                    .as_ref()
                    .unwrap()
                    .send(Action::Stop { turn: turn.clone() })
                {
                    Ok(()) => self.status = Status::Stopping,
                    Err(error) => self.fail(error.to_string()),
                }
            }
        } else {
            self.enabled = true;
            self.error = None;
            if self.status == Status::Complete {
                self.status = Status::Ready;
            }
        }
    }

    pub fn poll(
        &mut self,
        store: &mut Store,
        code_mod: &mut CodeMod,
        dispatch: bool,
        project: &Path,
    ) -> rusqlite::Result<()> {
        if self.status == Status::Routing {
            if !self.enabled {
                return Ok(());
            }
            let selection = self.selection.clone().unwrap_or_else(Selection::planner);
            store.planning_model(self.mod_id, &selection)?;
            code_mod.planning = store.planning(self.mod_id)?;
            self.selection = Some(selection.clone());
            let mut context = Context::worker(code_mod, self.id, self.role);
            context.muse = self.muse;
            match Client::start(
                project,
                self.thread_id.clone().map(|id| Resume {
                    id,
                    accepted_instructions: code_mod
                        .messages
                        .iter()
                        .filter(|message| message.role == "user")
                        .map(|message| message.body.clone())
                        .collect(),
                    restart_if_missing: self.recovery.is_none()
                        && code_mod
                            .planning
                            .as_ref()
                            .is_some_and(|plan| plan.status == "pending"),
                }),
                self.role,
                &code_mod.description,
                None,
                Some(selection),
                context,
            ) {
                Ok(client) => {
                    self.client = Some(client);
                    self.status = Status::Connecting;
                }
                Err(error) => self.fail(error.to_string()),
            }
        }
        let events = self
            .client
            .as_ref()
            .map(|client| client.poll().collect::<Vec<_>>())
            .unwrap_or_default();
        for event in events {
            self.receive(event, store, code_mod)?;
        }
        if self.role == Role::Reviewer {
            if dispatch && self.enabled && self.status == Status::Ready && self.pending.is_none() {
                if let Some(input) = store.review_input(code_mod)? {
                    store.pending(self.id, Some(&input.source))?;
                    self.task_source = Some(input.source.clone());
                    self.task_report = None;
                    let action = Action::Run {
                        source: input.source.clone(),
                        text: input.texts.join("\n\n"),
                        routing: code_mod
                            .planning
                            .as_ref()
                            .and_then(|p| p.plan.as_ref())
                            .map(|p| serde_json::to_value(p).unwrap()),
                    };
                    self.pending = Some(input);
                    self.status = Status::Starting;
                    if let Err(error) = self.client.as_ref().unwrap().send(action) {
                        store.review_status(
                            self.mod_id,
                            &self.task_source.clone().unwrap(),
                            "blocked",
                        )?;
                        store.pending(self.id, None)?;
                        code_mod.agent_review = store.review_state(self.mod_id)?;
                        self.fail(error.to_string());
                    }
                } else {
                    if let Some(review) = &code_mod.agent_review {
                        store.review_status(self.mod_id, &review.source, "stale")?;
                    }
                    code_mod.agent_review = store.review_state(self.mod_id)?;
                    self.enabled = false;
                    self.status = Status::Complete;
                }
            }
            return Ok(());
        }
        if dispatch
            && self.role == Role::Executor
            && self.status == Status::Ready
            && self.pending.is_none()
            && self.recovery.is_none()
            && code_mod.execution.as_ref().is_some_and(|e| {
                e.tasks
                    .iter()
                    .any(|r| r.worker == Some(self.id) && r.status == "waiting")
            })
            && store.resume_mail(self.mod_id, self.id)?
        {
            code_mod.execution = store.execution(self.mod_id)?;
            self.enabled = true;
            self.error = None;
        }
        if !dispatch || !self.enabled || self.pending.is_some() || self.recovery.is_some() {
            return Ok(());
        }
        let steering = self.status == Status::Running;
        if !steering && self.status != Status::Ready {
            return Ok(());
        }
        let mut input = if self.role == Role::Planner && !steering {
            store.planner_input(self.mod_id)?
        } else if !steering && self.role == Role::Executor && code_mod.execution.is_some() {
            None
        } else {
            if steering {
                store.next_steering(self.mod_id, self.id)?
            } else {
                store.next_input(self.mod_id, false)?
            }
        };
        if input.is_none() && steering && self.role == Role::Executor {
            input = store.next_mail(self.mod_id, self.id)?;
        }
        if input.is_none()
            && self.role == Role::Executor
            && !steering
            && code_mod.execution.is_some()
        {
            let plan = code_mod
                .planning
                .as_ref()
                .and_then(|p| p.plan.as_ref())
                .unwrap();
            input = store.task_input(self.mod_id, self.id, plan)?;
            code_mod.execution = store.execution(self.mod_id)?;
            if input.is_none() {
                let execution = code_mod.execution.as_ref().unwrap();
                let applied = execution.status == "applied";
                if execution.complete()
                    && !matches!(
                        execution.status.as_str(),
                        "applied" | "verifying" | "review"
                    )
                {
                    let checks = execution
                        .tasks
                        .iter()
                        .flat_map(|run| &run.checks)
                        .map(|result| Check {
                            task: result.task,
                            check: result.check.clone(),
                            command: result.command.clone(),
                        })
                        .collect();
                    store.execution_checks(self.mod_id, "verifying", &[], None)?;
                    code_mod.execution = store.execution(self.mod_id)?;
                    self.begin_checks(format!("final:{}", self.mod_id), checks);
                } else if !execution.tasks.iter().any(|run| {
                    run.status == "pending"
                        && code_mod
                            .planning
                            .as_ref()
                            .and_then(|p| p.plan.as_ref())
                            .is_some_and(|p| {
                                p.tasks.iter().any(|t| {
                                    t.id == run.task_id
                                        && t.accepts(&self.provider)
                                        && run.worker.is_none_or(|id| id == self.id)
                                })
                            })
                }) && !execution
                    .tasks
                    .iter()
                    .any(|run| ["sending", "running", "checking"].contains(&run.status.as_str()))
                    || execution.complete()
                {
                    self.enabled = false;
                    if execution.status == "review" {
                        self.status = Status::Complete;
                    }
                }
                if applied {
                    self.status = Status::Complete;
                }
            }
        }
        if let Some(input) = input {
            if self.writable && !steering {
                store.execution_status(self.mod_id, "running")?;
                code_mod.execution = store.execution(self.mod_id)?;
            }
            store.pending(self.id, Some(&input.source))?;
            let action = if steering {
                Action::Steer {
                    source: input.source.clone(),
                    texts: input.texts.clone(),
                    turn: self.turn.clone().unwrap(),
                }
            } else {
                Action::Run {
                    source: input.source.clone(),
                    text: input.texts.join("\n\n"),
                    routing: self.task_context(code_mod, &input.source),
                }
            };
            if !steering {
                self.status = Status::Starting;
                if input.source.starts_with("00000004-") {
                    self.task_source = Some(input.source.clone());
                    self.report_input(&input.source);
                }
            }
            self.pending = Some(input);
            if let Err(error) = self.client.as_ref().unwrap().send(action) {
                self.fail(error.to_string());
            }
        }
        Ok(())
    }

    fn task_context(&self, code_mod: &CodeMod, source: &str) -> Option<Value> {
        let execution = code_mod.execution.as_ref()?;
        let run = execution.tasks.iter().find(|run| run.source == source)?;
        let plan = code_mod.planning.as_ref()?.plan.as_ref()?;
        let task = plan.tasks.iter().find(|task| task.id == run.task_id)?;
        let mut state =
            crate::router::context(&execution.workspace.join("work"), &code_mod.description);
        state["source"] = serde_json::json!(source);
        state["plan"] = serde_json::json!(plan);
        state["task"] = serde_json::json!(task);
        state["previous_result"] = serde_json::json!({"summary":run.summary,"checks":run.checks});
        state["verification_feedback"] = serde_json::json!(run.verification_feedback);
        state["repair"] = serde_json::json!(run.repair);
        state["review_fix"] = serde_json::json!(run.review_feedback.is_some());
        state["runtime_recovery"] = serde_json::json!(run.restoring_runtime());
        Some(state)
    }

    fn receive(
        &mut self,
        event: Event,
        store: &mut Store,
        code_mod: &mut CodeMod,
    ) -> rusqlite::Result<()> {
        match event {
            Event::MailboxChanged => {
                code_mod.coordination = store.mailbox(self.mod_id)?;
            }
            Event::Preparing(label) => self.preparing = (!label.is_empty()).then_some(label),
            Event::Configured(selection) => {
                if self.role == Role::Planner {
                    store.planning_model(self.mod_id, &selection)?;
                    code_mod.planning = store.planning(self.mod_id)?;
                }
                self.selection = Some(selection);
            }
            Event::TaskConfigured { source, selection } => {
                store.task_model(self.mod_id, self.id, &source, &selection)?;
                code_mod.execution = store.execution(self.mod_id)?;
                self.model = Some(selection.model.clone());
                self.effort = Some(selection.effort.clone());
                self.selection = Some(selection);
            }
            Event::Ready {
                thread,
                model,
                effort,
            } => {
                self.model = model;
                self.effort = effort;
                self.preparing = None;
                store.save_thread(self.id, thread["id"].as_str().unwrap())?;
                self.thread_id = thread["id"].as_str().map(str::to_owned);
                if self.role == Role::Executor {
                    self.recover_task(store, code_mod, &thread)?;
                    if let Some(selection) = code_mod.execution.as_ref().and_then(|execution| {
                        execution
                            .tasks
                            .iter()
                            .find(|run| {
                                run.worker == Some(self.id)
                                    && ["sending", "running", "checking"]
                                        .contains(&run.status.as_str())
                            })
                            .and_then(|run| run.selection.as_ref())
                    }) {
                        self.model = Some(selection.model.clone());
                        self.effort = Some(selection.effort.clone());
                    }
                }
                if let Some(turns) = thread["turns"].as_array() {
                    for turn in turns {
                        let task_turn = turn["items"].as_array().is_some_and(|items| {
                            items.iter().any(|item| {
                                item["clientId"]
                                    .as_str()
                                    .is_some_and(|id| id.starts_with("00000004-"))
                            })
                        });
                        let current_plan = self.role == Role::Planner
                            && turn["items"].as_array().is_some_and(|items| {
                                items.iter().any(|item| {
                                    code_mod.planning.as_ref().is_some_and(|p| {
                                        item["clientId"].as_str() == Some(p.source.as_str())
                                    })
                                })
                            });
                        if self.role == Role::Planner {
                            self.final_plan = None;
                        }
                        if let Some(items) = turn["items"].as_array() {
                            for item in items {
                                if self.recovery.is_some()
                                    && self.recovery.as_deref() == item["clientId"].as_str()
                                {
                                    let source = self.recovery.take().unwrap();
                                    let input = store.submission(self.mod_id, &source)?;
                                    self.acknowledge(store, code_mod, &input)?;
                                }
                                if !task_turn
                                    || item["clientId"]
                                        .as_str()
                                        .is_some_and(|id| id.starts_with("00000002-"))
                                {
                                    self.item(store, code_mod, item, true)?;
                                }
                            }
                        }
                        if current_plan && turn["status"] == "completed" {
                            self.complete_plan(store, code_mod)?;
                        }
                    }
                }
                if self.recovery.is_some() {
                    self.fail("Delivery could not be confirmed. The instruction is retained; automatic retry is paused.".into());
                } else if !matches!(
                    self.status,
                    Status::Complete | Status::Failed | Status::Checking | Status::Stopping
                ) {
                    self.status = Status::Ready;
                    if self.role == Role::Planner
                        && code_mod
                            .planning
                            .as_ref()
                            .is_some_and(|p| p.status != "pending")
                    {
                        self.enabled = false;
                        store.planning_status(self.mod_id, "paused")?;
                        code_mod.planning = store.planning(self.mod_id)?;
                        self.error = Some("Planning was interrupted. Ctrl+R to retry.".into());
                    }
                }
            }
            Event::Accepted { source, turn } => {
                if !source.starts_with("00000005-") {
                    self.activity_started = Instant::now();
                }
                self.preparing = None;
                if let Some(input) = self.pending.take() {
                    if input.source != source {
                        return Err(rusqlite::Error::InvalidQuery);
                    }
                    self.acknowledge(store, code_mod, &input)?;
                }
                self.report_input(&source);
                if self.role == Role::Planner && source.starts_with("00000003-") {
                    self.final_plan = None;
                }
                if source.starts_with("00000004-") {
                    self.task_source = Some(source.clone());
                    store.task_status(self.mod_id, &source, "running", Some(&turn))?;
                    code_mod.execution = store.execution(self.mod_id)?;
                }
                self.turn = Some(turn.clone());
                self.status = Status::Running;
                if !self.enabled {
                    if let Err(error) = self.client.as_ref().unwrap().send(Action::Stop { turn }) {
                        self.fail(error.to_string());
                    } else {
                        self.status = Status::Stopping;
                    }
                }
            }
            Event::Rejected { source, message } => {
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|input| input.source == source)
                {
                    store.pending(self.id, None)?;
                    self.pending = None;
                }
                if source.starts_with("00000005-") {
                    let id = i64::from_str_radix(source.rsplit('-').next().unwrap_or(""), 16)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?;
                    store.defer_mail(self.mod_id, id)?;
                    code_mod.coordination = store.mailbox(self.mod_id)?;
                    return Ok(());
                }
                if source.starts_with("00000002-") {
                    code_mod
                        .queue
                        .extend(store.reject_steering(self.mod_id, self.id, &source)?);
                    code_mod.steering = store.steering(self.mod_id)?;
                    save_message(
                        store,
                        code_mod,
                        Message {
                            task: None,
                            item_id: Some(format!("rejected:{source}:{}", self.id)),
                            role: "harness".into(),
                            body: format!(
                                "Steering not accepted · w{}. Queued for the next edit round.",
                                self.id
                            ),
                            model: None,
                            effort: None,
                        },
                    )?;
                    return Ok(());
                }
                if self.role == Role::Reviewer {
                    store.review_status(self.mod_id, &source, "blocked")?;
                    code_mod.agent_review = store.review_state(self.mod_id)?;
                }
                self.enabled = false;
                self.error = Some(message);
                if self.turn.is_none() {
                    self.status = Status::Ready;
                }
                if source.starts_with("00000004-") {
                    self.block_task(
                        store,
                        code_mod,
                        "paused",
                        self.error.clone().unwrap_or_default(),
                    )?;
                }
            }
            Event::Failed(message) => {
                if let Some(client) = &mut self.client {
                    client.shutdown();
                }
                self.fail(message);
                if self.role == Role::Reviewer {
                    if let Some(review) = &code_mod.agent_review {
                        store.review_status(self.mod_id, &review.source, "blocked")?;
                    }
                    code_mod.agent_review = store.review_state(self.mod_id)?;
                    return Ok(());
                }
                if self.role == Role::Planner {
                    store.planning_status(self.mod_id, "failed")?;
                    code_mod.planning = store.planning(self.mod_id)?;
                }
                if self.task_source.is_none() {
                    self.task_source = code_mod.execution.as_ref().and_then(|execution| {
                        execution
                            .tasks
                            .iter()
                            .find(|run| {
                                (run.worker == Some(self.id) || run.worker.is_none())
                                    && ["sending", "running", "checking"]
                                        .contains(&run.status.as_str())
                            })
                            .map(|run| run.source.clone())
                    });
                }
                if self
                    .task_source
                    .as_ref()
                    .is_some_and(|source| source.starts_with("final:"))
                    || code_mod
                        .execution
                        .as_ref()
                        .is_some_and(|execution| execution.status == "verifying")
                {
                    store.execution_status(self.mod_id, "blocked")?;
                    code_mod.execution = store.execution(self.mod_id)?;
                    self.task_source = None;
                } else if self.task_source.is_some() {
                    self.block_task(
                        store,
                        code_mod,
                        "paused",
                        self.error.clone().unwrap_or_default(),
                    )?;
                }
            }
            Event::Checked {
                source,
                mut checks,
                before,
            } if self.task_source.as_deref() == Some(&source) => {
                self.preparing = None;
                self.verify_before = Some(before);
                let passed = self.checks_passed(code_mod, &checks);
                let matched = checks.len() <= self.verification.len()
                    && checks
                        .iter()
                        .zip(&self.verification)
                        .all(|(result, expected)| {
                            expected.task.is_none_or(|id| result.task == Some(id))
                                && result.check == expected.check
                                && result.command == expected.command
                        });
                if matched {
                    // Retain commands skipped after a failure or interruption.
                    checks.extend(self.verification.iter().skip(checks.len()).map(|check| {
                        crate::execution::CheckResult {
                            missing_runtime: None,
                            task: check.task,
                            check: check.check.clone(),
                            command: check.command.clone(),
                            exit_code: None,
                            output: "Not run.".into(),
                        }
                    }));
                }
                if source.starts_with("final:") {
                    let fingerprint = if passed {
                        self.verify_before
                            .as_ref()
                            .and_then(|snapshot| workspace::fingerprint(snapshot).ok())
                    } else {
                        None
                    };
                    let passed = passed && fingerprint.is_some();
                    store.execution_checks(
                        self.mod_id,
                        if passed { "review" } else { "blocked" },
                        &checks,
                        fingerprint.as_deref(),
                    )?;
                    let recover = !passed
                        && matched
                        && self.enabled
                        && self.status == Status::Checking
                        && self.error.is_none()
                        && store.recover_runtime(self.mod_id)?;
                    code_mod.execution = store.execution(self.mod_id)?;
                    self.task_source = None;
                    self.enabled = recover;
                    self.status = if passed {
                        Status::Complete
                    } else {
                        Status::Ready
                    };
                    if recover {
                        save_message(store, code_mod, Message {
                            task: None,
                            item_id: Some(format!("runtime:{source}:{}", checks.iter().find_map(|c| c.missing_runtime.as_ref()).unwrap())),
                            role: "system".into(),
                            body: "Restoring a missing task environment, then rerunning final checks.".into(),
                            model: None,
                            effort: None,
                        })?;
                    } else if !passed && self.error.is_none() {
                        self.error = Some(format!(
                            "Final verification blocked · {} · Ctrl+R retries",
                            checks
                                .iter()
                                .find(|c| c.failed())
                                .map_or_else(|| "checks interrupted".into(), |c| c.brief())
                        ));
                    }
                    return Ok(());
                }
                store.finish_task(
                    self.mod_id,
                    &source,
                    if passed { "done" } else { "blocked" },
                    &self.task_summary,
                    &checks,
                )?;
                let failure = checks.iter().find(|check| check.exit_code != Some(0));
                let recover = !passed
                    && self.enabled
                    && self.status == Status::Checking
                    && failure.is_some_and(|check| check.exit_code.is_some_and(|code| code > 0))
                    && matched
                    && store.recover_verification(self.mod_id, &source, self.id)?;
                let summary = format!(
                    "{} · {}\n{}",
                    if passed {
                        "✓ task complete"
                    } else if recover {
                        "↺ verification failed · recovering"
                    } else {
                        "! check failed"
                    },
                    self.task_name(code_mod),
                    if passed {
                        self.task_summary.clone()
                    } else {
                        failure.map_or_else(
                            || "Checks incomplete or interrupted.".into(),
                            |check| check.brief(),
                        )
                    }
                );
                save_message(
                    store,
                    code_mod,
                    Message {
                        task: crate::task_worktree::task_id(&source).ok(),
                        item_id: Some(format!("result:{source}")),
                        role: format!("{}:{}", self.provider, self.id),
                        body: summary,
                        model: self.model.clone(),
                        effort: self.effort.clone(),
                    },
                )?;
                self.task_source = None;
                self.task_report = None;
                self.status = Status::Ready;
                if recover {
                    self.error = None;
                } else if !passed {
                    self.enabled = false;
                    self.error.get_or_insert_with(|| {
                        format!(
                            "Verification failed · {} · Ctrl+R retries",
                            failure.map_or_else(
                                || "checks incomplete or interrupted".into(),
                                |check| check.brief()
                            )
                        )
                    });
                }
                code_mod.execution = store.execution(self.mod_id)?;
            }
            Event::Checked { .. } => {}
            Event::Notification(message) => {
                let params = &message["params"];
                match message["method"].as_str().unwrap_or("") {
                    "thread/started" => {
                        if let Some(id) = params["thread"]["id"].as_str() {
                            store.save_thread(self.id, id)?;
                            self.thread_id = Some(id.into());
                        }
                    }
                    "item/agentMessage/delta"
                        if self.role == Role::Executor && self.task_source.is_none() =>
                    {
                        if let (Some(id), Some(delta)) =
                            (params["itemId"].as_str(), params["delta"].as_str())
                        {
                            let mut message = code_mod
                                .messages
                                .iter()
                                .find(|message| message.item_id.as_deref() == Some(id))
                                .cloned()
                                .unwrap_or(Message {
                                    task: None,
                                    item_id: Some(id.into()),
                                    role: format!("{}:{}", self.provider, self.id),
                                    body: String::new(),
                                    model: self.model.clone(),
                                    effort: self.effort.clone(),
                                });
                            message.body.push_str(delta);
                            save_message(store, code_mod, message)?;
                        }
                    }
                    "item/completed" => self.item(store, code_mod, &params["item"], false)?,
                    "turn/completed" if self.turn.as_deref() == params["turn"]["id"].as_str() => {
                        self.turn = None;
                        self.status = Status::Ready;
                        if self.role == Role::Reviewer {
                            self.complete_review(
                                store,
                                code_mod,
                                params["turn"]["status"] == "completed",
                            )?;
                        }
                        if self.role == Role::Planner {
                            if params["turn"]["status"] == "completed" {
                                self.complete_plan(store, code_mod)?;
                            } else {
                                store.planning_status(self.mod_id, "paused")?;
                                code_mod.planning = store.planning(self.mod_id)?;
                            }
                        }
                        if self.role == Role::Executor && self.task_source.is_some() {
                            if params["turn"]["status"] == "completed" {
                                self.complete_task(store, code_mod)?;
                            } else {
                                self.block_task(store,code_mod,"paused","Task interrupted. Ctrl+R retries it from the current working files.".into())?;
                            }
                        }
                        if params["turn"]["status"] != "completed" {
                            self.enabled = false;
                            if params["turn"]["status"] == "failed" {
                                self.error = Some(
                                    params["turn"]["error"]["message"]
                                        .as_str()
                                        .unwrap_or("Codex could not finish this turn.")
                                        .into(),
                                );
                            }
                        }
                    }
                    "error" if params["willRetry"] == false => {
                        self.error = Some(
                            params["error"]["message"]
                                .as_str()
                                .unwrap_or("Codex reported an error.")
                                .into(),
                        );
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn acknowledge(
        &self,
        store: &mut Store,
        code_mod: &mut CodeMod,
        input: &Submission,
    ) -> rusqlite::Result<()> {
        if self.role == Role::Reviewer {
            store.pending(self.id, None)?;
            store.review_status(self.mod_id, &input.source, "running")?;
            code_mod.agent_review = store.review_state(self.mod_id)?;
            return Ok(());
        }
        store.acknowledge(self.id, input)?;
        if input.source.starts_with("00000005-") {
            code_mod.coordination = store.mailbox(self.mod_id)?;
            return Ok(());
        }
        if input.source.starts_with("00000003-") {
            code_mod.planning = store.planning(self.mod_id)?;
            return Ok(());
        }
        if input.source.starts_with("00000004-") {
            code_mod.execution = store.execution(self.mod_id)?;
            return Ok(());
        }
        code_mod
            .queue
            .retain(|message| crate::store::source_id(message.id, false) != input.source);
        if input.source.starts_with("00000002-") {
            code_mod.steering = store.steering(self.mod_id)?;
        }
        save_message(
            store,
            code_mod,
            Message {
                task: None,
                item_id: Some(input.source.clone()),
                role: "user".into(),
                body: input.texts.join("\n\n"),
                model: None,
                effort: None,
            },
        )?;
        if input.source.starts_with("00000002-") {
            save_message(
                store,
                code_mod,
                Message {
                    task: None,
                    item_id: Some(format!("delivered:{}:{}", input.source, self.id)),
                    role: "harness".into(),
                    body: format!("Steering delivered · w{}", self.id),
                    model: None,
                    effort: None,
                },
            )?;
        }
        Ok(())
    }

    fn item(
        &mut self,
        store: &Store,
        code_mod: &mut CodeMod,
        item: &Value,
        historical: bool,
    ) -> rusqlite::Result<()> {
        if self.role == Role::Reviewer {
            if item["type"] == "userMessage" {
                return Ok(());
            }
            if item["type"] == "agentMessage" && item["phase"] != "commentary" {
                self.task_report = item["text"].as_str().map(str::to_owned);
                return Ok(());
            }
        }
        if self.role == Role::Executor
            && self.task_source.is_some()
            && item["type"] == "userMessage"
            && let Some(source) = item["clientId"].as_str()
        {
            self.report_input(source);
        }
        if self.role == Role::Executor
            && self.task_source.is_some()
            && item["type"] == "agentMessage"
            && item["phase"] != "commentary"
        {
            let text = item["text"].as_str();
            let acknowledgement = self.task_mail
                && text
                    .and_then(|text| serde_json::from_str::<Report>(text).ok())
                    .is_some_and(|report| {
                        report.status == "completed"
                            && !report.summary.trim().is_empty()
                            && report.checks.is_empty()
                            && report.repair.is_none()
                    });
            let keep_report = acknowledgement
                && self
                    .task_report
                    .as_deref()
                    .zip(self.current_task(code_mod))
                    .is_some_and(|(text, task)| Report::parse(text, task).is_ok());
            if !keep_report {
                self.task_report = text.map(str::to_owned);
            }
            return Ok(());
        }
        if item["clientId"].as_str().is_some_and(|source| {
            source.starts_with("00000004-") || source.starts_with("00000005-")
        }) {
            return Ok(());
        }
        if self.role == Role::Planner {
            match item["type"].as_str() {
                Some("userMessage") => return Ok(()),
                Some("agentMessage") if item["phase"] != "commentary" => {
                    self.final_plan = item["text"].as_str().map(str::to_owned);
                    return Ok(());
                }
                _ => {}
            }
        }
        let Some(id) = item["clientId"].as_str().or_else(|| item["id"].as_str()) else {
            return Ok(());
        };
        let (role, body) = match item["type"].as_str() {
            Some("agentMessage") => (
                if self.role == Role::Planner {
                    "planner"
                } else if self.role == Role::Reviewer {
                    "reviewer"
                } else {
                    "codex"
                },
                item["text"].as_str().unwrap_or("").to_owned(),
            ),
            Some("userMessage") => (
                "user",
                item["content"]
                    .as_array()
                    .map(|input| {
                        input
                            .iter()
                            .filter_map(|part| part["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n\n")
                    })
                    .unwrap_or_default(),
            ),
            _ => return Ok(()),
        };
        if body.is_empty() {
            return Ok(());
        }
        save_message(
            store,
            code_mod,
            Message {
                task: if historical { None } else { self.task_run_id() },
                item_id: Some(id.into()),
                role: if role == "codex" {
                    format!("{}:{}", self.provider, self.id)
                } else {
                    role.into()
                },
                body,
                model: if historical || role == "user" {
                    None
                } else {
                    self.model.clone()
                },
                effort: if historical || role == "user" {
                    None
                } else {
                    self.effort.clone()
                },
            },
        )
    }

    fn complete_review(
        &mut self,
        store: &mut Store,
        code_mod: &mut CodeMod,
        completed: bool,
    ) -> rusqlite::Result<()> {
        if let Some(source) = self.task_source.take() {
            if completed {
                store.finish_review(
                    code_mod,
                    &source,
                    self.task_report.as_deref().unwrap_or(""),
                )?;
            } else {
                store.review_status(self.mod_id, &source, "paused")?;
            }
            code_mod.agent_review = store.review_state(self.mod_id)?;
            let review = code_mod.agent_review.as_ref().unwrap();
            if review.source != source {
                self.enabled = false;
                self.status = Status::Complete;
                return Ok(());
            }
            let body = review.report.as_ref().map_or_else(
                || review.label(),
                |r| {
                    let mut text = format!("{}\n{}", review.label(), r.summary);
                    for f in &r.findings {
                        text.push_str(&format!(
                            "\n{} · {} · {}:{}\n{}\nFix: {}",
                            f.priority, f.title, f.file, f.line, f.evidence, f.fix
                        ));
                    }
                    text
                },
            );
            save_message(
                store,
                code_mod,
                Message {
                    task: None,
                    item_id: Some(format!("review:{source}")),
                    role: "reviewer".into(),
                    body,
                    model: self.model.clone(),
                    effort: self.effort.clone(),
                },
            )?;
        }
        self.enabled = false;
        self.status = Status::Complete;
        Ok(())
    }

    fn complete_plan(&mut self, store: &mut Store, code_mod: &mut CodeMod) -> rusqlite::Result<()> {
        let plan = self
            .final_plan
            .as_deref()
            .ok_or_else(|| "Codex returned no plan.".to_owned())
            .and_then(Plan::parse);
        match plan {
            Ok(plan) => {
                let source = code_mod.planning.as_ref().unwrap().source.clone();
                let message = store.save_plan(self.mod_id, &source, &plan)?;
                remember_message(code_mod, message);
                code_mod.planning = store.planning(self.mod_id)?;
                self.status = Status::Complete;
                self.enabled = false;
            }
            Err(error) => {
                store.planning_status(self.mod_id, "failed")?;
                code_mod.planning = store.planning(self.mod_id)?;
                self.fail(format!("{error} Ctrl+R to retry planning."));
            }
        }
        Ok(())
    }

    fn report_input(&mut self, source: &str) {
        if source.starts_with("00000004-") || source.starts_with("00000002-") {
            self.task_report = None;
            self.task_mail = false;
        } else if source.starts_with("00000005-") {
            self.task_mail = true;
        }
    }

    pub fn task_run_id(&self) -> Option<i64> {
        self.task_source
            .as_deref()
            .and_then(|source| crate::task_worktree::task_id(source).ok())
    }

    fn current_task<'a>(&self, code_mod: &'a CodeMod) -> Option<&'a Task> {
        let run = code_mod
            .execution
            .as_ref()?
            .tasks
            .iter()
            .find(|run| Some(run.source.as_str()) == self.task_source.as_deref())?;
        code_mod
            .planning
            .as_ref()?
            .plan
            .as_ref()?
            .tasks
            .iter()
            .find(|task| task.id == run.task_id)
    }

    fn task_name(&self, code_mod: &CodeMod) -> String {
        self.current_task(code_mod)
            .map_or_else(|| "task".into(), |task| task.title.clone())
    }

    pub fn progress(&self) -> Option<&str> {
        self.busy().then_some(self.preparing.as_deref()).flatten()
    }

    pub fn elapsed_label(&self) -> String {
        let seconds = self.activity_started.elapsed().as_secs();
        if seconds >= 60 {
            format!("{}m {:02}s", seconds / 60, seconds % 60)
        } else {
            format!("{seconds}s")
        }
    }

    pub fn activity(&self, code_mod: &CodeMod) -> String {
        let task = code_mod.execution.as_ref().and_then(|e| {
            e.tasks
                .iter()
                .find(|run| {
                    run.worker == Some(self.id)
                        && matches!(run.status.as_str(), "sending" | "running" | "checking")
                })
                .or_else(|| {
                    e.tasks
                        .iter()
                        .find(|run| run.worker == Some(self.id) && run.status == "pending")
                })
        });
        let number = task.and_then(|run| {
            code_mod
                .planning
                .as_ref()?
                .plan
                .as_ref()?
                .tasks
                .iter()
                .position(|task| task.id == run.task_id)
        });
        let action = number.map_or_else(
            || {
                if self.status == Status::Checking {
                    "final checks".into()
                } else {
                    "connecting".into()
                }
            },
            |n| format!("task {}", n + 1),
        );
        format!("{} w{} · {action}", self.provider, self.id)
    }

    fn block_task(
        &mut self,
        store: &mut Store,
        code_mod: &mut CodeMod,
        status: &str,
        summary: String,
    ) -> rusqlite::Result<()> {
        if let Some(source) = self.task_source.take() {
            store.finish_task(self.mod_id, &source, status, &summary, &[])?;
            code_mod.execution = store.execution(self.mod_id)?;
        }
        self.task_report = None;
        self.enabled = false;
        self.error = (status != "waiting").then_some(summary);
        Ok(())
    }

    fn complete_task(&mut self, store: &mut Store, code_mod: &mut CodeMod) -> rusqlite::Result<()> {
        let source = self.task_source.as_ref().unwrap().clone();
        let run = code_mod
            .execution
            .as_ref()
            .unwrap()
            .tasks
            .iter()
            .find(|run| run.source == source)
            .unwrap();
        if let Some(recipient) = store.waiting_mail(self.mod_id, run.id)? {
            return self.block_task(
                store,
                code_mod,
                "waiting",
                if recipient == "user" {
                    "Waiting for your answer in the composer.".into()
                } else {
                    format!("Waiting for a reply from {recipient}.")
                },
            );
        }
        let task = self.current_task(code_mod).unwrap();
        let report = self
            .task_report
            .as_deref()
            .ok_or_else(|| format!("{} returned no task report.", self.provider))
            .and_then(|text| Report::parse(text, task));
        match report {
            Ok(report) if report.status == "completed" => {
                self.task_summary = report.summary;
                store.task_status(self.mod_id, &source, "checking", None)?;
                code_mod.execution = store.execution(self.mod_id)?;
                self.begin_checks(source, report.checks);
            }
            Ok(report) if report.repair.is_some() => {
                match store.request_repair(self.mod_id, &source, report.repair.as_ref().unwrap())? {
                    Ok(()) => {
                        save_message(
                            store,
                            code_mod,
                            Message {
                                task: crate::task_worktree::task_id(&source).ok(),
                                item_id: Some(format!("repair:{source}")),
                                role: "harness".into(),
                                body: report.repair.as_ref().unwrap().notice(),
                                model: None,
                                effort: None,
                            },
                        )?;
                        code_mod.execution = store.execution(self.mod_id)?;
                        self.task_source = None;
                        self.task_report = None;
                        self.enabled = false;
                        self.error = None;
                    }
                    Err(error) => self.block_task(store, code_mod, "blocked", error)?,
                }
            }
            Ok(report) => self.block_task(store, code_mod, "blocked", report.summary)?,
            Err(error) => self.block_task(store, code_mod, "blocked", error)?,
        }
        Ok(())
    }

    fn recover_task(
        &mut self,
        store: &mut Store,
        code_mod: &mut CodeMod,
        thread: &Value,
    ) -> rusqlite::Result<()> {
        let active = code_mod
            .execution
            .as_ref()
            .and_then(|execution| {
                execution.tasks.iter().find(|run| {
                    (run.worker == Some(self.id) || run.worker.is_none())
                        && (["sending", "running", "checking"].contains(&run.status.as_str())
                            || run.status == "paused"
                                && self.recovery.as_deref() == Some(&run.source))
                })
            })
            .cloned();
        let Some(run) = active else {
            return Ok(());
        };
        self.task_source = Some(run.source.clone());
        self.task_report = None;
        self.task_mail = false;
        let turn = thread["turns"].as_array().and_then(|turns| {
            turns.iter().find(|turn| {
                run.turn
                    .as_deref()
                    .is_some_and(|id| turn["id"].as_str() == Some(id))
                    || turn["items"].as_array().is_some_and(|items| {
                        items
                            .iter()
                            .any(|item| item["clientId"].as_str() == Some(&run.source))
                    })
            })
        });
        let Some(turn) = turn else {
            self.block_task(store,code_mod,"paused","Task delivery could not be confirmed. The working files are retained; Ctrl+R explicitly retries it.".into())?;
            self.fail(self.error.clone().unwrap());
            return Ok(());
        };
        if self.recovery.as_deref() == Some(&run.source) {
            let input = store.submission(self.mod_id, &run.source)?;
            self.acknowledge(store, code_mod, &input)?;
            self.recovery = None;
        }
        store.task_status(self.mod_id, &run.source, "running", turn["id"].as_str())?;
        code_mod.execution = store.execution(self.mod_id)?;
        if let Some(items) = turn["items"].as_array() {
            for item in items {
                self.item(store, code_mod, item, true)?;
            }
        }
        if turn["status"] == "completed" {
            self.complete_task(store, code_mod)?;
        } else {
            if turn["status"] == "inProgress" {
                let _ = self.client.as_ref().unwrap().send(Action::Stop {
                    turn: turn["id"].as_str().unwrap().into(),
                });
            }
            self.block_task(
                store,
                code_mod,
                "paused",
                "Task was interrupted. Ctrl+R retries it from the saved working files.".into(),
            )?;
            if turn["status"] == "inProgress" {
                self.turn = turn["id"].as_str().map(str::to_owned);
                self.status = Status::Stopping;
            }
        }
        Ok(())
    }

    fn begin_checks(&mut self, source: String, mut checks: Vec<Check>) {
        if let Ok(id) = crate::task_worktree::task_id(&source) {
            for check in &mut checks {
                check.task = Some(id);
            }
        }
        self.preparing = Some("Preparing checks".into());
        self.verify_before = None;
        self.verification = checks.clone();
        self.task_source = Some(source.clone());
        self.status = Status::Checking;
        if let Err(error) = self
            .client
            .as_ref()
            .unwrap()
            .send(Action::Verify { source, checks })
        {
            self.fail(error.to_string());
        }
    }

    fn checks_passed(
        &mut self,
        code_mod: &CodeMod,
        checks: &[crate::execution::CheckResult],
    ) -> bool {
        let unchanged = !self
            .task_source
            .as_ref()
            .is_some_and(|source| source.starts_with("final:"))
            || workspace::source_state(
                &code_mod.execution.as_ref().unwrap().workspace.join("work"),
            )
            .ok()
            .is_some_and(|state| self.verify_before.as_ref() == Some(&state));
        if !unchanged {
            self.error = Some("Source changed after final verification. Run checks again.".into());
        }
        unchanged
            && !checks.is_empty()
            && checks.len() == self.verification.len()
            && checks
                .iter()
                .zip(&self.verification)
                .all(|(result, expected)| {
                    result.exit_code == Some(0)
                        && result.check == expected.check
                        && result.command == expected.command
                })
    }

    fn fail(&mut self, message: String) {
        self.status = Status::Failed;
        self.enabled = false;
        self.error = Some(message);
    }
}

fn save_message(store: &Store, code_mod: &mut CodeMod, message: Message) -> rusqlite::Result<()> {
    store.save_message(code_mod.id, &message)?;
    remember_message(code_mod, message);
    Ok(())
}

fn remember_message(code_mod: &mut CodeMod, mut message: Message) {
    if let Some(existing) = code_mod
        .messages
        .iter_mut()
        .find(|existing| existing.item_id == message.item_id)
    {
        message.task = existing.task.or(message.task);
        if message.model.is_none() {
            message.model.clone_from(&existing.model);
        }
        if message.effort.is_none() {
            message.effort.clone_from(&existing.effort);
        }
        *existing = message;
    } else {
        code_mod.messages.push(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::test_support::TestData;
    use serde_json::json;

    #[test]
    #[ignore = "Uses both subscription CLIs and a disposable Apple Container VM"]
    fn mixed_vm_workers_execute_and_exchange_mail() {
        use std::{
            fs,
            time::{Duration, Instant},
        };
        let data = TestData::new();
        let project = data.0.join("project");
        let root = data.0.join(format!(
            "mixed-{}",
            data.0.file_name().unwrap().to_str().unwrap()
        ));
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join("README.md"),
            "A disposable greeting fixture.\n",
        )
        .unwrap();
        let mut store = data.store();
        let project_id = store.load_project(&project).unwrap().id;
        let mut m = store
            .create_mod(project_id, "Create two independent greeting scripts")
            .unwrap();
        let context_token = format!(
            "muse-context-{}",
            data.0.file_name().unwrap().to_str().unwrap()
        );
        let plan = Plan::parse(&json!({"summary":"Two greetings","tasks":[
            {"id":"codex","title":"Codex greeting","outcome":"Create codex.sh printing exactly hello codex. Read your inbox and send a useful update to the muse task once done.","files":["codex.sh"],"depends_on":[],"coordination":[{"task":"muse","topic":"Greeting fixture verification"}],"worker":"codex","checks":["Script prints hello codex"]},
            {"id":"muse","title":"Muse greeting","outcome":format!("This is a cancellation fixture. Remember {context_token} for your final summary; do not write it to files or repeat it in commentary. Before writing source, if $HOME/cancel-once is absent, create that marker and run /bin/sleep 299. The harness will interrupt that command and restart. On retry skip the sleep and recall the same token from conversation history in your final summary; do not invent a replacement. Then create muse.sh printing exactly hello muse. Use the native MCP inbox tools to read messages and send the codex task a useful update once done."),"files":["muse.sh"],"depends_on":[],"coordination":[{"task":"codex","topic":"Greeting fixture verification"}],"worker":"muse","checks":["Script prints hello muse"]}
        ]}).to_string()).unwrap();
        let source = m.planning.as_ref().unwrap().source.clone();
        store.save_plan(m.id, &source, &plan).unwrap();
        m.planning = store.planning(m.id).unwrap();
        workspace::create(&project, &root).unwrap();
        store.create_execution(m.id, &root, &plan).unwrap();
        m.execution = store.execution(m.id).unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut workers: Vec<_> = ["codex", "muse"]
                .iter()
                .map(|provider| {
                    Worker::start_with_muse(
                        &project,
                        &m,
                        store
                            .worker_provider(m.id, Role::Executor, 0, provider)
                            .unwrap(),
                        Role::Executor,
                        None,
                        true,
                    )
                    .unwrap()
                })
                .collect();
            let startup = Instant::now() + Duration::from_secs(240);
            while workers.iter().any(|w| w.status != Status::Ready) && Instant::now() < startup {
                for worker in &mut workers {
                    worker.poll(&mut store, &mut m, false, &project).unwrap();
                    assert!(
                        worker.error.is_none(),
                        "{}: {}",
                        worker.provider,
                        worker.error.as_deref().unwrap_or("")
                    );
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            assert!(
                workers.iter().all(|w| w.status == Status::Ready),
                "Worker startup timed out"
            );
            let deadline = Instant::now() + Duration::from_secs(600);
            let mut previous = String::new();
            let mut overlap = false;
            let mut steered = false;
            let mut lost_ack = false;
            let mut interrupted = false;
            let mut retried = false;
            let mut inspect_at = Instant::now();
            let name = format!("sprowt-{}", root.file_name().unwrap().to_str().unwrap());
            let sleeper = || {
                std::process::Command::new("container").args(["exec", &name, "/usr/bin/python3", "-c",
                    "from pathlib import Path; print(sum(b'/bin/sleep\\x00299\\x00' in p.read_bytes() for p in Path('/proc').glob('[0-9]*/cmdline')))"])
                    .output().unwrap()
            };
            while Instant::now() < deadline && m.execution.as_ref().unwrap().status != "review" {
                for worker in &mut workers {
                    if worker.provider == "muse"
                        && worker
                            .pending
                            .as_ref()
                            .is_some_and(|input| input.source.starts_with("00000002-"))
                    {
                        let events = worker.client.as_ref().unwrap().poll().collect::<Vec<_>>();
                        for event in events {
                            if matches!(&event, Event::Accepted { source, .. } if source.starts_with("00000002-"))
                            {
                                lost_ack = true;
                            } else {
                                worker.receive(event, &mut store, &mut m).unwrap();
                            }
                        }
                    } else {
                        worker.poll(&mut store, &mut m, true, &project).unwrap();
                    }
                    assert!(
                        worker.error.is_none()
                            || interrupted
                                && !retried
                                && worker.provider == "muse"
                                && worker.status == Status::Ready,
                        "{}: {}",
                        worker.provider,
                        worker.error.as_deref().unwrap_or("")
                    );
                }
                overlap |= m
                    .execution
                    .as_ref()
                    .unwrap()
                    .tasks
                    .iter()
                    .filter(|t| t.status == "running")
                    .count()
                    == 2;
                if workers[1].status == Status::Running && !steered {
                    let queued = store.enqueue(m.id, "Include sprowt-steering-ok in your final JSON summary; keep the required greeting unchanged.").unwrap();
                    store
                        .steer_to(m.id, &[queued.id], &[workers[1].id])
                        .unwrap();
                    steered = true;
                }
                if steered && !interrupted && lost_ack && Instant::now() >= inspect_at {
                    let process = sleeper();
                    assert!(process.status.success());
                    if String::from_utf8_lossy(&process.stdout).trim() != "0" {
                        workers[1].toggle();
                        interrupted = true;
                        eprintln!("Interrupting Muse's native sleep command");
                    }
                    inspect_at = Instant::now() + Duration::from_millis(500);
                }
                if interrupted && !retried && workers[1].status == Status::Ready {
                    let process = sleeper();
                    assert!(
                        process.status.success(),
                        "Stopping Muse stopped the shared VM"
                    );
                    assert_eq!(
                        String::from_utf8_lossy(&process.stdout).trim(),
                        "0",
                        "Muse left its child command running"
                    );
                    let updated = Plan::parse(
                        &serde_json::to_string(&plan)
                            .unwrap()
                            .replace(&context_token, "the token from your earlier turn"),
                    )
                    .unwrap();
                    store.save_plan(m.id, &source, &updated).unwrap();
                    m.planning = store.planning(m.id).unwrap();
                    let state_path = root.join("muse").join(format!("{}.json", workers[1].id));
                    let state: Value =
                        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
                    let native_session = state["sessionId"].as_str().unwrap().to_owned();
                    let steering = state["commands"]
                        .as_object()
                        .unwrap()
                        .keys()
                        .find(|source| source.starts_with("00000002-"))
                        .unwrap()
                        .clone();
                    assert_eq!(
                        store
                            .worker_provider(m.id, Role::Executor, 0, "muse")
                            .unwrap()
                            .pending
                            .as_deref(),
                        Some(steering.as_str())
                    );
                    let user_messages = m.messages.iter().filter(|msg| msg.role == "user").count();
                    workers.clear();
                    store = data.store();
                    m = store.load_project(&project).unwrap().mods.remove(0);
                    workers = ["codex", "muse"]
                        .iter()
                        .map(|provider| {
                            Worker::start_with_muse(
                                &project,
                                &m,
                                store
                                    .worker_provider(m.id, Role::Executor, 0, provider)
                                    .unwrap(),
                                Role::Executor,
                                None,
                                true,
                            )
                            .unwrap()
                        })
                        .collect();
                    let reconnect = Instant::now() + Duration::from_secs(180);
                    while workers.iter().any(|w| w.status == Status::Connecting)
                        && Instant::now() < reconnect
                    {
                        for worker in &mut workers {
                            worker.poll(&mut store, &mut m, false, &project).unwrap();
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    assert!(
                        workers[1].status == Status::Ready,
                        "Muse recovery: {:?}",
                        workers[1].error
                    );
                    assert!(
                        workers[1].recovery.is_none(),
                        "Accepted steering was not recovered"
                    );
                    assert_eq!(
                        workers[1].thread_id.as_deref(),
                        Some(native_session.as_str())
                    );
                    assert_eq!(
                        m.messages.iter().filter(|msg| msg.role == "user").count(),
                        user_messages + 1
                    );
                    let restored: Value =
                        serde_json::from_slice(&fs::read(state_path).unwrap()).unwrap();
                    assert_eq!(
                        restored["commands"], state["commands"],
                        "Recovery resubmitted instructions"
                    );
                    store.retry_tasks(m.id).unwrap();
                    m.execution = store.execution(m.id).unwrap();
                    for worker in &mut workers {
                        if !worker.enabled {
                            worker.toggle();
                        }
                    }
                    retried = true;
                    eprintln!(
                        "Harness restarted; native session and lost steering receipt recovered without redelivery"
                    );
                }
                let state = workers
                    .iter()
                    .map(|w| w.label(None))
                    .collect::<Vec<_>>()
                    .join("; ");
                if state != previous {
                    eprintln!("{state}");
                    previous = state;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            assert_eq!(m.execution.as_ref().unwrap().status, "review");
            assert!(overlap, "Mixed workers never overlapped");
            assert!(
                steered && interrupted && retried,
                "Steering or cancellation was not exercised"
            );
            assert!(
                m.messages
                    .iter()
                    .any(|msg| msg.role.starts_with("muse:")
                        && msg.body.contains("sprowt-steering-ok")),
                "Muse did not follow accepted steering"
            );
            assert!(m.messages.iter().any(|msg| msg.role.starts_with("muse:")
                && msg.model.as_deref() == Some(crate::muse::MODEL)
                && msg.effort.as_deref() == Some("high")));
            assert!(
                m.execution
                    .as_ref()
                    .unwrap()
                    .tasks
                    .iter()
                    .find(|task| task.task_id == "muse")
                    .unwrap()
                    .summary
                    .contains(&context_token),
                "Muse lost its native conversation context"
            );
            assert!(
                store
                    .mailbox(m.id)
                    .unwrap()
                    .iter()
                    .any(|msg| msg.from_task == "muse"),
                "Muse never used the MCP mailbox"
            );
            assert!(!project.join("muse.sh").exists());
            drop(workers);
        }));
        crate::sandbox::delete(&root).unwrap();
        if let Err(error) = outcome {
            std::panic::resume_unwind(error);
        }
    }

    fn final_plan() -> Value {
        json!({"summary":"Add greeting", "tasks":[{"id":"1","title":"Greeting flag",
            "outcome":"Print the requested greeting", "files":["src/main.rs"],
            "depends_on":[],"worker":"codex","checks":["Run greeting flag test"]}]})
    }

    fn planner() -> (TestData, Store, CodeMod, Worker) {
        let data = TestData::new();
        let mut store = data.store();
        let project = store.load_project(Path::new("/planner-project")).unwrap();
        let code_mod = store.create_mod(project.id, "Add a greeting flag").unwrap();
        let record = store.worker_for(code_mod.id, Role::Planner).unwrap();
        let worker = Worker::start(
            Path::new("/planner-project"),
            &code_mod,
            record,
            Role::Planner,
            None,
        )
        .unwrap();
        (data, store, code_mod, worker)
    }

    #[test]
    fn review_completion_preserves_execution_and_cannot_relabel_a_newer_review() {
        let (data, mut store, mut m, _) = crate::review::tests::fixture();
        let fingerprint = m.execution.as_ref().unwrap().fingerprint.clone().unwrap();
        let checks = serde_json::to_string(&m.execution.as_ref().unwrap().checks).unwrap();
        store.begin_review(m.id, &fingerprint).unwrap();
        let old = store.review_state(m.id).unwrap().unwrap().source;
        let record = store.worker_for(m.id, Role::Reviewer).unwrap();
        let mut worker =
            Worker::start(&data.0.join("project"), &m, record, Role::Planner, None).unwrap();
        worker.role = Role::Reviewer;
        worker.task_source = Some(old);
        worker.task_report =
            Some(r#"{"status":"clean","summary":"Inspected source.","findings":[]}"#.into());
        store.begin_review(m.id, &fingerprint).unwrap();
        m.agent_review = store.review_state(m.id).unwrap();
        let current = m.agent_review.as_ref().unwrap().source.clone();
        worker.complete_review(&mut store, &mut m, true).unwrap();
        assert_eq!(m.agent_review.as_ref().unwrap().status, "pending");
        assert!(!m.messages.iter().any(|m| m.role == "reviewer"));
        worker.task_source = Some(current.clone());
        worker.complete_review(&mut store, &mut m, false).unwrap();
        assert_eq!(m.agent_review.as_ref().unwrap().status, "paused");
        assert!(m.has_worker_history());
        store.begin_review(m.id, &fingerprint).unwrap();
        m.agent_review = store.review_state(m.id).unwrap();
        worker.task_source = Some(m.agent_review.as_ref().unwrap().source.clone());
        worker.complete_review(&mut store, &mut m, true).unwrap();
        assert_eq!(m.agent_review.as_ref().unwrap().status, "clean");
        let execution = store.execution(m.id).unwrap().unwrap();
        assert!(execution.complete());
        assert_eq!(execution.fingerprint.as_deref(), Some(fingerprint.as_str()));
        assert_eq!(serde_json::to_string(&execution.checks).unwrap(), checks);
    }

    #[test]
    fn peer_delivery_is_not_a_user_message_and_waiting_gates_completion() {
        let (data, mut store, mut m, ids, _) = crate::mailbox::tests::fixture();
        let record = store.worker_at(m.id, Role::Executor, 1).unwrap();
        let mut worker =
            Worker::start(&data.0.join("project"), &m, record, Role::Planner, None).unwrap();
        worker.role = Role::Executor;
        worker.status = Status::Running;
        let id =
            crate::mailbox::tests::send(&mut store, m.id, ids[0], "b", "ask", None, "contract");
        let input = store.next_mail(m.id, ids[1]).unwrap().unwrap();
        store.pending(ids[1], Some(&input.source)).unwrap();
        worker.pending = Some(input.clone());
        worker
            .receive(
                Event::Accepted {
                    source: input.source.clone(),
                    turn: "turn-b".into(),
                },
                &mut store,
                &mut m,
            )
            .unwrap();
        worker.item(&store, &mut m, &json!({"type":"userMessage","id":"peer","clientId":input.source,"text":input.texts[0]}), false).unwrap();
        assert_eq!(
            store.load_project(&data.0.join("project")).unwrap().mods[0]
                .messages
                .len(),
            2
        );
        assert_eq!(m.coordination[0].delivered, Some(ids[1]));
        assert!(m.coordination[0].acknowledged.is_none());
        assert_eq!(m.coordination[0].id, id);
        crate::mailbox::tests::send(&mut store, m.id, ids[1], "user", "ask", None, "decision");
        worker.task_source = Some(m.execution.as_ref().unwrap().tasks[1].source.clone());
        worker.complete_task(&mut store, &mut m).unwrap();
        assert_eq!(m.execution.as_ref().unwrap().tasks[1].status, "waiting");
        assert!(!worker.enabled && worker.error.is_none());
        assert!(!m.execution.as_ref().unwrap().complete());
    }

    #[test]
    #[ignore = "runs a natural to-do request from SPROWT_TEST_PROJECT with Astra and two VM workers"]
    fn natural_request_plans_parallel_work_and_coordinates_without_user_instructions() {
        use crate::store::test_support::TestData;
        use std::{
            fs,
            time::{Duration, Instant},
        };

        let source = std::path::PathBuf::from(
            std::env::var("SPROWT_TEST_PROJECT").expect("Set SPROWT_TEST_PROJECT to the to-do app"),
        );
        assert!(source.join("app/main.py").is_file());
        let data = TestData::new();
        let project = data.0.join("project");
        let root = data.0.join("workspace");
        let source_before = workspace::source_state(&source).unwrap();
        fs::create_dir_all(&project).unwrap();
        for (path, bytes, _) in &source_before {
            let target = project.join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, bytes).unwrap();
        }
        let before = workspace::source_state(&project).unwrap();
        let mut store = data.store();
        let project_id = store.load_project(&project).unwrap().id;
        let goal = "let me mark tasks as done and undo that later. keep completed tasks visible, and remember their state after reloading.";
        let mut m = store.create_mod(project_id, goal).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut planner = Worker::start(
                &project,
                &m,
                store.worker_for(m.id, Role::Planner).unwrap(),
                Role::Planner,
                None,
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(240);
            while Instant::now() < deadline && m.planning.as_ref().unwrap().plan.is_none() {
                planner.poll(&mut store, &mut m, true, &project).unwrap();
                assert!(
                    planner.error.is_none(),
                    "{}",
                    planner.error.as_deref().unwrap_or("")
                );
                std::thread::sleep(Duration::from_millis(50));
            }
            drop(planner);
            let plan = m
                .planning
                .as_ref()
                .unwrap()
                .plan
                .clone()
                .expect("Planner timed out");
            assert!(
                before == workspace::source_state(&project).unwrap(),
                "Planning changed source"
            );
            let ready: Vec<_> = plan
                .tasks
                .iter()
                .filter(|task| task.depends_on.is_empty())
                .collect();
            assert!(
                ready.len() >= 2,
                "No concurrent tasks: {}",
                serde_json::to_string(&plan).unwrap()
            );
            assert!(
                ready.iter().any(|task| !plan.peers(task).is_empty()),
                "Related components lack coordination context"
            );
            for task in &plan.tasks {
                eprintln!(
                    "Plan: {} · {} · after {:?} · {} peer links",
                    task.id,
                    task.title,
                    task.depends_on,
                    plan.peers(task).len()
                );
            }
            workspace::create(&project, &root).unwrap();
            store.create_execution(m.id, &root, &plan).unwrap();
            m.execution = store.execution(m.id).unwrap();
            let records = store
                .schedule_workers(m.id, &plan, &[], &[], &["codex"])
                .unwrap();
            m.execution = store.execution(m.id).unwrap();
            let mut workers: Vec<_> = records
                .into_iter()
                .map(|record| Worker::start(&project, &m, record, Role::Executor, None).unwrap())
                .collect();
            let deadline = Instant::now() + Duration::from_secs(1800);
            let mut overlap = false;
            let mut previous = String::new();
            while Instant::now() < deadline && m.execution.as_ref().unwrap().status != "review" {
                let busy = workers
                    .iter()
                    .filter(|w| w.busy())
                    .map(|w| w.id)
                    .collect::<Vec<_>>();
                let records = store
                    .schedule_workers(m.id, &plan, &busy, &[], &["codex"])
                    .unwrap();
                m.execution = store.execution(m.id).unwrap();
                for record in records {
                    if let Some(worker) = workers.iter_mut().find(|w| w.id == record.id) {
                        if !worker.enabled
                            && matches!(worker.status, Status::Ready | Status::Complete)
                        {
                            worker.toggle();
                        }
                    } else {
                        workers.push(
                            Worker::start(&project, &m, record, Role::Executor, None).unwrap(),
                        );
                    }
                }
                for worker in &mut workers {
                    worker.poll(&mut store, &mut m, true, &project).unwrap();
                    assert!(
                        worker.error.is_none(),
                        "{}",
                        worker.error.as_deref().unwrap_or("")
                    );
                }
                let execution = m.execution.as_ref().unwrap();
                overlap |= execution
                    .tasks
                    .iter()
                    .filter(|t| t.status == "running")
                    .count()
                    >= 2;
                let state = execution
                    .tasks
                    .iter()
                    .map(|t| format!("{}: {}", t.task_id, t.status))
                    .collect::<Vec<_>>()
                    .join("; ");
                let state = format!("{state}; {} messages", store.mailbox(m.id).unwrap().len());
                if state != previous {
                    eprintln!("Execution: {state}");
                    previous = state;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let execution = m.execution.as_ref().unwrap();
            assert_eq!(
                execution.status,
                "review",
                "{}",
                execution
                    .tasks
                    .iter()
                    .map(|t| format!("{}: {} · {}", t.task_id, t.status, t.summary))
                    .collect::<Vec<_>>()
                    .join("; ")
            );
            assert!(overlap, "Implementation never overlapped");
            let messages = store.mailbox(m.id).unwrap();
            assert!(
                messages
                    .iter()
                    .any(|m| m.to_task != "user" && m.acknowledged.is_some()),
                "No acknowledged peer communication"
            );
            assert!(
                !messages.iter().any(|m| m.needs_user()),
                "The request needed artificial clarification"
            );
            for message in &messages {
                eprintln!(
                    "Peer {} → {} · {} · {}",
                    message.from_task, message.to_task, message.kind, message.body
                );
            }
            for task in &execution.tasks {
                eprintln!("Result: {} · {}", task.task_id, task.summary);
            }
            assert!(
                source_before == workspace::source_state(&source).unwrap(),
                "Live project changed"
            );
        }));
        crate::sandbox::delete(&root).unwrap();
        result.unwrap();
    }

    #[test]
    #[ignore = "runs two cooperating Codex workers in one temporary Apple Container VM"]
    fn codex_workers_exchange_messages_and_resume_waiting_work() {
        use std::{
            fs,
            time::{Duration, Instant},
        };
        let (data, mut store, mut m, ids, mut plan) = crate::mailbox::tests::fixture();
        plan.tasks[0].outcome = "Use send_worker_message to ask task b which label to use, kind ask, key label-choice. No label is available until b replies. Read and acknowledge its reply, then write that exact label to a.txt. If unanswered, return blocked without writing a guessed label. Do not poll, install anything or touch other files.".into();
        plan.tasks[0].checks = vec!["A contains sprowt-peer-42".into()];
        plan.tasks[1].outcome = "Read your saved inbox. A asks which label to use. Acknowledge its ask, reply with exactly sprowt-peer-42 using send_worker_message kind reply, key label-answer, and its reply_to ID. Then write sprowt-peer-42 to b.txt. Do not install anything or touch other files.".into();
        plan.tasks[1].checks = vec!["B contains sprowt-peer-42".into()];
        store
            .save_plan(m.id, &m.planning.as_ref().unwrap().source, &plan)
            .unwrap();
        for run in &m.execution.as_ref().unwrap().tasks {
            store
                .finish_task(m.id, &run.source, "paused", "", &[])
                .unwrap();
        }
        store.retry_tasks(m.id).unwrap();
        m.planning = store.planning(m.id).unwrap();
        m.execution = store.execution(m.id).unwrap();
        let root = m.execution.as_ref().unwrap().workspace.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let project = data.0.join("project");
            let mut workers: Vec<_> = (0..2)
                .map(|slot| {
                    Worker::start(
                        &project,
                        &m,
                        store.worker_at(m.id, Role::Executor, slot).unwrap(),
                        Role::Executor,
                        None,
                    )
                    .unwrap()
                })
                .collect();
            let deadline = Instant::now() + Duration::from_secs(240);
            let mut waited = false;
            while Instant::now() < deadline && m.execution.as_ref().unwrap().status != "review" {
                workers[0].poll(&mut store, &mut m, true, &project).unwrap();
                waited |= m.execution.as_ref().unwrap().tasks[0].status == "waiting";
                workers[1]
                    .poll(&mut store, &mut m, waited, &project)
                    .unwrap();
                for worker in &workers {
                    assert!(
                        worker.error.is_none(),
                        "{}",
                        worker.error.as_deref().unwrap_or("")
                    );
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            assert!(waited, "A must pause until B's answer");
            assert_eq!(
                m.execution.as_ref().unwrap().status,
                "review",
                "Tasks: {}",
                m.execution
                    .as_ref()
                    .unwrap()
                    .tasks
                    .iter()
                    .map(|t| format!("{}: {} · {}", t.task_id, t.status, t.summary))
                    .collect::<Vec<_>>()
                    .join("; ")
            );
            let messages = store.mailbox(m.id).unwrap();
            let ask = messages
                .iter()
                .find(|m| m.kind == "ask" && m.to_task == "b")
                .unwrap();
            let reply = messages
                .iter()
                .find(|m| m.reply_to == Some(ask.id))
                .unwrap();
            assert!(ask.answered && ask.acknowledged == Some(ids[1]));
            assert_eq!(reply.acknowledged, Some(ids[0]));
            for file in ["a.txt", "b.txt"] {
                assert_eq!(
                    fs::read_to_string(root.join("work").join(file))
                        .unwrap()
                        .trim(),
                    "sprowt-peer-42"
                );
            }
        }));
        crate::sandbox::delete(&root).unwrap();
        result.unwrap();
    }

    fn accept(worker: &mut Worker, store: &mut Store, code_mod: &mut CodeMod) {
        let input = store.planner_input(code_mod.id).unwrap().unwrap();
        store.pending(worker.id, Some(&input.source)).unwrap();
        worker.pending = Some(input.clone());
        worker
            .receive(
                Event::Accepted {
                    source: input.source,
                    turn: "turn-1".into(),
                },
                store,
                code_mod,
            )
            .unwrap();
    }

    #[test]
    fn task_routing_saves_scoped_choices_and_updates_live_reply_labels() {
        let (data, mut store, mut code_mod, mut worker) = executor();
        worker.status = Status::Ready;
        let plan = code_mod.planning.as_ref().unwrap().plan.as_ref().unwrap();
        store
            .planning_model(code_mod.id, &Selection::planner())
            .unwrap();
        let input = store
            .task_input(code_mod.id, worker.id, plan)
            .unwrap()
            .unwrap();
        code_mod.execution = store.execution(code_mod.id).unwrap();
        let state = worker.task_context(&code_mod, &input.source).unwrap();
        assert_eq!(state["task"]["id"], plan.tasks[0].id);
        assert!(!state["task"]["checks"].as_array().unwrap().is_empty());
        let mut selection = Selection::fallback("Jev uncertain");
        selection.evidence = Some(serde_json::json!({"policy":"test","state":state}));
        assert!(
            store
                .task_model(code_mod.id, worker.id + 1, &input.source, &selection)
                .is_err()
        );
        worker
            .receive(
                Event::TaskConfigured {
                    source: input.source,
                    selection,
                },
                &mut store,
                &mut code_mod,
            )
            .unwrap();
        assert!(worker.label(None).contains("gpt-6.1-sol · xhigh"));
        assert_eq!(
            store
                .planning(code_mod.id)
                .unwrap()
                .unwrap()
                .model
                .as_deref(),
            Some("gpt-6-astra")
        );
        worker.item(&store, &mut code_mod, &serde_json::json!({"type":"agentMessage","phase":"commentary","id":"routed-reply","text":"Task implemented"}),false).unwrap();
        drop(store);
        let reopened = data.store();
        let execution = reopened.execution(code_mod.id).unwrap().unwrap();
        let saved = execution.tasks[0].selection.as_ref().unwrap();
        assert_eq!(saved.effort, "xhigh");
        assert_eq!(saved.evidence.as_ref().unwrap()["policy"], "test");
        let state = reopened
            .load_project(Path::new("/planner-project"))
            .unwrap();
        let reply = state.mods[0]
            .messages
            .iter()
            .find(|message| message.item_id.as_deref() == Some("routed-reply"))
            .unwrap();
        assert_eq!(reply.model.as_deref(), Some("gpt-6.1-sol"));
        assert_eq!(reply.effort.as_deref(), Some("xhigh"));
    }

    #[test]
    fn current_check_progress_is_visible_only_while_busy() {
        let (_data, mut store, mut m, mut worker) = executor();
        worker.status = Status::Checking;
        worker.activity_started = Instant::now() - std::time::Duration::from_secs(125);
        worker
            .receive(
                Event::Preparing("Checking 2/4 · Review regression 1 · Preserve focus".into()),
                &mut store,
                &mut m,
            )
            .unwrap();
        assert_eq!(
            worker.progress(),
            Some("Checking 2/4 · Review regression 1 · Preserve focus")
        );
        assert!(worker.elapsed_label().starts_with("2m "));
        worker.status = Status::Complete;
        assert_eq!(worker.progress(), None);
    }

    fn executor() -> (TestData, Store, CodeMod, Worker) {
        let (data, mut store, mut code_mod, mut worker) = planner();
        let plan = Plan::parse(&final_plan().to_string()).unwrap();
        store
            .save_plan(
                code_mod.id,
                &code_mod.planning.as_ref().unwrap().source,
                &plan,
            )
            .unwrap();
        code_mod.planning = store.planning(code_mod.id).unwrap();
        let project = data.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let root = data.0.join("workspace");
        workspace::create(&project, &root).unwrap();
        store.create_execution(code_mod.id, &root, &plan).unwrap();
        code_mod.execution = store.execution(code_mod.id).unwrap();
        worker.role = Role::Executor;
        worker.status = Status::Checking;
        worker.task_source = Some(code_mod.execution.as_ref().unwrap().tasks[0].source.clone());
        worker.verification = vec![Check {
            task: None,
            check: "Run greeting flag test".into(),
            command: vec!["/usr/bin/true".into()],
        }];
        worker.verify_before = Some(workspace::source_state(&root.join("work")).unwrap());
        (data, store, code_mod, worker)
    }

    #[test]
    fn completion_requires_actual_complete_checks_and_unchanged_source() {
        for case in ["success", "failure", "missing", "source changed"] {
            let (_data, mut store, mut code_mod, mut worker) = executor();
            if case == "source changed" {
                std::fs::write(
                    code_mod
                        .execution
                        .as_ref()
                        .unwrap()
                        .workspace
                        .join("work/unverified.txt"),
                    "changed",
                )
                .unwrap();
            }
            let checks = if case == "missing" {
                vec![]
            } else {
                vec![crate::execution::CheckResult {
                    missing_runtime: None,
                    task: None,
                    check: worker.verification[0].check.clone(),
                    command: worker.verification[0].command.clone(),
                    exit_code: if case == "source changed" {
                        None
                    } else {
                        Some(if case == "failure" { 1 } else { 0 })
                    },
                    output: String::new(),
                }]
            };
            worker
                .receive(
                    Event::Checked {
                        source: worker.task_source.clone().unwrap(),
                        checks,
                        before: worker.verify_before.clone().unwrap(),
                    },
                    &mut store,
                    &mut code_mod,
                )
                .unwrap();
            assert_eq!(
                code_mod.execution.as_ref().unwrap().tasks[0].status,
                if case == "success" { "done" } else { "blocked" }
            );
            assert_ne!(code_mod.execution.as_ref().unwrap().status, "review");
        }
    }

    #[test]
    fn failed_verification_returns_evidence_to_its_owner_once_across_restart() {
        let (data, mut store, mut m, mut worker) = executor();
        worker.id = store.worker_for(m.id, Role::Executor).unwrap().id;
        let id = m.execution.as_ref().unwrap().tasks[0].id;
        store
            .0
            .execute(
                "UPDATE task_runs SET worker_id=?2,status='checking' WHERE id=?1",
                rusqlite::params![id, worker.id],
            )
            .unwrap();
        m.execution = store.execution(m.id).unwrap();
        worker.task_summary = "All checks passed".into();
        let source = worker.task_source.clone().unwrap();
        let checked = |source: String| Event::Checked {
            source,
            checks: vec![crate::execution::CheckResult {
                missing_runtime: None,
                task: Some(id),
                check: worker.verification[0].check.clone(),
                command: worker.verification[0].command.clone(),
                exit_code: Some(1),
                output: "Warning: legacy dependency\nTraceback\nAssertionError: Save not finished"
                    .into(),
            }],
            before: workspace::source_state(&m.execution.as_ref().unwrap().workspace.join("work"))
                .unwrap(),
        };
        let first = checked(source.clone());
        let duplicate = checked(source.clone());
        worker.receive(first, &mut store, &mut m).unwrap();
        worker.receive(duplicate, &mut store, &mut m).unwrap();
        let run = &m.execution.as_ref().unwrap().tasks[0];
        assert_eq!(run.status, "pending");
        assert_eq!(run.worker, Some(worker.id));
        assert_ne!(run.source, source);
        assert_eq!(run.verification_feedback.len(), 1);
        assert!(worker.enabled && worker.error.is_none());
        assert!(
            !m.messages
                .last()
                .unwrap()
                .body
                .contains("All checks passed")
        );
        let next = run.source.clone();
        drop(store);
        let mut store = data.store();
        let input = store.submission(m.id, &next).unwrap();
        assert!(input.texts[0].contains("AssertionError: Save not finished"));
        assert!(input.texts[0].contains("before triggering an action"));
        assert!(input.texts[0].contains("do not skip failures or weaken assertions"));
        worker.task_source = Some(next.clone());
        worker.status = Status::Checking;
        let result = Event::Checked {
            source: next.clone(),
            checks: m.execution.as_ref().unwrap().tasks[0]
                .verification_feedback
                .clone(),
            before: worker.verify_before.clone().unwrap(),
        };
        worker.receive(result, &mut store, &mut m).unwrap();
        assert_eq!(m.execution.as_ref().unwrap().tasks[0].status, "blocked");
        assert!(!worker.enabled);
        assert!(worker.error.as_ref().unwrap().contains("Save not finished"));
        store.retry_tasks(m.id).unwrap();
        let source = store.execution(m.id).unwrap().unwrap().tasks[0]
            .source
            .clone();
        store
            .finish_task(
                m.id,
                &source,
                "blocked",
                "Still failed",
                &m.execution.as_ref().unwrap().tasks[0].checks,
            )
            .unwrap();
        assert!(
            !store
                .recover_verification(m.id, &source, worker.id)
                .unwrap()
        );
        assert!(!store.recover_verification(m.id, &next, worker.id).unwrap());
    }

    #[test]
    fn interrupted_stopped_or_mismatched_checks_never_start_recovery() {
        for case in ["stopped", "interrupted", "mismatch", "wrong owner"] {
            let (_data, mut store, mut m, mut worker) = executor();
            let id = m.execution.as_ref().unwrap().tasks[0].id;
            store
                .0
                .execute(
                    "UPDATE task_runs SET worker_id=?2,status='checking' WHERE id=?1",
                    rusqlite::params![id, worker.id],
                )
                .unwrap();
            if case == "stopped" {
                worker.enabled = false;
            }
            if case == "wrong owner" {
                worker.id += 1;
            }
            worker
                .receive(
                    Event::Checked {
                        source: worker.task_source.clone().unwrap(),
                        checks: vec![crate::execution::CheckResult {
                            missing_runtime: None,
                            task: Some(id),
                            check: worker.verification[0].check.clone(),
                            command: if case == "mismatch" {
                                vec!["/bin/false".into()]
                            } else {
                                worker.verification[0].command.clone()
                            },
                            exit_code: if case == "interrupted" { None } else { Some(1) },
                            output: "AssertionError: failed".into(),
                        }],
                        before: worker.verify_before.clone().unwrap(),
                    },
                    &mut store,
                    &mut m,
                )
                .unwrap();
            assert_eq!(
                m.execution.as_ref().unwrap().tasks[0].status,
                "blocked",
                "{case}"
            );
            assert!(!worker.enabled, "{case}");
        }
    }

    fn reported_task() -> (TestData, Store, CodeMod, Worker, Value) {
        let (data, mut store, mut code_mod, mut worker) = executor();
        let mut plan = code_mod.planning.as_ref().unwrap().plan.clone().unwrap();
        plan.tasks[0].checks = vec![
            "API passes".into(),
            "Runtime passes".into(),
            "Backfill passes".into(),
        ];
        store
            .save_plan(
                code_mod.id,
                &code_mod.planning.as_ref().unwrap().source,
                &plan,
            )
            .unwrap();
        code_mod.planning = store.planning(code_mod.id).unwrap();
        store
            .task_status(
                code_mod.id,
                worker.task_source.as_ref().unwrap(),
                "running",
                Some("task-turn"),
            )
            .unwrap();
        code_mod.execution = store.execution(code_mod.id).unwrap();
        worker.status = Status::Running;
        worker.turn = Some("task-turn".into());
        let report = json!({"status":"completed","summary":"Task implemented","repair":null,
            "checks":plan.tasks[0].checks.iter().map(|check| json!({"check":check,"command":["/bin/true"]})).collect::<Vec<_>>()});
        (data, store, code_mod, worker, report)
    }

    fn report_item(text: &Value) -> Value {
        json!({"type":"agentMessage","phase":"final_answer","text":text.to_string()})
    }

    #[test]
    fn multiple_commands_per_check_are_verified_and_partial_results_stay_incomplete() {
        for provider in ["codex", "muse"] {
            for case in ["pass", "extra fails", "interrupted"] {
                let (_data, mut store, mut m, mut worker, mut report) = reported_task();
                worker.provider = provider.into();
                worker.id = store
                    .worker_provider(m.id, Role::Executor, 0, provider)
                    .unwrap()
                    .id;
                store
                    .0
                    .execute(
                        "UPDATE task_runs SET worker_id=?1 WHERE mod_id=?2",
                        rusqlite::params![worker.id, m.id],
                    )
                    .unwrap();
                m.execution = store.execution(m.id).unwrap();
                let (client, actions) = Client::recording();
                worker.client = Some(client);
                let original = report["checks"][0].clone();
                for flag in ["extra-one", "extra-two"] {
                    let mut extra = original.clone();
                    extra["command"] = json!(["/bin/sh", "-c", "exit 0", flag]);
                    report["checks"].as_array_mut().unwrap().push(extra);
                }
                worker
                    .item(&store, &mut m, &report_item(&report), false)
                    .unwrap();
                worker.complete_task(&mut store, &mut m).unwrap();
                let Action::Verify { source, checks } = actions.try_recv().unwrap() else {
                    panic!("Every reported command must be independently verified")
                };
                assert_eq!(checks.len(), 5);
                let count = if case == "pass" { 5 } else { 2 };
                let results = checks
                    .into_iter()
                    .take(count)
                    .enumerate()
                    .map(|(i, c)| crate::execution::CheckResult {
                        missing_runtime: None,
                        task: c.task,
                        check: c.check,
                        command: c.command,
                        exit_code: Some(if case == "extra fails" && i == 1 {
                            1
                        } else {
                            0
                        }),
                        output: if case == "extra fails" && i == 1 {
                            "AssertionError: extra check failed".into()
                        } else {
                            String::new()
                        },
                    })
                    .collect();
                worker
                    .receive(
                        Event::Checked {
                            source,
                            checks: results,
                            before: workspace::source_state(
                                &m.execution.as_ref().unwrap().workspace.join("work"),
                            )
                            .unwrap(),
                        },
                        &mut store,
                        &mut m,
                    )
                    .unwrap();
                let execution = store.execution(m.id).unwrap().unwrap();
                let run = &execution.tasks[0];
                let saved = if case == "extra fails" {
                    assert_eq!(run.status, "pending");
                    assert_eq!(run.verification_feedback[1].exit_code, Some(1));
                    &run.verification_feedback
                } else {
                    assert_eq!(run.status, if case == "pass" { "done" } else { "blocked" });
                    &run.checks
                };
                assert_eq!(saved.len(), 5);
                assert!(saved[count..].iter().all(|c| c.exit_code.is_none()));
                if case == "pass" {
                    assert_eq!(
                        execution.check_count(m.planning.as_ref().unwrap().plan.as_ref().unwrap()),
                        5
                    );
                    worker.turn = None;
                    worker
                        .poll(&mut store, &mut m, true, Path::new("/planner-project"))
                        .unwrap();
                    let Action::Verify { source, checks } = actions.try_recv().unwrap() else {
                        panic!("Combined verification must retain every command")
                    };
                    assert!(source.starts_with("final:"));
                    assert_eq!(checks.len(), 5);
                }
            }
        }
    }

    #[test]
    fn late_handoff_ack_keeps_checks_live_and_on_recovery() {
        for provider in ["codex", "muse"] {
            for historical in [false, true] {
                for exit_code in [0, 1] {
                    let (_data, mut store, mut m, mut worker, report) = reported_task();
                    worker.provider = provider.into();
                    let (client, actions) = Client::recording();
                    worker.client = Some(client);
                    let source = worker.task_source.clone().unwrap();
                    let items = vec![
                        json!({"type":"userMessage","clientId":source,"content":[]}),
                        report_item(&report),
                        json!({"type":"userMessage","clientId":"00000005-0000-0000-0000-000000000001","content":[]}),
                        json!({"type":"agentMessage","phase":"commentary","text":"Acknowledging the handoff"}),
                        report_item(
                            &json!({"status":"completed","summary":"Handoff acknowledged","checks":[],"repair":null}),
                        ),
                    ];
                    if historical {
                        worker.task_source = None;
                        worker.recover_task(&mut store, &mut m, &json!({"turns":[{"id":"task-turn","status":"completed","items":items}]})).unwrap();
                    } else {
                        for item in items {
                            if provider == "muse"
                                && item["clientId"]
                                    .as_str()
                                    .is_some_and(|id| id.starts_with("00000005-"))
                            {
                                worker
                                    .receive(
                                        Event::Accepted {
                                            source: item["clientId"].as_str().unwrap().into(),
                                            turn: "task-turn".into(),
                                        },
                                        &mut store,
                                        &mut m,
                                    )
                                    .unwrap();
                                continue;
                            }
                            worker.item(&store, &mut m, &item, false).unwrap();
                        }
                        worker.receive(Event::Notification(json!({"method":"turn/completed","params":{"turn":{"id":"task-turn","status":"completed"}}})), &mut store, &mut m).unwrap();
                    }
                    assert!(worker.status == Status::Checking);
                    assert_eq!(m.execution.as_ref().unwrap().tasks[0].status, "checking");
                    let Action::Verify {
                        source: verified_source,
                        checks,
                    } = actions.try_recv().unwrap()
                    else {
                        panic!("Task must go through independent verification")
                    };
                    assert_eq!(verified_source, source);
                    assert_eq!(checks.len(), 3);
                    worker
                        .receive(
                            Event::Checked {
                                source,
                                before: worker.verify_before.clone().unwrap_or_else(|| {
                                    workspace::source_state(
                                        &m.execution.as_ref().unwrap().workspace.join("work"),
                                    )
                                    .unwrap()
                                }),
                                checks: checks
                                    .into_iter()
                                    .map(|check| crate::execution::CheckResult {
                                        missing_runtime: None,
                                        task: check.task,
                                        check: check.check,
                                        command: check.command,
                                        exit_code: Some(exit_code),
                                        output: String::new(),
                                    })
                                    .collect(),
                            },
                            &mut store,
                            &mut m,
                        )
                        .unwrap();
                    assert_eq!(
                        m.execution.as_ref().unwrap().tasks[0].status,
                        if exit_code == 0 { "done" } else { "blocked" }
                    );
                    assert_eq!(
                        m.execution.as_ref().unwrap().tasks[0].summary,
                        "Task implemented"
                    );
                }
            }
        }
    }

    #[test]
    fn handoff_ack_never_supplies_missing_task_checks() {
        for case in [
            "missing",
            "invalid",
            "no mail",
            "accepted steering",
            "replayed steering",
        ] {
            let (_data, mut store, mut m, mut worker, mut report) = reported_task();
            let (client, actions) = Client::recording();
            worker.client = Some(client);
            if case == "invalid" {
                report["checks"].as_array_mut().unwrap().pop();
            }
            if case != "missing" {
                worker
                    .item(&store, &mut m, &report_item(&report), false)
                    .unwrap();
            }
            if case != "no mail" {
                worker
                    .item(
                        &store,
                        &mut m,
                        &json!({"type":"userMessage","clientId":"00000005-peer"}),
                        false,
                    )
                    .unwrap();
            }
            if case == "accepted steering" {
                worker
                    .receive(
                        Event::Accepted {
                            source: "00000002-user".into(),
                            turn: "task-turn".into(),
                        },
                        &mut store,
                        &mut m,
                    )
                    .unwrap();
            }
            if case == "replayed steering" {
                worker
                    .item(
                        &store,
                        &mut m,
                        &json!({"type":"userMessage","clientId":"00000002-user","content":[]}),
                        true,
                    )
                    .unwrap();
            }
            worker.item(&store, &mut m, &report_item(&json!({"status":"completed","summary":"Acknowledged","checks":[],"repair":null})), false).unwrap();
            worker.complete_task(&mut store, &mut m).unwrap();
            let run = &m.execution.as_ref().unwrap().tasks[0];
            assert_eq!(run.status, "blocked", "{case}");
            assert_eq!(
                run.summary,
                "Task returned 0 completion checks; expected 3."
            );
            assert!(actions.try_recv().is_err());
        }
    }

    #[test]
    fn later_full_reports_and_blockers_replace_prior_success() {
        for status in ["completed", "blocked"] {
            let (_data, mut store, mut m, mut worker, mut report) = reported_task();
            let (client, actions) = Client::recording();
            worker.client = Some(client);
            worker
                .item(&store, &mut m, &report_item(&report), false)
                .unwrap();
            worker
                .item(
                    &store,
                    &mut m,
                    &json!({"type":"userMessage","clientId":"00000005-peer"}),
                    false,
                )
                .unwrap();
            report["status"] = json!(status);
            report["summary"] = json!("Updated task result");
            for check in report["checks"].as_array_mut().unwrap() {
                check["command"] = json!(["/bin/false"]);
            }
            worker
                .item(&store, &mut m, &report_item(&report), false)
                .unwrap();
            worker.item(&store, &mut m, &report_item(&json!({"status":"completed","summary":"Acknowledged","checks":[],"repair":null})), false).unwrap();
            worker.complete_task(&mut store, &mut m).unwrap();
            if status == "blocked" {
                assert_eq!(m.execution.as_ref().unwrap().tasks[0].status, "blocked");
                assert_eq!(
                    m.execution.as_ref().unwrap().tasks[0].summary,
                    "Updated task result"
                );
                assert!(actions.try_recv().is_err());
            } else {
                assert_eq!(worker.task_summary, "Updated task result");
                let Action::Verify { checks, .. } = actions.try_recv().unwrap() else {
                    panic!("Expected fresh verification")
                };
                assert!(checks.iter().all(|check| check.command == ["/bin/false"]));
            }
        }
    }

    #[test]
    fn integration_report_requests_repair_without_claiming_completion() {
        let (_data, mut store, mod_id, _plan, ids, _) = crate::repair::tests::fixture();
        let mut code_mod = store
            .load_project(Path::new("/repair-test"))
            .unwrap()
            .mods
            .remove(0);
        let record = store.worker_at(mod_id, Role::Executor, 0).unwrap();
        let mut worker = Worker::start(
            Path::new("/repair-test"),
            &code_mod,
            record,
            Role::Planner,
            None,
        )
        .unwrap();
        worker.role = Role::Executor;
        worker.status = Status::Ready;
        worker.task_source = Some(crate::store::task_source(ids[2], 1));
        let checks = &worker.current_task(&code_mod).unwrap().checks;
        let success = json!({"status":"completed","summary":"Previously passed","repair":null,
            "checks":checks.iter().map(|check| json!({"check":check,"command":["/bin/true"]})).collect::<Vec<_>>()});
        worker
            .item(&store, &mut code_mod, &report_item(&success), false)
            .unwrap();
        let repair = json!({"status":"blocked","summary":"Retry is broken","checks":[],"repair":{
            "task":"runtime","files":["app/runtime.js"],"check":"Retry works",
            "command":["/usr/bin/node","/tasks/3/tests/retry.mjs"],"evidence":"Rejected download leaves Loading disabled."
        }});
        worker
            .item(&store, &mut code_mod, &report_item(&repair), false)
            .unwrap();
        worker
            .item(
                &store,
                &mut code_mod,
                &json!({"type":"userMessage","clientId":"00000005-peer"}),
                false,
            )
            .unwrap();
        worker.item(&store, &mut code_mod, &report_item(&json!({"status":"completed","summary":"Acknowledged","checks":[],"repair":null})), false).unwrap();
        worker.complete_task(&mut store, &mut code_mod).unwrap();
        let execution = code_mod.execution.as_ref().unwrap();
        assert_eq!(execution.tasks[2].status, "repair_wait");
        assert!(!execution.complete());
        assert!(worker.task_source.is_none() && !worker.enabled && worker.error.is_none());
        assert!(
            code_mod
                .messages
                .iter()
                .any(|message| message.role == "harness"
                    && message.body.contains("Repair requested"))
        );
        assert!(store.resume_repairs(mod_id).unwrap());
    }

    #[test]
    #[ignore = "runs a Codex integration regression and a Muse repair in a disposable VM"]
    fn codex_verifier_hands_a_regression_back_to_muse() {
        use std::{
            fs,
            os::unix::fs::PermissionsExt,
            sync::atomic::AtomicBool,
            time::{Duration, Instant},
        };
        let data = TestData::new();
        let project = data.0.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join("greet.sh"),
            "#!/bin/sh\nprintf 'hello %s\\n' \"${1-}\"\n",
        )
        .unwrap();
        fs::set_permissions(project.join("greet.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        let mut store = data.store();
        let project_id = store.load_project(&project).unwrap().id;
        let mut m=store.create_mod(project_id,"A greeting script must greet a supplied name and reject an empty name with a nonzero exit. Verify it and document usage.").unwrap();
        let plan=Plan::parse(&json!({"summary":"Verify greeting", "tasks":[
            {"id":"runtime","title":"Greeting runtime","outcome":"Greet supplied names and reject empty names","files":["greet.sh"],"depends_on":[],"worker":"muse","checks":["Passing a name prints the greeting."]},
            {"id":"integration","title":"Check empty input","outcome":"Verify empty input fails and document usage","files":["tests/empty.sh","README.md"],"depends_on":["runtime"],"worker":"codex","checks":["An empty name fails."]}
        ]}).to_string()).unwrap();
        store
            .save_plan(m.id, &m.planning.as_ref().unwrap().source, &plan)
            .unwrap();
        m.planning = store.planning(m.id).unwrap();
        let root = store.workspace_path(m.id).unwrap();
        workspace::create(&project, &root).unwrap();
        store.create_execution(m.id, &root, &plan).unwrap();
        let muse = store
            .worker_provider(m.id, Role::Executor, 0, "muse")
            .unwrap();
        let codex = store.worker_at(m.id, Role::Executor, 0).unwrap();
        m.execution = store.execution(m.id).unwrap();
        let runs = m.execution.as_ref().unwrap().tasks.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // A completed happy-path check can miss a later integration regression.
            let flag = AtomicBool::new(false);
            let mut vm = crate::sandbox::Sandbox::prepare(&root, &flag, |_| {}).unwrap();
            vm.prepare_tasks(&runs.iter().map(|run| run.id).collect::<Vec<_>>(), &flag)
                .unwrap();
            vm.assign_task(runs[0].id, muse.id, &flag).unwrap();
            let check = Check {
                task: Some(runs[0].id),
                check: plan.tasks[0].checks[0].clone(),
                command: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "test \"$(./greet.sh sprowt)\" = 'hello sprowt'".into(),
                ],
            };
            let (_, checks) = vm
                .verify_execution(&runs[0].source, &[check], &flag)
                .unwrap();
            assert_eq!(checks[0].exit_code, Some(0));
            store
                .0
                .execute(
                    "UPDATE task_runs SET worker_id=?2 WHERE id=?1",
                    rusqlite::params![runs[0].id, muse.id],
                )
                .unwrap();
            store
                .finish_task(m.id, &runs[0].source, "done", "Happy path passes", &checks)
                .unwrap();
            drop(vm);
            m.execution = store.execution(m.id).unwrap();
            let mut workers = vec![
                Worker::start_with_muse(&project, &m, muse, Role::Executor, None, true).unwrap(),
                Worker::start_with_muse(&project, &m, codex, Role::Executor, None, true).unwrap(),
            ];
            let deadline = Instant::now() + Duration::from_secs(360);
            let mut repaired = false;
            while Instant::now() < deadline {
                for worker in &mut workers {
                    worker.poll(&mut store, &mut m, true, &project).unwrap();
                }
                if workers
                    .iter()
                    .all(|worker| matches!(worker.status, Status::Ready | Status::Complete))
                    && store.resume_repairs(m.id).unwrap()
                {
                    repaired = true;
                    m.execution = store.execution(m.id).unwrap();
                    for worker in &mut workers {
                        if !worker.enabled {
                            worker.toggle();
                        }
                    }
                }
                let execution = m.execution.as_ref().unwrap();
                assert_ne!(
                    execution.status,
                    "blocked",
                    "{}",
                    workers
                        .iter()
                        .filter_map(|worker| worker.error.as_deref())
                        .collect::<Vec<_>>()
                        .join("; ")
                );
                if execution.status == "review" {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            drop(workers);
            assert!(repaired, "Verifier did not request a repair");
            let execution = store.execution(m.id).unwrap().unwrap();
            assert_eq!(execution.status, "review", "Repair did not finish");
            assert_eq!(execution.checks.len(), 2);
            assert!(
                execution
                    .checks
                    .iter()
                    .all(|check| check.exit_code == Some(0))
            );
            assert!(
                root.join("work/tests/empty.sh").exists() && root.join("work/README.md").exists()
            );
        }));
        crate::sandbox::delete(&root).unwrap();
        result.unwrap();
    }

    #[test]
    fn final_missing_runtime_returns_to_its_owner_once() {
        for provider in ["codex", "muse"] {
            let (data, mut store, mut m, mut verifier) = executor();
            let owner = store
                .worker_provider(m.id, Role::Executor, 0, provider)
                .unwrap();
            let id = m.execution.as_ref().unwrap().tasks[0].id;
            let source = m.execution.as_ref().unwrap().tasks[0].source.clone();
            let executable = format!("/tasks/{id}/.venv/bin/python");
            let mut check = crate::execution::CheckResult {
                task: Some(id),
                check: verifier.verification[0].check.clone(),
                command: vec![executable.clone()],
                exit_code: Some(0),
                output: String::new(),
                missing_runtime: None,
            };
            store
                .0
                .execute(
                    "UPDATE task_runs SET worker_id=?2 WHERE id=?1",
                    rusqlite::params![id, owner.id],
                )
                .unwrap();
            store
                .finish_task(m.id, &source, "done", "Passed", &[check.clone()])
                .unwrap();
            m.execution = store.execution(m.id).unwrap();
            verifier.verification = vec![Check {
                task: Some(id),
                check: check.check.clone(),
                command: check.command.clone(),
            }];
            verifier.task_source = Some(format!("final:{}", m.id));
            check.exit_code = None;
            check.output = "Check executable is unavailable.".into();
            check.missing_runtime = Some(executable);
            let event = || Event::Checked {
                source: format!("final:{}", m.id),
                checks: vec![check.clone()],
                before: workspace::source_state(
                    &m.execution.as_ref().unwrap().workspace.join("work"),
                )
                .unwrap(),
            };
            let first = event();
            let duplicate = event();
            verifier.receive(first, &mut store, &mut m).unwrap();
            verifier.receive(duplicate, &mut store, &mut m).unwrap();
            let run = &m.execution.as_ref().unwrap().tasks[0];
            assert_eq!(run.status, "pending");
            assert_eq!(run.worker, Some(owner.id));
            assert!(run.restoring_runtime());
            assert!(verifier.error.is_none());
            let next = run.source.clone();
            assert_ne!(next, source);
            assert_eq!(
                verifier.task_context(&m, &next).unwrap()["runtime_recovery"],
                true
            );
            drop(store);
            let mut store = data.store();
            let input = store.submission(m.id, &next).unwrap();
            assert!(input.texts[0].contains("Preserve the combined source, tests"));
            assert!(input.texts[0].contains("every declared check"));
            // A second missing-runtime result cannot reopen the task again, even after restart.
            store
                .finish_task(m.id, &next, "done", "Rebuilt", &[check.clone()])
                .unwrap();
            store
                .execution_checks(m.id, "blocked", &[check], None)
                .unwrap();
            assert!(!store.recover_runtime(m.id).unwrap());
            assert_eq!(store.execution(m.id).unwrap().unwrap().status, "blocked");
        }
    }

    #[test]
    fn final_runtime_recovery_rejects_stops_mismatches_and_assertions() {
        for case in [
            "stopped",
            "mismatch",
            "wrong task",
            "assertion",
            "source changed",
        ] {
            let (_data, mut store, mut m, mut worker) = executor();
            let id = m.execution.as_ref().unwrap().tasks[0].id;
            let source = m.execution.as_ref().unwrap().tasks[0].source.clone();
            let mut check = crate::execution::CheckResult {
                task: Some(id),
                check: worker.verification[0].check.clone(),
                command: vec!["/tasks/1/runtime".into()],
                exit_code: Some(0),
                output: String::new(),
                missing_runtime: None,
            };
            store
                .0
                .execute(
                    "UPDATE task_runs SET worker_id=?2 WHERE id=?1",
                    rusqlite::params![id, worker.id],
                )
                .unwrap();
            store
                .finish_task(m.id, &source, "done", "Passed", &[check.clone()])
                .unwrap();
            m.execution = store.execution(m.id).unwrap();
            worker.verification = vec![Check {
                task: Some(id),
                check: check.check.clone(),
                command: check.command.clone(),
            }];
            worker.task_source = Some(format!("final:{}", m.id));
            check.exit_code = None;
            check.output = "Check executable is unavailable.".into();
            check.missing_runtime = check.command.first().cloned();
            match case {
                "stopped" => worker.enabled = false,
                "mismatch" => check.command[0] = "/another/runtime".into(),
                "wrong task" => check.task = Some(id + 1),
                "assertion" => {
                    check.exit_code = Some(1);
                    check.missing_runtime = None;
                    check.output = "AssertionError".into();
                }
                "source changed" => std::fs::write(
                    m.execution.as_ref().unwrap().workspace.join("work/new.txt"),
                    "changed",
                )
                .unwrap(),
                _ => unreachable!(),
            }
            worker
                .receive(
                    Event::Checked {
                        source: worker.task_source.clone().unwrap(),
                        checks: vec![check],
                        before: worker.verify_before.clone().unwrap(),
                    },
                    &mut store,
                    &mut m,
                )
                .unwrap();
            assert_eq!(m.execution.as_ref().unwrap().status, "blocked", "{case}");
            assert_eq!(
                m.execution.as_ref().unwrap().tasks[0].status,
                "done",
                "{case}"
            );
            assert!(!worker.enabled, "{case}");
        }
    }

    #[test]
    fn final_checks_save_the_verified_snapshot_and_unconfirmed_delivery_stays_retryable() {
        let (_data, mut store, mut code_mod, mut worker) = executor();
        let source = worker.task_source.clone().unwrap();
        store
            .finish_task(code_mod.id, &source, "done", "Done", &[])
            .unwrap();
        code_mod.execution = store.execution(code_mod.id).unwrap();
        worker.task_source = Some(format!("final:{}", code_mod.id));
        let checks = vec![crate::execution::CheckResult {
            missing_runtime: None,
            task: None,
            check: worker.verification[0].check.clone(),
            command: worker.verification[0].command.clone(),
            exit_code: Some(0),
            output: String::new(),
        }];
        worker
            .receive(
                Event::Checked {
                    source: worker.task_source.clone().unwrap(),
                    checks,
                    before: worker.verify_before.clone().unwrap(),
                },
                &mut store,
                &mut code_mod,
            )
            .unwrap();
        assert_eq!(code_mod.execution.as_ref().unwrap().status, "review");
        assert!(code_mod.execution.as_ref().unwrap().fingerprint.is_some());
        store
            .task_status(code_mod.id, &source, "sending", None)
            .unwrap();
        code_mod.execution = store.execution(code_mod.id).unwrap();
        worker
            .recover_task(&mut store, &mut code_mod, &json!({"turns":[]}))
            .unwrap();
        assert!(worker.status == Status::Failed && !worker.enabled);
        assert_eq!(
            code_mod.execution.as_ref().unwrap().tasks[0].status,
            "paused"
        );
        assert!(
            store
                .task_input(
                    code_mod.id,
                    worker.id,
                    code_mod.planning.as_ref().unwrap().plan.as_ref().unwrap()
                )
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn muse_native_receipt_confirms_a_task_paused_after_losing_its_ack() {
        let (_data, mut store, mut m, mut worker) = executor();
        worker.provider = "muse".into();
        let source = worker.task_source.take().unwrap();
        store.task_status(m.id, &source, "paused", None).unwrap();
        store.pending(worker.id, Some(&source)).unwrap();
        m.execution = store.execution(m.id).unwrap();
        worker.recovery = Some(source.clone());
        worker.receive(Event::Ready { model:Some(crate::muse::MODEL.into()), effort:Some("high".into()),
            thread:json!({"id":"native-session", "turns":[{"id":"native-turn", "status":"cancelled", "items":[
                {"type":"userMessage", "clientId":source, "content":[{"type":"text", "text":"task"}]}]}]}) },
            &mut store, &mut m).unwrap();
        assert!(worker.recovery.is_none());
        assert_eq!(worker.thread_id.as_deref(), Some("native-session"));
        assert!(
            store
                .worker_for(m.id, Role::Planner)
                .unwrap()
                .pending
                .is_none()
        );
        assert_eq!(m.execution.as_ref().unwrap().tasks[0].status, "paused");
        assert_eq!(
            m.execution.as_ref().unwrap().tasks[0].turn.as_deref(),
            Some("native-turn")
        );
        assert!(!worker.enabled);
    }

    #[test]
    fn connection_failure_keeps_unknown_delivery_until_explicit_retry() {
        let (_data, mut store, mut code_mod, mut worker) = executor();
        let source = worker.task_source.take().unwrap();
        store
            .task_status(code_mod.id, &source, "sending", None)
            .unwrap();
        store.pending(worker.id, Some(&source)).unwrap();
        code_mod.execution = store.execution(code_mod.id).unwrap();
        worker.recovery = Some(source.clone());
        worker
            .receive(
                Event::Failed("no rollout found for thread id unknown".into()),
                &mut store,
                &mut code_mod,
            )
            .unwrap();
        assert_eq!(
            code_mod.execution.as_ref().unwrap().tasks[0].status,
            "paused"
        );
        assert_eq!(
            store
                .worker_for(code_mod.id, Role::Planner)
                .unwrap()
                .pending
                .as_deref(),
            Some(source.as_str())
        );
        assert!(!worker.enabled);
        store.retry_tasks(code_mod.id).unwrap();
        assert_ne!(
            store.execution(code_mod.id).unwrap().unwrap().tasks[0].source,
            source
        );
        assert!(
            store
                .worker_for(code_mod.id, Role::Planner)
                .unwrap()
                .pending
                .is_none()
        );
    }

    #[test]
    fn planner_saves_a_readable_plan_without_consuming_executor_instructions() {
        let (_data, mut store, mut code_mod, mut worker) = planner();
        let queued = store
            .enqueue(code_mod.id, "Inspect the implementation")
            .unwrap();
        code_mod.queue.push(queued);
        accept(&mut worker, &mut store, &mut code_mod);
        worker.receive(Event::Notification(json!({"method":"item/completed","params":{"item":{
            "id":"plan-item","type":"agentMessage","phase":"final_answer","text":final_plan().to_string()}}})),&mut store,&mut code_mod).unwrap();
        worker.receive(Event::Notification(json!({"method":"turn/completed","params":{"turn":{"id":"turn-1","status":"completed"}}})),&mut store,&mut code_mod).unwrap();
        assert!(worker.status == Status::Complete);
        assert!(!worker.enabled);
        assert_eq!(code_mod.messages.len(), 2);
        assert!(code_mod.messages[1].body.starts_with("plan ·"));
        assert_eq!(code_mod.queue.len(), 1);
        assert_eq!(
            store.planning(code_mod.id).unwrap().unwrap().status,
            "ready"
        );
    }

    #[test]
    fn incomplete_or_invalid_plans_remain_retryable() {
        let (_data, mut store, mut code_mod, mut worker) = planner();
        accept(&mut worker, &mut store, &mut code_mod);
        worker.final_plan = Some("invalid JSON".into());
        worker.receive(Event::Notification(json!({"method":"turn/completed","params":{"turn":{"id":"turn-1","status":"completed"}}})),&mut store,&mut code_mod).unwrap();
        assert!(worker.status == Status::Failed);
        assert_eq!(code_mod.messages.len(), 1);
        assert_eq!(
            store.planning(code_mod.id).unwrap().unwrap().status,
            "failed"
        );
        store.retry_plan(code_mod.id).unwrap();
        code_mod.planning = store.planning(code_mod.id).unwrap();
        worker.enabled = true;
        accept(&mut worker, &mut store, &mut code_mod);
        worker.receive(Event::Notification(json!({"method":"turn/completed","params":{"turn":{"id":"turn-1","status":"interrupted"}}})),&mut store,&mut code_mod).unwrap();
        assert_eq!(
            store.planning(code_mod.id).unwrap().unwrap().status,
            "paused"
        );
        assert!(!worker.enabled);
        assert_eq!(code_mod.messages.len(), 1);
    }

    #[test]
    fn resumes_a_completed_plan_after_missing_its_acknowledgment() {
        let (_data, mut store, mut code_mod, mut worker) = planner();
        let input = store.planner_input(code_mod.id).unwrap().unwrap();
        store.pending(worker.id, Some(&input.source)).unwrap();
        worker.recovery = Some(input.source.clone());
        worker.receive(Event::Ready { model:Some("planner-model".into()), effort:Some("high".into()), thread:json!({"id":"saved-thread", "turns":[{
            "id":"turn-1","status":"completed","items":[
                {"type":"userMessage","clientId":input.source,"content":[{"type":"text","text":"description"}]},
                {"id":"plan-item","type":"agentMessage","phase":"final_answer","text":final_plan().to_string()}
            ]}]}) },&mut store,&mut code_mod).unwrap();
        assert!(worker.recovery.is_none());
        assert!(worker.status == Status::Complete);
        assert_eq!(code_mod.messages.len(), 2);
        assert!(
            store
                .worker_for(code_mod.id, Role::Planner)
                .unwrap()
                .pending
                .is_none()
        );
    }

    #[test]
    fn task_messages_keep_their_owner_when_workers_are_reused_or_history_replayed() {
        let (data, store, mut code_mod, mut worker) = planner();
        worker.role = Role::Executor;
        for task in [101, 102] {
            worker.task_source = Some(crate::store::task_source(task, 1));
            worker.item(&store, &mut code_mod, &json!({"type":"agentMessage", "id":format!("task-{task}"), "phase":"commentary", "text":format!("Task {task} update")}), false).unwrap();
        }
        for task in [101, 102, 103] {
            worker.item(&store, &mut code_mod, &json!({"type":"agentMessage", "id":format!("task-{task}"), "phase":"commentary", "text":format!("Task {task} update")}), true).unwrap();
        }
        let saved = data
            .store()
            .load_project(Path::new("/planner-project"))
            .unwrap();
        for messages in [&code_mod.messages, &saved.mods[0].messages] {
            for (id, expected) in [(101, Some(101)), (102, Some(102)), (103, None)] {
                assert_eq!(
                    messages
                        .iter()
                        .find(|message| message.item_id.as_deref() == Some(&format!("task-{id}")))
                        .unwrap()
                        .task,
                    expected
                );
            }
        }
    }

    #[test]
    fn live_replies_save_model_and_effort_without_relabeling_history() {
        let (data, mut store, mut code_mod, mut worker) = planner();
        worker.role = Role::Executor;
        save_message(
            &store,
            &mut code_mod,
            Message {
                task: None,
                item_id: Some("model-only".into()),
                role: "codex".into(),
                body: "Reply saved before effort labels.".into(),
                model: Some("older-model".into()),
                effort: None,
            },
        )
        .unwrap();
        for (model, effort, id) in [
            ("first-model", "high", "reply-1"),
            ("second-model", "low", "reply-2"),
            ("second-model", "medium", "reply-3"),
        ] {
            worker.receive(Event::Ready { model:Some(model.into()), effort:Some(effort.into()), thread:json!({"id":"thread-1","turns":[{"items":[
                {"type":"agentMessage","id":"reply-1","text":"Earlier reply."},
                {"type":"agentMessage","id":"model-only","text":"Reply saved before effort labels."},
                {"type":"agentMessage","id":"legacy-reply","text":"Unknown earlier model."}
            ]}]}) }, &mut store, &mut code_mod).unwrap();
            worker
                .receive(
                    Event::Notification(json!({"method":"item/completed","params":{"item":{
                        "type":"agentMessage","id":id,"text":"Live reply."
                    }}})),
                    &mut store,
                    &mut code_mod,
                )
                .unwrap();
            assert_eq!(worker.model.as_deref(), Some(model));
            assert_eq!(worker.effort.as_deref(), Some(effort));
        }
        let saved = data
            .store()
            .load_project(Path::new("/planner-project"))
            .unwrap();
        for (id, model, effort) in [
            ("reply-1", Some("first-model"), Some("high")),
            ("reply-2", Some("second-model"), Some("low")),
            ("reply-3", Some("second-model"), Some("medium")),
            ("model-only", Some("older-model"), None),
            ("legacy-reply", None, None),
        ] {
            for messages in [&saved.mods[0].messages, &code_mod.messages] {
                let message = messages
                    .iter()
                    .find(|message| message.item_id.as_deref() == Some(id))
                    .unwrap();
                assert_eq!(message.model.as_deref(), model);
                assert_eq!(message.effort.as_deref(), effort);
            }
        }
    }
}
