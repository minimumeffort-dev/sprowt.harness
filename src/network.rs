use std::{collections::BTreeMap, io, path::Path};

use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::store::{Store, WorkerRecord};

pub const TOOL: &str = "request_network_access";
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS network_requests (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    mod_id INTEGER NOT NULL REFERENCES code_mods(id) ON DELETE CASCADE,
    worker_id INTEGER NOT NULL, task_id INTEGER NOT NULL,
    domains TEXT NOT NULL, reason TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending', resumed INTEGER NOT NULL DEFAULT 0,
    UNIQUE(mod_id, task_id, domains)
);
CREATE TABLE IF NOT EXISTS network_grants (
    mod_id INTEGER NOT NULL REFERENCES code_mods(id) ON DELETE CASCADE,
    domain TEXT NOT NULL, PRIMARY KEY(mod_id, domain)
);";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub domains: Vec<String>,
    pub reason: String,
}

impl Request {
    pub fn parse(value: Value) -> io::Result<Self> {
        serde_json::from_value::<Self>(value)?.validate()
    }

    pub fn validate(mut self) -> io::Result<Self> {
        let request = &mut self;
        if request.domains.is_empty()
            || request.domains.len() > 8
            || request.reason.trim().is_empty()
            || request.reason.len() > 300
            || request.reason.chars().any(char::is_control)
        {
            return Err(io::Error::other(
                "Request 1–8 exact hostnames and a short reason.",
            ));
        }
        for domain in &mut request.domains {
            *domain = domain.to_ascii_lowercase();
            if !hostname(domain) {
                return Err(io::Error::other(
                    "Use exact public hostnames, without URLs, ports, IPs or wildcards.",
                ));
            }
        }
        request.domains.sort();
        request.domains.dedup();
        request.reason = request.reason.trim().to_owned();
        Ok(self)
    }
}

fn hostname(domain: &str) -> bool {
    let labels: Vec<_> = domain.split('.').collect();
    domain.len() <= 253
        && labels.len() >= 2
        && labels
            .last()
            .is_some_and(|label| label.len() >= 2 && label.bytes().all(|c| c.is_ascii_lowercase()))
        && !domain.ends_with(".localhost")
        && !domain.ends_with(".local")
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        })
}

