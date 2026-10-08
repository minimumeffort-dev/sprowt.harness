use std::io;

use rusqlite::{OptionalExtension, Result, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::store::{Message, Store, Submission};

pub const SEND: &str = "send_worker_message";
pub const READ: &str = "read_worker_messages";
pub const ACK: &str = "ack_worker_messages";
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS worker_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    mod_id INTEGER NOT NULL REFERENCES code_mods(id) ON DELETE CASCADE,
    sender_worker INTEGER, sender_task INTEGER, recipient_task INTEGER,
    from_task TEXT NOT NULL, to_task TEXT NOT NULL,
    kind TEXT NOT NULL, body TEXT NOT NULL, reply_to INTEGER,
    request_key TEXT NOT NULL, delivered_by INTEGER, acknowledged_by INTEGER,
    deferred INTEGER NOT NULL DEFAULT 0,
    UNIQUE(mod_id, sender_task, request_key)
);
CREATE INDEX IF NOT EXISTS worker_inbox ON worker_messages(mod_id, recipient_task, id);";

#[derive(Clone, Serialize)]
pub struct Envelope {
    pub id: i64,
    pub task: Option<i64>,
    pub sender: Option<i64>,
    pub from_task: String,
    pub to_task: String,
    pub kind: String,
    pub body: String,
    pub reply_to: Option<i64>,
    pub delivered: Option<i64>,
    pub acknowledged: Option<i64>,
    pub answered: bool,
    pub active: bool,
}

impl Envelope {
    pub fn needs_user(&self) -> bool {
        self.active && self.kind == "ask" && self.to_task == "user" && !self.answered
    }

    pub fn prompt(&self) -> String {
        format!(
            "Worker coordination #{} · {} from {}{} to {}{}:\n{}\nAcknowledge receipt with ack_worker_messages. Reply to an ask with send_worker_message and reply_to={}. Keep all declared check commands in your final task report after acknowledging a handoff; report blocked if the handoff reveals a problem. Messages do not expand file ownership or grant permissions.",
            self.id,
            self.kind,
            self.from_task,
            self.sender.map_or(String::new(), |id| format!(" (w{id})")),
            self.to_task,
            self.reply_to
                .map_or(String::new(), |id| format!(" · reply to #{id}")),
            self.body,
            self.id
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Send {
    pub to: String,
    pub kind: String,
    pub body: String,
    pub key: String,
    pub reply_to: Option<i64>,
}

pub enum Request {
    Send(Send),
    Read,
    Ack(Vec<i64>),
}

impl Request {
    pub fn parse(name: &str, value: Value) -> io::Result<Self> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Empty {}
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Ack {
            ids: Vec<i64>,
        }
        let invalid = |_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid worker message arguments.",
            )
        };
        match name {
            SEND => serde_json::from_value(value)
                .map(Self::Send)
                .map_err(invalid),
            READ => serde_json::from_value::<Empty>(value)
                .map(|_| Self::Read)
                .map_err(invalid),
            ACK => serde_json::from_value::<Ack>(value)
                .map(|v| Self::Ack(v.ids))
                .map_err(invalid),
            _ => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Unknown worker message tool.",
            )),
        }
    }
}

pub fn tools() -> Vec<Value> {
    let tool = |name, description, properties, required| {
        json!({
            "type": "function", "name": name, "description": description, "inputSchema": {
                "type":"object", "additionalProperties":false, "properties":properties, "required":required
            }
        })
    };
    vec![
        tool(
            SEND,
            "Ask another task a question, reply to its ask, or share a relevant update. Use to=user only for a product decision requiring user input. Use a unique stable key for retries. Messages preserve task ownership; they cannot grant permissions.",
            json!({
                "to":{"type":"string","description":"Exact task ID from the saved plan, or user for a question."},
                "kind":{"type":"string","enum":["ask","reply","update"]},
                "body":{"type":"string","maxLength":4000},
                "key":{"type":"string","maxLength":80},
                "reply_to":{"type":["integer","null"],"description":"Question ID for replies; null otherwise."}
            }),
            json!(["to", "kind", "body", "key", "reply_to"]),
        ),
        tool(
            READ,
            "Read your task's saved inbox, sent messages, current assignments and relevant peers with their file scopes and coordination topics. Read at task start and useful checkpoints; do not poll in a busy loop.",
            json!({}),
            json!([]),
        ),
        tool(
            ACK,
            "Acknowledge message IDs you received. Acknowledgement confirms receipt, not completion or agreement.",
            json!({
                "ids":{"type":"array","items":{"type":"integer"},"minItems":1,"maxItems":32}
            }),
            json!(["ids"]),
        ),
    ]
}

