use std::{
    io,
    path::Path,
    sync::mpsc::{Receiver, TryRecvError},
    time::{Duration, Instant},
};

use serde_json::Value;

use crate::{
    codex::{Action, Client, Event},
    plan::{Plan, Role},
    router::Selection,
    store::{CodeMod, Message, Store, Submission, WorkerRecord},
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
}

pub struct Worker {
    pub id: i64,
    pub mod_id: i64,
    pub role: Role,
    pub selection: Option<Selection>,
    pub status: Status,
    pub error: Option<String>,
    pub enabled: bool,
    client: Option<Client>,
    routing: Option<(Receiver<Selection>, Instant)>,
    thread_id: Option<String>,
    final_plan: Option<String>,
    turn: Option<String>,
    pending: Option<Submission>,
    recovery: Option<String>,
}

impl Worker {
    pub fn start(
        project: &Path,
        code_mod: &CodeMod,
        record: WorkerRecord,
        role: Role,
        routing: Option<Receiver<Selection>>,
    ) -> io::Result<Self> {
        let client = if role == Role::Executor {
            Some(Client::start(
                project,
                record.thread_id.clone(),
                role,
                &code_mod.description,
                code_mod.planning.as_ref().and_then(|p| p.plan.as_ref()),
                None,
            )?)
        } else {
            None
        };
        Ok(Self {
            id: record.id,
            mod_id: code_mod.id,
            role,
            selection: None,
            status: if role == Role::Planner {
                Status::Routing
            } else {
                Status::Connecting
            },
            error: None,
            enabled: true,
            client,
            routing: routing.map(|receiver| (receiver, Instant::now())),
            thread_id: record.thread_id,
            final_plan: None,
            turn: None,
            pending: None,
            recovery: record.pending,
        })
    }

    pub fn label(&self) -> String {
        let state = match self.status {
            Status::Routing => "choosing model",
            Status::Complete => "plan ready",
            Status::Connecting => "connecting",
            Status::Ready if self.enabled => "ready",
            Status::Ready => "paused",
            Status::Starting => "starting",
            Status::Running => "running",
            Status::Stopping => "stopping",
            Status::Failed => "stopped",
        };
        let model = self
            .selection
            .as_ref()
            .map_or(String::new(), |s| format!(" · {} · {}", s.model, s.effort));
        format!("◆ codex {} · {state}{model} · read-only", self.role.name())
    }

    pub fn toggle(&mut self) {
        if self.enabled {
            self.enabled = false;
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
            let selection = self
                .routing
                .as_ref()
                .map(|(receiver, started)| match receiver.try_recv() {
                    Ok(selection) => Some(selection),
                    Err(TryRecvError::Empty) if started.elapsed() < Duration::from_secs(30) => None,
                    Err(_) => Some(Selection::fallback(
                        "Laya unavailable or timed out · Sol high fallback",
                    )),
                })
                .unwrap_or_else(|| {
                    Some(Selection::fallback(
                        "Laya not installed · Sol high fallback; run sprowt-harness setup",
                    ))
                });
            let Some(selection) = selection else {
                return Ok(());
            };
            store.planning_model(self.mod_id, &selection)?;
            code_mod.planning = store.planning(self.mod_id)?;
            self.selection = Some(selection.clone());
            self.routing = None;
            match Client::start(
                project,
                self.thread_id.clone(),
                self.role,
                &code_mod.description,
                None,
                Some(selection),
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
        if !dispatch || !self.enabled || self.pending.is_some() || self.recovery.is_some() {
            return Ok(());
        }
        let steering = self.status == Status::Running;
        if !steering && self.status != Status::Ready {
            return Ok(());
        }
        let input = if self.role == Role::Planner && !steering {
            store.planner_input(self.mod_id)?
        } else {
            store.next_input(self.mod_id, steering)?
        };
        if let Some(input) = input {
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
                }
            };
            self.pending = Some(input);
            if !steering {
                self.status = Status::Starting;
            }
            if let Err(error) = self.client.as_ref().unwrap().send(action) {
                self.fail(error.to_string());
            }
        }
        Ok(())
    }

    fn receive(
        &mut self,
        event: Event,
        store: &mut Store,
        code_mod: &mut CodeMod,
    ) -> rusqlite::Result<()> {
        match event {
            Event::Configured(selection) => {
                store.planning_model(self.mod_id, &selection)?;
                code_mod.planning = store.planning(self.mod_id)?;
                self.selection = Some(selection);
            }
            Event::Ready(thread) => {
                store.save_thread(self.id, thread["id"].as_str().unwrap())?;
                if let Some(turns) = thread["turns"].as_array() {
                    for turn in turns {
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
                                self.item(store, code_mod, item)?;
                            }
                        }
                        if current_plan && turn["status"] == "completed" {
                            self.complete_plan(store, code_mod)?;
                        }
                    }
                }
                if self.recovery.is_some() {
                    self.fail("Delivery could not be confirmed. The instruction is retained; automatic retry is paused.".into());
                } else if self.status != Status::Complete && self.status != Status::Failed {
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
                if let Some(input) = self.pending.take() {
                    if input.source != source {
                        return Err(rusqlite::Error::InvalidQuery);
                    }
                    self.acknowledge(store, code_mod, &input)?;
                }
                if self.role == Role::Planner && source.starts_with("00000003-") {
                    self.final_plan = None;
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
                // A rejected steer must leave its request available for the next active turn.
                self.enabled = false;
                self.error = Some(message);
                if self.turn.is_none() {
                    self.status = Status::Ready;
                }
            }
            Event::Failed(message) => {
                self.client.as_mut().unwrap().shutdown();
                self.fail(message);
                if self.role == Role::Planner {
                    store.planning_status(self.mod_id, "failed")?;
                    code_mod.planning = store.planning(self.mod_id)?;
                }
            }
            Event::Notification(message) => {
                let params = &message["params"];
                match message["method"].as_str().unwrap_or("") {
                    "item/agentMessage/delta" if self.role == Role::Executor => {
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
                                    role: "codex".into(),
                                    body: String::new(),
                                });
                            message.body.push_str(delta);
                            save_message(store, code_mod, message)?;
                        }
                    }
                    "item/completed" => self.item(store, code_mod, &params["item"])?,
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
        if input.source.starts_with("00000003-") {
            code_mod.planning = store.planning(self.mod_id)?;
            return Ok(());
        }
        code_mod
            .queue
            .retain(|message| crate::store::source_id(message.id, false) != input.source);
        if input.source.starts_with("00000002-") {
            code_mod.steering.drain(..input.texts.len());
        }
        save_message(
            store,
            code_mod,
            Message {
                item_id: Some(input.source.clone()),
                role: "user".into(),
                body: input.texts.join("\n\n"),
            },
        )
    }

    fn item(
        &mut self,
        store: &Store,
        code_mod: &mut CodeMod,
        item: &Value,
    ) -> rusqlite::Result<()> {
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
                role: role.into(),
                body,
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

fn remember_message(code_mod: &mut CodeMod, message: Message) {
    if let Some(existing) = code_mod
        .messages
        .iter_mut()
        .find(|existing| existing.item_id == message.item_id)
    {
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
        worker.receive(Event::Ready(json!({"id":"saved-thread", "turns":[{
            "id":"turn-1","status":"completed","items":[
                {"type":"userMessage","clientId":input.source,"content":[{"type":"text","text":"description"}]},
                {"id":"plan-item","type":"agentMessage","phase":"final_answer","text":final_plan().to_string()}
            ]}]})),&mut store,&mut code_mod).unwrap();
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
}
