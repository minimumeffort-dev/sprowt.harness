use std::{io, path::Path};

use serde_json::Value;

use crate::{
    codex::{Action, Client, Event, Resume},
    execution::{Check, Report},
    plan::{Plan, Role},
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
    task_summary: String,
    writable: bool,
    verification: Vec<Check>,
    verify_before: Option<workspace::Snapshot>,
    preparing: Option<String>,
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
        let client = if role == Role::Executor {
            Some(Client::start(
                project,
                record.thread_id.clone().map(|id| Resume {
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
                None,
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
            task_summary: String::new(),
            writable: workspace.is_some(),
            verification: Vec::new(),
            verify_before: None,
            preparing: None,
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
        let state = match self.status {
            Status::Routing => "choosing model",
            Status::Complete if self.role == Role::Planner => "plan ready",
            Status::Complete => "changes ready",
            Status::Checking => "checking",
            Status::Connecting => self.preparing.as_deref().unwrap_or("connecting"),
            Status::Ready if self.enabled => "ready",
            Status::Ready => "paused",
            Status::Starting => "starting",
            Status::Running => self.preparing.as_deref().unwrap_or("running"),
            Status::Stopping => "stopping",
            Status::Failed => "stopped",
        };
        let glyph = activity.unwrap_or(if self.role == Role::Planner {
            "▤"
        } else {
            "◆"
        });
        format!(
            "{glyph} {} · {}{}{} · {state} · {}",
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
            if self.writable {
                "Linux VM"
            } else {
                "read-only"
            }
        )
    }

    pub fn toggle(&mut self) {
        if self.enabled {
            self.enabled = false;
            if self.status == Status::Checking
                || (self.writable && self.status == Status::Connecting)
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
                    store.execution_status(self.mod_id, "verifying")?;
                    code_mod.execution = store.execution(self.mod_id)?;
                    self.begin_checks(format!("final:{}", self.mod_id), checks);
                } else if !execution.tasks.iter().any(|run| {
                    run.status == "pending"
                        && code_mod
                            .planning
                            .as_ref()
                            .and_then(|p| p.plan.as_ref())
                            .is_some_and(|p| {
                                p.tasks
                                    .iter()
                                    .any(|t| t.id == run.task_id && t.worker == self.provider)
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
                    self.task_report = None;
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
                store.planning_model(self.mod_id, &selection)?;
                code_mod.planning = store.planning(self.mod_id)?;
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
                self.preparing = None;
                if let Some(input) = self.pending.take() {
                    if input.source != source {
                        return Err(rusqlite::Error::InvalidQuery);
                    }
                    self.acknowledge(store, code_mod, &input)?;
                }
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
                checks,
                before,
            } if self.task_source.as_deref() == Some(&source) => {
                self.verify_before = Some(before);
                let passed = self.checks_passed(code_mod, &checks);
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
                    code_mod.execution = store.execution(self.mod_id)?;
                    self.task_source = None;
                    self.enabled = false;
                    self.status = if passed {
                        Status::Complete
                    } else {
                        Status::Ready
                    };
                    if !passed && self.error.is_none() {
                        self.error = Some("Final checks failed or were stopped. See plan details; Ctrl+R reruns them.".into());
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
                let summary = format!(
                    "{} · {}\n{}",
                    if passed {
                        "✓ task complete"
                    } else {
                        "! check failed"
                    },
                    self.task_name(code_mod),
                    self.task_summary
                );
                save_message(
                    store,
                    code_mod,
                    Message {
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
                if !passed {
                    self.enabled = false;
                    self.error.get_or_insert_with(|| "Verification failed or was stopped. See plan details; Ctrl+R retries the task.".into());
                }
                code_mod.execution = store.execution(self.mod_id)?;
            }
            Event::Checked { .. } => {}
            Event::Notification(message) => {
                let params = &message["params"];
                match message["method"].as_str().unwrap_or("") {
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
        if self.role == Role::Executor
            && self.task_source.is_some()
            && item["type"] == "agentMessage"
            && item["phase"] != "commentary"
        {
            self.task_report = item["text"].as_str().map(str::to_owned);
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

    fn task_name(&self, code_mod: &CodeMod) -> String {
        code_mod
            .execution
            .as_ref()
            .and_then(|execution| {
                execution
                    .tasks
                    .iter()
                    .find(|run| Some(run.source.as_str()) == self.task_source.as_deref())
            })
            .and_then(|run| {
                code_mod
                    .planning
                    .as_ref()?
                    .plan
                    .as_ref()?
                    .tasks
                    .iter()
                    .find(|task| task.id == run.task_id)
            })
            .map_or_else(|| "task".into(), |task| task.title.clone())
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
        let task = code_mod
            .planning
            .as_ref()
            .unwrap()
            .plan
            .as_ref()
            .unwrap()
            .tasks
            .iter()
            .find(|task| task.id == run.task_id)
            .unwrap();
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
                        && ["sending", "running", "checking"].contains(&run.status.as_str())
                })
            })
            .cloned();
        let Some(run) = active else {
            return Ok(());
        };
        self.task_source = Some(run.source.clone());
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
        let plan = Plan::parse(&json!({"summary":"Two greetings","tasks":[
            {"id":"codex","title":"Codex greeting","outcome":"Create codex.sh printing exactly hello codex. Read your inbox and send a useful update to the muse task once done.","files":["codex.sh"],"depends_on":[],"coordination":[{"task":"muse","topic":"Greeting fixture verification"}],"worker":"codex","checks":["Script prints hello codex"]},
            {"id":"muse","title":"Muse greeting","outcome":"This is a cancellation fixture. Before writing source, if $HOME/cancel-once is absent, create that marker and run /bin/sleep 299. The harness will interrupt that command; on retry skip it. Then create muse.sh printing exactly hello muse. Use the native MCP inbox tools to read messages and send the codex task a useful update once done.","files":["muse.sh"],"depends_on":[],"coordination":[{"task":"codex","topic":"Greeting fixture verification"}],"worker":"muse","checks":["Script prints hello muse"]}
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
            let deadline = Instant::now() + Duration::from_secs(360);
            let mut previous = String::new();
            let mut overlap = false;
            let mut steered = false;
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
                    worker.poll(&mut store, &mut m, true, &project).unwrap();
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
                if steered
                    && !interrupted
                    && workers[1].pending.is_none()
                    && Instant::now() >= inspect_at
                {
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
                    store.retry_tasks(m.id).unwrap();
                    m.execution = store.execution(m.id).unwrap();
                    workers[1].toggle();
                    retried = true;
                    eprintln!("Native child exit verified; retrying the Muse task");
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
            let deadline = Instant::now() + Duration::from_secs(1800);
            let mut overlap = false;
            let mut previous = String::new();
            while Instant::now() < deadline && m.execution.as_ref().unwrap().status != "review" {
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
    fn final_checks_save_the_verified_snapshot_and_unconfirmed_delivery_stays_retryable() {
        let (_data, mut store, mut code_mod, mut worker) = executor();
        let source = worker.task_source.clone().unwrap();
        store
            .finish_task(code_mod.id, &source, "done", "Done", &[])
            .unwrap();
        code_mod.execution = store.execution(code_mod.id).unwrap();
        worker.task_source = Some(format!("final:{}", code_mod.id));
        let checks = vec![crate::execution::CheckResult {
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
    fn live_replies_save_model_and_effort_without_relabeling_history() {
        let (data, mut store, mut code_mod, mut worker) = planner();
        worker.role = Role::Executor;
        save_message(
            &store,
            &mut code_mod,
            Message {
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