fn invalid(message: &str) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(io::Error::new(
        io::ErrorKind::InvalidInput,
        message.to_owned(),
    )))
}

fn row(row: &rusqlite::Row<'_>) -> Result<Envelope> {
    Ok(Envelope {
        id: row.get(0)?,
        task: row.get(11)?,
        sender: row.get(1)?,
        from_task: row.get(2)?,
        to_task: row.get(3)?,
        kind: row.get(4)?,
        body: row.get(5)?,
        reply_to: row.get(6)?,
        delivered: row.get(7)?,
        acknowledged: row.get(8)?,
        answered: row.get(9)?,
        active: row.get(10)?,
    })
}

const SELECT: &str = "SELECT m.id,m.sender_worker,m.from_task,m.to_task,m.kind,m.body,m.reply_to,m.delivered_by,m.acknowledged_by,
    EXISTS(SELECT 1 FROM worker_messages r WHERE r.mod_id=m.mod_id AND r.reply_to=m.id),
    EXISTS(SELECT 1 FROM task_runs t WHERE t.mod_id=m.mod_id AND t.id IN (m.sender_task,m.recipient_task)),m.sender_task
    FROM worker_messages m";

impl Store {
    fn actor(&self, mod_id: i64, worker: i64) -> Result<(i64, String)> {
        self.0.query_row(
            "SELECT t.id,t.task_id FROM task_runs t
            JOIN workers w ON w.id=t.worker_id AND w.mod_id=t.mod_id
            JOIN code_mods c ON c.id=t.mod_id
            WHERE t.mod_id=?1 AND w.id=?2 AND w.role='executor' AND c.closed=0
            AND t.status IN ('sending','running','checking')",
            params![mod_id, worker],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    pub fn mailbox(&self, mod_id: i64) -> Result<Vec<Envelope>> {
        let mut messages = self
            .0
            .prepare(&format!(
                "{SELECT} WHERE m.mod_id=?1
            ORDER BY (m.kind='ask' AND m.to_task='user' AND EXISTS(SELECT 1 FROM task_runs t WHERE t.id=m.sender_task) AND NOT EXISTS(
                SELECT 1 FROM worker_messages r WHERE r.reply_to=m.id)) DESC,m.id DESC LIMIT 64"
            ))?
            .query_map([mod_id], row)?
            .collect::<Result<Vec<_>>>()?;
        messages.sort_by_key(|m| m.id);
        Ok(messages)
    }

    pub fn mailbox_call(&mut self, mod_id: i64, worker: i64, request: Request) -> Result<Value> {
        let (task, task_id) = self
            .actor(mod_id, worker)
            .map_err(|_| invalid("Messaging needs your assigned task in this open codemod."))?;
        match request {
            Request::Send(message) => self.send_mail(mod_id, worker, message),
            Request::Read => {
                let messages = self
                    .0
                    .prepare(&format!(
                        "{SELECT} WHERE m.mod_id=?1
                    AND (m.recipient_task=?2 OR m.sender_task=?2)
                    ORDER BY (m.recipient_task=?2 AND m.acknowledged_by IS NULL) DESC,m.id DESC LIMIT 32"
                    ))?
                    .query_map(params![mod_id, task], row)?
                    .collect::<Result<Vec<_>>>()?;
                let peers = self.0.prepare("SELECT task_id,worker_id,status FROM task_runs WHERE mod_id=?1 ORDER BY id")?
                    .query_map([mod_id], |r| Ok(json!({"task":r.get::<_,String>(0)?,
                        "worker":r.get::<_,Option<i64>>(1)?,"status":r.get::<_,String>(2)?})))?
                    .collect::<Result<Vec<_>>>()?;
                let plan = self.planning(mod_id)?.and_then(|planning| planning.plan);
                let mut relevant = plan
                    .as_ref()
                    .and_then(|plan| {
                        plan.tasks
                            .iter()
                            .find(|task| task.id == task_id)
                            .map(|task| serde_json::to_value(plan.peers(task)).unwrap())
                    })
                    .unwrap_or_else(|| json!([]));
                for peer in relevant.as_array_mut().unwrap() {
                    if let Some(assignment) = peers.iter().find(|p| p["task"] == peer["task"]) {
                        peer["worker"] = assignment["worker"].clone();
                        peer["status"] = assignment["status"].clone();
                    }
                }
                Ok(json!({"messages":messages,"peers":peers,"relevant_peers":relevant}))
            }
            Request::Ack(ids) => {
                if ids.is_empty() || ids.len() > 32 {
                    return Err(invalid("Acknowledge 1–32 received messages."));
                }
                let tx = self.0.transaction()?;
                for id in ids {
                    if tx.execute("UPDATE worker_messages SET delivered_by=COALESCE(delivered_by,?3),acknowledged_by=?3
                        WHERE mod_id=?1 AND id=?2 AND recipient_task=?4",
                        params![mod_id,id,worker,task])? != 1 {
                        return Err(invalid("Message is not in your task's inbox."));
                    }
                }
                tx.commit()?;
                Ok(json!({"acknowledged":true}))
            }
        }
    }

    fn send_mail(&mut self, mod_id: i64, worker: i64, message: Send) -> Result<Value> {
        if !["ask", "reply", "update"].contains(&message.kind.as_str())
            || message.body.trim().is_empty()
            || message.body.len() > 4000
            || message
                .body
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
            || message.key.trim().is_empty()
            || message.key.len() > 80
            || (message.kind == "reply") != message.reply_to.is_some()
            || (message.to == "user" && message.kind != "ask")
        {
            return Err(invalid(
                "Use a brief ask, reply or update with a stable key; only asks can target user.",
            ));
        }
        let tx = self.0.transaction()?;
        let (task, from): (i64, String) = tx.query_row(
            "SELECT id,task_id FROM task_runs WHERE mod_id=?1 AND worker_id=?2
            AND status IN ('sending','running','checking')",
            params![mod_id, worker],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let recipient = if message.to == "user" {
            None
        } else {
            Some(
                tx.query_row(
                    "SELECT id FROM task_runs WHERE mod_id=?1 AND task_id=?2",
                    params![mod_id, message.to],
                    |r| r.get::<_, i64>(0),
                )
                .map_err(|_| invalid("Recipient must be a task in this codemod's current plan."))?,
            )
        };
        if recipient == Some(task) {
            return Err(invalid("Send coordination to another task."));
        }
        if let Some((id,kind,body,reply,to)) = tx.query_row(
            "SELECT id,kind,body,reply_to,recipient_task FROM worker_messages WHERE mod_id=?1 AND sender_task=?2 AND request_key=?3",
            params![mod_id,task,message.key], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<i64>>(3)?,r.get::<_,Option<i64>>(4)?))).optional()? {
            if (kind,body,reply,to)!=(message.kind,message.body,message.reply_to,recipient) {
                return Err(invalid("That message key already belongs to a different message."));
            }
            return Ok(json!({"id":id,"saved":true}));
        }
        if let Some(id) = message.reply_to {
            let valid: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM worker_messages q WHERE q.mod_id=?1 AND q.id=?2
                AND q.kind='ask' AND q.recipient_task=?3 AND q.sender_task=?4
                AND NOT EXISTS(SELECT 1 FROM worker_messages r WHERE r.reply_to=q.id))",
                params![mod_id, id, task, recipient],
                |r| r.get(0),
            )?;
            if !valid {
                return Err(invalid(
                    "Reply only to an unanswered ask addressed to your task.",
                ));
            }
        }
        if message.kind == "ask" {
            if let Some(recipient) = recipient {
                let available: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM task_runs WHERE id=?1 AND status IN ('pending','sending','running'))",
                    [recipient], |r| r.get(0))?;
                if !available {
                    return Err(invalid(
                        "That task cannot answer now. Ask the user if a decision is needed.",
                    ));
                }
                let cycle: bool = tx.query_row("WITH RECURSIVE edges(sender,recipient) AS (
                    SELECT q.sender_task,q.recipient_task FROM worker_messages q WHERE q.mod_id=?1 AND q.kind='ask'
                    AND NOT EXISTS(SELECT 1 FROM worker_messages r WHERE r.reply_to=q.id)
                    UNION SELECT t.id,d.id FROM plans p,json_each(p.body,'$.tasks') task,json_each(task.value,'$.depends_on') dep
                    JOIN task_runs t ON t.mod_id=p.mod_id AND t.task_id=json_extract(task.value,'$.id')
                    JOIN task_runs d ON d.mod_id=p.mod_id AND d.task_id=dep.value WHERE p.mod_id=?1
                    ), waits(id) AS (VALUES (?2) UNION SELECT e.recipient FROM edges e JOIN waits w ON e.sender=w.id)
                    SELECT EXISTS(SELECT 1 FROM waits WHERE id=?3)", params![mod_id,recipient,task], |r| r.get(0))?;
                if cycle {
                    return Err(invalid(
                        "That question would make tasks wait on each other. Share an update or ask the user.",
                    ));
                }
            }
            if recipient.is_none()
                && tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM worker_messages q WHERE q.mod_id=?1
                AND q.sender_task=?2 AND q.to_task='user' AND q.kind='ask'
                AND NOT EXISTS(SELECT 1 FROM worker_messages r WHERE r.reply_to=q.id))",
                    params![mod_id, task],
                    |r| r.get::<_, bool>(0),
                )?
            {
                return Err(invalid(
                    "Your task already has a question waiting for the user.",
                ));
            }
        }
        let pending: i64=tx.query_row("SELECT COUNT(*) FROM worker_messages WHERE mod_id=?1 AND recipient_task=?2 AND acknowledged_by IS NULL",
            params![mod_id,recipient], |r| r.get(0))?;
        if pending >= 32 {
            return Err(invalid(
                "Recipient inbox is full; wait for its acknowledgements.",
            ));
        }
        tx.execute("INSERT INTO worker_messages(mod_id,sender_worker,sender_task,recipient_task,from_task,to_task,kind,body,reply_to,request_key)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![mod_id,worker,task,recipient,from,message.to,message.kind,message.body,message.reply_to,message.key])?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(json!({"id":id,"saved":true,"delivery":"pending"}))
    }

    pub fn next_mail(&self, mod_id: i64, worker: i64) -> Result<Option<Submission>> {
        let id: Option<i64> = self
            .0
            .query_row(
                "SELECT m.id FROM worker_messages m JOIN task_runs t ON t.id=m.recipient_task
            WHERE m.mod_id=?1 AND t.worker_id=?2 AND t.status='running' AND m.delivered_by IS NULL
            AND m.acknowledged_by IS NULL AND m.deferred=0 ORDER BY m.id LIMIT 1",
                params![mod_id, worker],
                |r| r.get(0),
            )
            .optional()?;
        id.map(|id| self.submission(mod_id, &source(id)))
            .transpose()
    }

    pub fn mail_prompt(&self, mod_id: i64, id: i64) -> Result<String> {
        self.0
            .query_row(
                &format!("{SELECT} WHERE m.mod_id=?1 AND m.id=?2"),
                params![mod_id, id],
                row,
            )
            .map(|m| m.prompt())
    }

    pub fn task_mail(&self, mod_id: i64, task: i64) -> Result<String> {
        let messages = self
            .0
            .prepare(&format!(
                "{SELECT} WHERE m.mod_id=?1 AND m.recipient_task=?2
            AND m.acknowledged_by IS NULL ORDER BY m.id LIMIT 32"
            ))?
            .query_map(params![mod_id, task], row)?
            .collect::<Result<Vec<_>>>()?;
        Ok(messages
            .iter()
            .map(Envelope::prompt)
            .collect::<Vec<_>>()
            .join("\n\n"))
    }

    pub fn defer_mail(&self, mod_id: i64, id: i64) -> Result<()> {
        self.0.execute(
            "UPDATE worker_messages SET deferred=1 WHERE mod_id=?1 AND id=?2",
            params![mod_id, id],
        )?;
        Ok(())
    }

    pub fn user_answer(&mut self, mod_id: i64, id: i64, body: &str) -> Result<Message> {
        let tx = self.0.transaction()?;
        let (task, to): (i64, String) = tx.query_row(
            "SELECT q.sender_task,q.from_task FROM worker_messages q
            JOIN task_runs t ON t.id=q.sender_task JOIN code_mods c ON c.id=q.mod_id
            WHERE q.mod_id=?1 AND q.id=?2 AND q.kind='ask' AND q.to_task='user' AND c.closed=0
            AND NOT EXISTS(SELECT 1 FROM worker_messages r WHERE r.reply_to=q.id)",
            params![mod_id, id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if body.trim().is_empty() || body.len() > 4000 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute("INSERT INTO worker_messages(mod_id,recipient_task,from_task,to_task,kind,body,reply_to,request_key)
            VALUES (?1,?2,'user',?3,'reply',?4,?5,?6)", params![mod_id,task,to,body,id,format!("user:{id}")])?;
        let reply = Message {
            item_id: Some(format!("answer:{id}")),
            role: "user".into(),
            body: format!("Reply to #{id} · {body}"),
            model: None,
            effort: None,
        };
        tx.execute(
            "INSERT INTO messages(mod_id,item_id,role,body) VALUES (?1,?2,?3,?4)",
            params![mod_id, reply.item_id, reply.role, reply.body],
        )?;
        tx.execute("UPDATE code_mods SET draft='' WHERE id=?1", [mod_id])?;
        tx.commit()?;
        Ok(reply)
    }

    pub fn waiting_mail(&self, mod_id: i64, task: i64) -> Result<Option<String>> {
        self.0
            .query_row(
                "SELECT q.to_task FROM worker_messages q WHERE q.mod_id=?1 AND q.sender_task=?2
            AND q.kind='ask' AND NOT EXISTS(SELECT 1 FROM worker_messages r WHERE r.reply_to=q.id)
            ORDER BY q.id LIMIT 1",
                params![mod_id, task],
                |r| r.get(0),
            )
            .optional()
    }

    pub fn resume_mail(&mut self, mod_id: i64, worker: i64) -> Result<bool> {
        let tx = self.0.transaction()?;
        let waiting: Option<(i64,i64)>=tx.query_row("SELECT t.id,t.attempt FROM task_runs t
            JOIN code_mods c ON c.id=t.mod_id WHERE t.mod_id=?1 AND t.worker_id=?2 AND t.status='waiting'
            AND c.closed=0 AND NOT EXISTS(SELECT 1 FROM worker_messages q WHERE q.mod_id=t.mod_id AND q.sender_task=t.id
                AND q.kind='ask' AND NOT EXISTS(SELECT 1 FROM worker_messages r WHERE r.reply_to=q.id))",
            params![mod_id,worker], |r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((task, attempt)) = waiting else {
            return Ok(false);
        };
        tx.execute("UPDATE task_runs SET status='pending',attempt=?2,source=?3,turn_id=NULL,summary='',checks='[]' WHERE id=?1",
            params![task,attempt+1,crate::store::task_source(task,attempt+1)])?;
        tx.execute("UPDATE executions SET status=CASE WHEN EXISTS(SELECT 1 FROM task_runs WHERE mod_id=?1 AND status IN ('blocked','paused','waiting'))
            THEN 'blocked' ELSE 'running' END WHERE mod_id=?1", [mod_id])?;
        tx.commit()?;
        Ok(true)
    }
}

pub fn source(id: i64) -> String {
    format!("00000005-0000-0000-0000-{id:012x}")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        plan::{Plan, Role},
        store::{CodeMod, test_support::TestData},
        tools::{Context, Dispatcher},
    };
    use std::sync::atomic::AtomicBool;

    pub(crate) fn fixture() -> (TestData, Store, CodeMod, [i64; 2], Plan) {
        let data = TestData::new();
        let project = data.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let root = data.0.join("workspace");
        crate::workspace::create(&project, &root).unwrap();
        let mut store = data.store();
        let project_id = store.load_project(&project).unwrap().id;
        let mut m = store
            .create_mod(project_id, "Two cooperating tasks")
            .unwrap();
        let plan=Plan::parse(&json!({"summary":"Two tasks","tasks":[
            {"id":"a","title":"Task A","outcome":"Create a.txt using the agreed label","files":["a.txt"],"depends_on":[],"worker":"codex","checks":["A contains the agreed label"]},
            {"id":"b","title":"Task B","outcome":"Answer A and create b.txt","files":["b.txt"],"depends_on":[],"worker":"codex","checks":["B answered A"]}
        ]}).to_string()).unwrap();
        store
            .save_plan(m.id, &m.planning.as_ref().unwrap().source, &plan)
            .unwrap();
        store.create_execution(m.id, &root, &plan).unwrap();
        let mut workers = [0; 2];
        for (slot, worker) in workers.iter_mut().enumerate() {
            *worker = store.worker_at(m.id, Role::Executor, slot).unwrap().id;
            let input = store.task_input(m.id, *worker, &plan).unwrap().unwrap();
            store.acknowledge(*worker, &input).unwrap();
        }
        m.planning = store.planning(m.id).unwrap();
        m.execution = store.execution(m.id).unwrap();
        (data, store, m, workers, plan)
    }

    pub(crate) fn send(
        store: &mut Store,
        m: i64,
        worker: i64,
        to: &str,
        kind: &str,
        reply_to: Option<i64>,
        key: &str,
    ) -> i64 {
        store
            .mailbox_call(
                m,
                worker,
                Request::Send(Send {
                    to: to.into(),
                    kind: kind.into(),
                    body: format!("Message {key}"),
                    key: key.into(),
                    reply_to,
                }),
            )
            .unwrap()["id"]
            .as_i64()
            .unwrap()
    }

    #[test]
    fn workers_receive_saved_peer_scope_topics_and_live_assignments() {
        let (data, mut store, m, workers, mut plan) = fixture();
        plan.tasks[0].coordination.push(crate::plan::Coordination {
            task: "b".into(),
            topic: "Label format and readiness".into(),
        });
        store
            .save_plan(m.id, &m.planning.as_ref().unwrap().source, &plan)
            .unwrap();
        drop(store);
        let mut store = data.store();
        let read = store.mailbox_call(m.id, workers[1], Request::Read).unwrap();
        assert_eq!(read["relevant_peers"][0]["task"], "a");
        assert_eq!(read["relevant_peers"][0]["worker"], workers[0]);
        assert_eq!(read["relevant_peers"][0]["status"], "running");
        assert_eq!(read["relevant_peers"][0]["files"], json!(["a.txt"]));
        assert_eq!(
            read["relevant_peers"][0]["topics"],
            json!(["Label format and readiness"])
        );
        let prompt = crate::execution::task_prompt(&plan, &plan.tasks[1], 42);
        assert!(prompt.contains("Relevant peers"));
        assert!(prompt.contains("Label format and readiness"));
    }

    #[test]
    fn mailbox_persists_delivery_acknowledgement_and_idempotent_replies() {
        let (data, mut store, m, workers, _) = fixture();
        let id = send(&mut store, m.id, workers[0], "b", "ask", None, "contract");
        assert_eq!(
            send(&mut store, m.id, workers[0], "b", "ask", None, "contract"),
            id
        );
        assert!(store.mailbox(m.id).unwrap()[0].delivered.is_none());
        let delivery = store.next_mail(m.id, workers[1]).unwrap().unwrap();
        assert_eq!(delivery.source, source(id));
        store.pending(workers[1], Some(&delivery.source)).unwrap();
        drop(store);
        let mut store = data.store();
        assert_eq!(
            store.worker_at(m.id, Role::Executor, 1).unwrap().pending,
            Some(source(id))
        );
        store.acknowledge(workers[1], &delivery).unwrap();
        assert!(store.next_mail(m.id, workers[1]).unwrap().is_none());
        assert!(store.mailbox(m.id).unwrap()[0].acknowledged.is_none());
        let replacement = store.worker_at(m.id, Role::Executor, 2).unwrap().id;
        store
            .0
            .execute(
                "UPDATE task_runs SET worker_id=?2 WHERE mod_id=?1 AND task_id='b'",
                params![m.id, replacement],
            )
            .unwrap();
        assert!(store.mailbox_call(m.id, workers[1], Request::Read).is_err());
        let inbox = store
            .mailbox_call(m.id, replacement, Request::Read)
            .unwrap();
        assert_eq!(inbox["messages"][0]["id"], id);
        store
            .0
            .execute(
                "UPDATE task_runs SET worker_id=?2 WHERE mod_id=?1 AND task_id='b'",
                params![m.id, workers[1]],
            )
            .unwrap();
        assert!(
            store
                .mailbox_call(m.id, workers[0], Request::Ack(vec![id]))
                .is_err()
        );
        store
            .mailbox_call(m.id, workers[1], Request::Ack(vec![id]))
            .unwrap();
        let reply = send(
            &mut store,
            m.id,
            workers[1],
            "a",
            "reply",
            Some(id),
            "answer",
        );
        assert_eq!(
            send(
                &mut store,
                m.id,
                workers[1],
                "a",
                "reply",
                Some(id),
                "answer"
            ),
            reply
        );
        let saved = store.mailbox(m.id).unwrap();
        assert_eq!(saved.len(), 2);
        assert!(saved[0].answered);
        assert_eq!(saved[0].acknowledged, Some(workers[1]));
        assert!(
            store
                .waiting_mail(m.id, m.execution.as_ref().unwrap().tasks[0].id)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store.next_mail(m.id, workers[0]).unwrap().unwrap().source,
            source(reply)
        );
        assert_eq!(
            store.load_project(&data.0.join("project")).unwrap().mods[0]
                .messages
                .len(),
            2
        );
    }

    #[test]
    fn tools_enforce_codemod_task_identity_and_atomic_acknowledgements() {
        let (data, mut store, m, workers, _) = fixture();
        let flag = AtomicBool::new(false);
        let database = data
            .store()
            .worker_at(m.id, Role::Executor, 0)
            .unwrap()
            .database;
        let context = Context::worker(&m, workers[0], Role::Executor).with_mailbox(&database);
        assert_eq!(crate::tools::advertised(&context).len(), 5);
        assert!(
            crate::tools::advertised(&context)
                .iter()
                .all(|tool| tool["type"] == "function")
        );
        let planner = Context::worker(&m, workers[0], Role::Planner).with_mailbox(&database);
        assert!(crate::tools::advertised(&planner).is_empty());
        assert!(
            Dispatcher::new(&planner, None, &flag)
                .worker_call(READ, json!({}), |_| {})
                .is_err()
        );
        assert!(Dispatcher::new(&context,None,&flag).worker_call(SEND,json!({
            "to":"b","kind":"ask","body":"Hi","key":"bad","reply_to":null,"worker_id":workers[1]
        }), |_|{}).is_err());
        let id = send(&mut store, m.id, workers[0], "b", "ask", None, "question");
        assert!(
            store
                .mailbox_call(m.id, workers[1], Request::Ack(vec![id, 999999]))
                .is_err()
        );
        assert!(store.mailbox(m.id).unwrap()[0].acknowledged.is_none());
        let other = store
            .create_mod(
                store.load_project(&data.0.join("project")).unwrap().id,
                "Other mod",
            )
            .unwrap();
        assert!(
            store
                .mailbox_call(other.id, workers[0], Request::Read)
                .is_err()
        );
        assert!(
            store
                .mailbox_call(
                    m.id,
                    workers[0],
                    Request::Send(Send {
                        to: "../other".into(),
                        kind: "ask".into(),
                        body: "Question".into(),
                        key: "bad-recipient".into(),
                        reply_to: None
                    })
                )
                .is_err()
        );
        assert!(
            store
                .mailbox_call(
                    m.id,
                    workers[0],
                    Request::Send(Send {
                        to: "b".into(),
                        kind: "reply".into(),
                        body: "Forged reply".into(),
                        key: "bad-reply".into(),
                        reply_to: Some(id)
                    })
                )
                .is_err()
        );
        assert!(
            store
                .mailbox_call(
                    m.id,
                    workers[0],
                    Request::Send(Send {
                        to: "b".into(),
                        kind: "ask".into(),
                        body: "Changed body".into(),
                        key: "question".into(),
                        reply_to: None
                    })
                )
                .is_err()
        );
        let output = Dispatcher::new(&context, None, &flag)
            .worker_call(READ, json!({}), |_| {})
            .unwrap()
            .message();
        assert!(!output.contains("request_key"));
        assert!(output.contains("Message question"));
    }

    #[test]
    fn questions_cannot_create_wait_cycles_or_wait_on_dependent_tasks() {
        let (_data, mut store, m, workers, mut plan) = fixture();
        send(&mut store, m.id, workers[0], "b", "ask", None, "a-asks");
        let ask = || {
            Request::Send(Send {
                to: "a".into(),
                kind: "ask".into(),
                body: "B asks A".into(),
                key: "b-asks".into(),
                reply_to: None,
            })
        };
        assert!(store.mailbox_call(m.id, workers[1], ask()).is_err());
        store
            .0
            .execute("DELETE FROM worker_messages WHERE mod_id=?1", [m.id])
            .unwrap();
        plan.tasks[0].depends_on.push("b".into());
        store
            .save_plan(m.id, &m.planning.as_ref().unwrap().source, &plan)
            .unwrap();
        assert!(store.mailbox_call(m.id, workers[1], ask()).is_err());
    }

    #[test]
    fn answers_resume_waiting_tasks_without_reusing_messages_in_new_edit_rounds() {
        let (data, mut store, mut m, workers, _) = fixture();
        let ask = send(
            &mut store, m.id, workers[0], "user", "ask", None, "decision",
        );
        let task = &m.execution.as_ref().unwrap().tasks[0];
        store
            .finish_task(m.id, &task.source, "waiting", "Waiting for user", &[])
            .unwrap();
        m.coordination = store.mailbox(m.id).unwrap();
        assert!(m.question().is_some());
        assert!(!store.resume_mail(m.id, workers[0]).unwrap());
        let old_source = task.source.clone();
        store.save_draft(m.id, "draft").unwrap();
        let reply = store
            .user_answer(m.id, ask, "Use the simple option")
            .unwrap();
        assert_eq!(reply.role, "user");
        assert!(store.user_answer(m.id, ask, "Duplicate").is_err());
        assert!(store.resume_mail(m.id, workers[0]).unwrap());
        assert!(!store.resume_mail(m.id, workers[0]).unwrap());
        let input = store
            .task_input(
                m.id,
                workers[0],
                &m.planning.as_ref().unwrap().plan.clone().unwrap(),
            )
            .unwrap()
            .unwrap();
        assert_ne!(input.source, old_source);
        assert!(input.texts.join("\n").contains("Use the simple option"));
        let reopened = store.load_project(&data.0.join("project")).unwrap();
        assert!(reopened.mods[0].draft.is_empty());
        store.follow_up(m.id, "Different edit", None).unwrap();
        m.execution = store.execution(m.id).unwrap();
        assert!(m.question().is_none());
        assert!(store.mailbox(m.id).unwrap().iter().all(|m| !m.active));
        assert!(store.next_mail(m.id, workers[0]).unwrap().is_none());
        store.delete_mod(reopened.id, m.id).unwrap();
        assert!(store.mailbox(m.id).unwrap().is_empty());
    }
}
