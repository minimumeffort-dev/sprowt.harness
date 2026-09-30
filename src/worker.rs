use std::{io, path::Path};

use serde_json::Value;

use crate::{
    codex::{Action, Client, Event},
    store::{CodeMod, Message, Store, Submission, WorkerRecord},
};

#[derive(Clone, Copy, PartialEq)]
pub enum Status {
    Connecting,
    Ready,
    Starting,
    Running,
    Stopping,
    Failed,
}

pub struct Worker {
    pub id: i64,
    pub mod_id: i64,
    pub status: Status,
    pub error: Option<String>,
    pub enabled: bool,
    client: Client,
    turn: Option<String>,
    pending: Option<Submission>,
    recovery: Option<String>,
}

impl Worker {
    pub fn start(project: &Path, mod_id: i64, record: WorkerRecord) -> io::Result<Self> {
        Ok(Self {
            id: record.id,
            mod_id,
            status: Status::Connecting,
            error: None,
            enabled: true,
            client: Client::start(project, record.thread_id)?,
            turn: None,
            pending: None,
            recovery: record.pending,
        })
    }

    pub fn label(&self) -> String {
        let state = match self.status {
            Status::Connecting => "connecting",
            Status::Ready if self.enabled => "ready",
            Status::Ready => "paused",
            Status::Starting => "starting",
            Status::Running => "running",
            Status::Stopping => "stopping",
            Status::Failed => "stopped",
        };
        format!("◆ codex · {state} · read-only")
    }

    pub fn toggle(&mut self) {
        if self.enabled {
            self.enabled = false;
            if let Some(turn) = &self.turn {
                match self.client.send(Action::Stop { turn: turn.clone() }) {
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
    ) -> rusqlite::Result<()> {
        let events = self.client.poll().collect::<Vec<_>>();
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
        if let Some(input) = store.next_input(self.mod_id, steering)? {
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
            if let Err(error) = self.client.send(action) {
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
            Event::Ready(thread) => {
                store.save_thread(self.id, thread["id"].as_str().unwrap())?;
                if let Some(turns) = thread["turns"].as_array() {
                    for turn in turns {
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
                    }
                }
                if self.recovery.is_some() {
                    self.fail("Delivery could not be confirmed. The instruction is retained; automatic retry is paused.".into());
                } else {
                    self.status = Status::Ready;
                }
            }
            Event::Accepted { source, turn } => {
                if let Some(input) = self.pending.take() {
                    if input.source != source {
                        return Err(rusqlite::Error::InvalidQuery);
                    }
                    self.acknowledge(store, code_mod, &input)?;
                }
                self.turn = Some(turn.clone());
                self.status = Status::Running;
                if !self.enabled {
                    if let Err(error) = self.client.send(Action::Stop { turn }) {
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
                self.client.shutdown();
                self.fail(message);
            }
            Event::Notification(message) => {
                let params = &message["params"];
                match message["method"].as_str().unwrap_or("") {
                    "item/agentMessage/delta" => {
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

    fn item(&self, store: &Store, code_mod: &mut CodeMod, item: &Value) -> rusqlite::Result<()> {
        let Some(id) = item["clientId"].as_str().or_else(|| item["id"].as_str()) else {
            return Ok(());
        };
        let (role, body) = match item["type"].as_str() {
            Some("agentMessage") => ("codex", item["text"].as_str().unwrap_or("").to_owned()),
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

    fn fail(&mut self, message: String) {
        self.status = Status::Failed;
        self.enabled = false;
        self.error = Some(message);
    }
}

fn save_message(store: &Store, code_mod: &mut CodeMod, message: Message) -> rusqlite::Result<()> {
    store.save_message(code_mod.id, &message)?;
    if let Some(existing) = code_mod
        .messages
        .iter_mut()
        .find(|existing| existing.item_id == message.item_id)
    {
        *existing = message;
    } else {
        code_mod.messages.push(message);
    }
    Ok(())
}