pub fn tool() -> Value {
    json!({"type":"function","name":TOOL,
        "description":"Request blocked download or documentation domains from the user. Use exact hostnames and a short reason, including any blocked redirect host. The harness shows an allow/deny dialog; only the user can grant access, scoped to this codemod. Pending or denied access means return completed:false with a short network blocker. Do not poll or retry a blocked download; after approval the harness reconnects and retries the task with its saved files.",
        "inputSchema":{"type":"object","additionalProperties":false,
        "properties":{"domains":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":8},"reason":{"type":"string","maxLength":300}},"required":["domains","reason"]}})
}

#[derive(Clone)]
pub struct Access {
    pub id: i64,
    pub mod_id: i64,
    pub worker: i64,
    pub task: i64,
    pub domains: Vec<String>,
    pub reason: String,
    pub status: String,
}

impl Store {
    pub fn request_network(
        &self,
        mod_id: i64,
        worker: i64,
        request: Request,
        already_allowed: bool,
    ) -> rusqlite::Result<String> {
        let task: i64 = self.0.query_row("SELECT t.id FROM task_runs t JOIN workers w ON w.id=t.worker_id JOIN code_mods m ON m.id=t.mod_id WHERE t.mod_id=?1 AND w.id=?2 AND w.mod_id=?1 AND w.role='executor' AND m.closed=0 AND t.status IN ('running','sending')", params![mod_id,worker], |r| r.get(0))?;
        let domains = json!(request.domains).to_string();
        self.0.execute("INSERT OR IGNORE INTO network_requests(mod_id,worker_id,task_id,domains,reason) VALUES(?1,?2,?3,?4,?5)", params![mod_id,worker,task,domains,request.reason])?;
        if already_allowed {
            self.0.execute("UPDATE network_requests SET status='approved',resumed=0 WHERE mod_id=?1 AND task_id=?2 AND domains=?3",params![mod_id,task,domains])?;
        }
        let status: String = self.0.query_row(
            "SELECT status FROM network_requests WHERE mod_id=?1 AND task_id=?2 AND domains=?3",
            params![mod_id, task, domains],
            |r| r.get(0),
        )?;
        Ok(format!(
            "Network access {status}: {}. Return completed:false with this blocker. The harness reconnects and retries after user approval; do not poll or bypass the policy.",
            request.domains.join(", ")
        ))
    }

    pub fn network_requests(&self, mod_id: i64) -> rusqlite::Result<Vec<Access>> {
        let mut query = self.0.prepare("SELECT r.id,r.worker_id,r.task_id,r.domains,r.reason,r.status FROM network_requests r JOIN task_runs t ON t.id=r.task_id AND t.mod_id=r.mod_id WHERE r.mod_id=?1 AND (r.status IN ('pending','denied') AND t.status!='done' OR r.status='approved' AND r.resumed=0) ORDER BY r.id")?;
        query
            .query_map([mod_id], |r| {
                Ok(Access {
                    id: r.get(0)?,
                    mod_id,
                    worker: r.get(1)?,
                    task: r.get(2)?,
                    domains: serde_json::from_str(&r.get::<_, String>(3)?).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    reason: r.get(4)?,
                    status: r.get(5)?,
                })
            })?
            .collect()
    }

    pub fn decide_network(&mut self, mod_id: i64, id: i64, allow: bool) -> rusqlite::Result<()> {
        let tx = self.0.transaction()?;
        let domains: String = tx.query_row("SELECT r.domains FROM network_requests r JOIN code_mods m ON m.id=r.mod_id WHERE r.id=?1 AND r.mod_id=?2 AND r.status='pending' AND m.closed=0",params![id,mod_id],|r|r.get(0))?;
        if allow {
            for domain in serde_json::from_str::<Vec<String>>(&domains).unwrap() {
                tx.execute(
                    "INSERT OR IGNORE INTO network_grants(mod_id,domain) VALUES(?1,?2)",
                    params![mod_id, domain],
                )?;
            }
        }
        tx.execute(
            "UPDATE network_requests SET status=?2 WHERE id=?1",
            params![id, if allow { "approved" } else { "denied" }],
        )?;
        if allow {
            tx.execute("UPDATE network_requests SET status='approved' WHERE mod_id=?1 AND status='pending' AND NOT EXISTS(SELECT 1 FROM json_each(domains) d WHERE NOT EXISTS(SELECT 1 FROM network_grants g WHERE g.mod_id=?1 AND g.domain=d.value))",[mod_id])?;
        }
        tx.commit()
    }

    pub fn resume_network(&mut self, access: &Access) -> rusqlite::Result<Option<WorkerRecord>> {
        let database = Path::new(self.0.path().unwrap()).to_owned();
        let tx = self.0.transaction()?;
        let (status, attempt, source): (String, i64, String) = tx.query_row(
            "SELECT t.status,t.attempt,t.source FROM task_runs t JOIN code_mods m ON m.id=t.mod_id WHERE t.id=?1 AND t.mod_id=?2 AND t.worker_id=?3 AND m.closed=0",
            params![access.task, access.mod_id, access.worker],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if !["pending", "paused", "blocked", "done"].contains(&status.as_str()) {
            return Ok(None);
        }
        let approved: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM network_requests WHERE id=?1 AND task_id=?2 AND mod_id=?3 AND worker_id=?4 AND status='approved' AND resumed=0)",params![access.id,access.task,access.mod_id,access.worker],|r|r.get(0))?;
        if !approved {
            return Ok(None);
        }
        tx.execute("UPDATE network_requests SET resumed=1 WHERE task_id=?1 AND mod_id=?2 AND worker_id=?3 AND status='approved' AND resumed=0",params![access.task,access.mod_id,access.worker])?;
        if status == "done" {
            tx.commit()?;
            return Ok(None);
        }
        if status != "pending" {
            tx.execute("UPDATE task_runs SET status='pending',attempt=?2,source=?3,turn_id=NULL,summary='',checks='[]' WHERE id=?1",params![access.task,attempt+1,crate::store::task_source(access.task,attempt+1)])?;
            tx.execute(
                "UPDATE workers SET pending=NULL WHERE id=?1 AND pending=?2",
                params![access.worker, source],
            )?;
        }
        let record = tx.query_row("SELECT id,provider,thread_id,pending FROM workers WHERE id=?1 AND mod_id=?2 AND role='executor'",params![access.worker,access.mod_id],|r|Ok(WorkerRecord {id:r.get(0)?,provider:r.get(1)?,thread_id:r.get(2)?,pending:r.get(3)?,database}))?;
        tx.commit()?;
        Ok(Some(record))
    }
}

pub fn grants(root: &Path) -> io::Result<BTreeMap<String, String>> {
    let database = root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("Invalid workspace location."))?
        .join("state.db");
    if !database.exists() {
        return Ok(BTreeMap::new());
    }
    let db =
        rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(io::Error::other)?;
    if db
        .query_row(
            "SELECT name FROM sqlite_master WHERE name='network_grants'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(io::Error::other)?
        .is_none()
    {
        return Ok(BTreeMap::new());
    }
    let mut query = db.prepare("SELECT g.domain FROM network_grants g JOIN executions e ON e.mod_id=g.mod_id JOIN code_mods m ON m.id=g.mod_id WHERE e.workspace=?1 AND m.closed=0").map_err(io::Error::other)?;
    query
        .query_map([root.to_string_lossy().as_ref()], |r| r.get::<_, String>(0))
        .map_err(io::Error::other)?
        .map(|domain| {
            domain
                .map(|d| (d, "allow".into()))
                .map_err(io::Error::other)
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        plan::{Plan, Role},
        store::{CodeMod, test_support::TestData},
        tools::{Context, Dispatcher, Output},
    };
    use std::{fs, path::PathBuf, sync::atomic::AtomicBool};

    pub fn fixture() -> (TestData, Store, CodeMod, PathBuf, [i64; 2]) {
        let data = TestData::new();
        let project = data.0.join("project");
        fs::create_dir_all(&project).unwrap();
        let mut store = data.store();
        let project_id = store.load_project(&project).unwrap().id;
        let mut m = store.create_mod(project_id, "Two downloads").unwrap();
        let plan=Plan::parse(&json!({"summary":"Two downloads","tasks":[
            {"id":"a","title":"First","outcome":"First download","files":["a.txt"],"depends_on":[],"worker":"codex","checks":["A exists"]},
            {"id":"b","title":"Second","outcome":"Second download","files":["b.txt"],"depends_on":[],"worker":"muse","checks":["B exists"]}
        ]}).to_string()).unwrap();
        store
            .save_plan(m.id, &m.planning.as_ref().unwrap().source, &plan)
            .unwrap();
        let root = store.workspace_path(m.id).unwrap();
        fs::create_dir_all(root.join("work")).unwrap();
        store.create_execution(m.id, &root, &plan).unwrap();
        let workers = ["codex", "muse"].map(|p| {
            store
                .worker_provider(m.id, Role::Executor, 0, p)
                .unwrap()
                .id
        });
        for worker in workers {
            store.task_input(m.id, worker, &plan).unwrap().unwrap();
        }
        m.execution = store.execution(m.id).unwrap();
        m.planning = store.planning(m.id).unwrap();
        (data, store, m, root, workers)
    }

    fn request(domain: &str) -> Request {
        Request::parse(json!({"domains":[domain],"reason":"Download fixture"})).unwrap()
    }

    #[test]
    fn explicit_retry_does_not_leave_an_approved_request_blocking_dispatch() {
        let (_data, mut store, m, _root, workers) = fixture();
        store
            .request_network(m.id, workers[0], request("example.com"), false)
            .unwrap();
        let access = store.network_requests(m.id).unwrap().remove(0);
        let source = m.execution.as_ref().unwrap().tasks[0].source.clone();
        store
            .finish_task(m.id, &source, "blocked", "Needs access", &[])
            .unwrap();
        store.decide_network(m.id, access.id, true).unwrap();
        store.retry_tasks(m.id).unwrap();
        let before = store.execution(m.id).unwrap().unwrap().tasks[0]
            .source
            .clone();
        let record = store.resume_network(&access).unwrap().unwrap();
        assert_eq!(record.id, workers[0]);
        assert_eq!(
            store.execution(m.id).unwrap().unwrap().tasks[0].source,
            before
        );
        assert!(store.network_requests(m.id).unwrap().is_empty());
    }

    #[test]
    fn validates_exact_hosts_without_hidden_terminal_or_shell_input() {
        for name in [
            "*",
            "*.example.com",
            "https://example.com",
            "example.com:443",
            "127.0.0.1",
            "::1",
            "localhost",
            "app.local",
            "a..com",
            "-a.com",
            "a_.com",
            "example.com\n",
            "user@example.com",
            "example.com/path",
        ] {
            assert!(
                Request::parse(json!({"domains":[name],"reason":"Download"})).is_err(),
                "{name}"
            );
        }
        assert!(Request::parse(json!({"domains":["example.com"],"reason":"\u{1b}[2J"})).is_err());
        assert!(
            Request::parse(json!({"domains":["example.com"],"reason":"Download","approved":true}))
                .is_err()
        );
        let value =
            Request::parse(json!({"domains":["EXAMPLE.COM","example.com"],"reason":"Download"}))
                .unwrap();
        assert_eq!(value.domains, ["example.com"]);
    }

    #[test]
    fn requests_persist_and_only_host_decisions_grant_codemod_access() {
        let (data, mut store, m, root, workers) = fixture();
        let before = crate::sandbox::network(&root).unwrap();
        let policy = fs::read(data.0.join("network.json")).unwrap();
        let flag = AtomicBool::new(false);
        for worker in workers {
            let context =
                Context::worker(&m, worker, Role::Executor).with_mailbox(&data.0.join("state.db"));
            assert!(
                crate::tools::advertised(&context)
                    .iter()
                    .any(|t| t["name"] == TOOL)
            );
            let result = Dispatcher::new(&context, None, &flag)
                .worker_call(
                    TOOL,
                    json!({"domains":["example.com"],"reason":"Download fixture"}),
                    |_| {},
                )
                .unwrap();
            assert!(matches!(result,Output::Text(text) if text.contains("pending")));
        }
        assert_eq!(grants(&root).unwrap().len(), 0);
        let first = store.network_requests(m.id).unwrap()[0].clone();
        assert!(store.decide_network(m.id + 1, first.id, true).is_err());
        let planner =
            Context::worker(&m, workers[0], Role::Planner).with_mailbox(&data.0.join("state.db"));
        assert!(
            Dispatcher::new(&planner, None, &flag)
                .worker_call(
                    TOOL,
                    json!({"domains":["example.com"],"reason":"Download"}),
                    |_| {}
                )
                .is_err()
        );
        drop(store);
        store = data.store();
        assert_eq!(store.network_requests(m.id).unwrap().len(), 2);
        store.decide_network(m.id, first.id, true).unwrap();
        assert!(
            store
                .network_requests(m.id)
                .unwrap()
                .iter()
                .all(|r| r.status == "approved")
        );
        let allowed = crate::sandbox::network(&root).unwrap();
        assert_eq!(allowed.len(), before.len() + 1);
        assert_eq!(allowed["example.com"], "allow");
        assert_eq!(fs::read(data.0.join("network.json")).unwrap(), policy);
        let other = store
            .create_mod(
                store.load_project(&data.0.join("project")).unwrap().id,
                "Other",
            )
            .unwrap();
        let other_root = store.workspace_path(other.id).unwrap();
        fs::create_dir_all(&other_root).unwrap();
        assert!(
            !crate::sandbox::network(&other_root)
                .unwrap()
                .contains_key("example.com")
        );
        store
            .request_network(m.id, workers[0], request("blocked.example"), false)
            .unwrap();
        let denied = store
            .network_requests(m.id)
            .unwrap()
            .into_iter()
            .find(|r| r.status == "pending")
            .unwrap();
        store.decide_network(m.id, denied.id, false).unwrap();
        assert!(!grants(&root).unwrap().contains_key("blocked.example"));
        assert!(
            store
                .request_network(m.id, workers[0], request("blocked.example"), false)
                .unwrap()
                .contains("denied")
        );
        assert!(store.resume_network(&denied).unwrap().is_none());
    }

    #[test]
    fn retry_is_once_and_does_not_reset_the_peer_or_saved_source() {
        let (_data, mut store, m, root, workers) = fixture();
        fs::write(root.join("work/a.txt"), "saved work").unwrap();
        store
            .request_network(m.id, workers[0], request("example.com"), false)
            .unwrap();
        let access = store.network_requests(m.id).unwrap()[0].clone();
        store
            .request_network(m.id, workers[0], request("downloads.example.com"), false)
            .unwrap();
        let second = store.network_requests(m.id).unwrap()[1].clone();
        let before = store.execution(m.id).unwrap().unwrap();
        assert!(store.resume_network(&access).unwrap().is_none());
        store.decide_network(m.id, access.id, true).unwrap();
        store.decide_network(m.id, second.id, true).unwrap();
        assert!(store.resume_network(&access).unwrap().is_none());
        let task = &before.tasks[0];
        store
            .finish_task(m.id, &task.source, "blocked", "Access needed", &[])
            .unwrap();
        assert_eq!(
            store.resume_network(&access).unwrap().unwrap().id,
            workers[0]
        );
        let after = store.execution(m.id).unwrap().unwrap();
        assert_eq!(after.tasks[0].status, "pending");
        assert_ne!(after.tasks[0].source, task.source);
        assert_eq!(after.tasks[1].source, before.tasks[1].source);
        assert_eq!(after.tasks[1].status, before.tasks[1].status);
        assert_eq!(
            fs::read_to_string(root.join("work/a.txt")).unwrap(),
            "saved work"
        );
        assert!(store.resume_network(&access).unwrap().is_none());
        assert!(store.network_requests(m.id).unwrap().is_empty());
        let project = store.load_project(&_data.0.join("project")).unwrap().id;
        store.delete_mod(project, m.id).unwrap();
        assert!(grants(&root).unwrap().is_empty());
    }

    #[test]
    fn close_and_edit_rounds_preserve_grants_without_reusing_task_requests() {
        let (data, mut store, m, root, workers) = fixture();
        store
            .request_network(m.id, workers[0], request("example.com"), false)
            .unwrap();
        let access = store.network_requests(m.id).unwrap()[0].clone();
        store.decide_network(m.id, access.id, true).unwrap();
        let project = store.load_project(&data.0.join("project")).unwrap().id;
        store.close_mod(project, m.id).unwrap();
        assert!(grants(&root).unwrap().is_empty());
        assert!(store.resume_network(&access).is_err());
        store.reopen_mod(m.id).unwrap();
        assert!(grants(&root).unwrap().contains_key("example.com"));
        store.follow_up(m.id, "Add another download", None).unwrap();
        assert!(store.network_requests(m.id).unwrap().is_empty());
        assert!(grants(&root).unwrap().contains_key("example.com"));
    }
}
