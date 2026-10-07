use std::{fs, io, path::Path, time::Duration};

use directories::ProjectDirs;
use rusqlite::{Connection, OptionalExtension, Result, params};

use crate::{
    execution::{Execution, TaskRun},
    plan::{Plan, Planning, Role},
    router::Selection,
};

pub struct CodeMod {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub closed: bool,
    pub planning: Option<Planning>,
    pub execution: Option<Execution>,
    pub git_root: Option<std::path::PathBuf>,
    pub draft: String,
    pub messages: Vec<Message>,
    pub queue: Vec<QueuedMessage>,
    pub steering: Vec<String>,
    pub coordination: Vec<crate::mailbox::Envelope>,
}

impl CodeMod {
    pub fn has_worker_history(&self) -> bool {
        self.messages.iter().any(|m| {
            m.role == "codex"
                || m.role.starts_with("codex:")
                || m.role.starts_with("muse:")
                || m.role == "planner"
                    && !m
                        .item_id
                        .as_deref()
                        .is_some_and(|id| id.starts_with("plan:"))
        }) || !self.coordination.is_empty()
    }

    pub fn question(&self) -> Option<&crate::mailbox::Envelope> {
        self.coordination.iter().find(|m| self.needs_answer(m))
    }

    pub fn needs_answer(&self, message: &crate::mailbox::Envelope) -> bool {
        !self.closed
            && message.needs_user()
            && self.execution.as_ref().is_some_and(|execution| {
                execution
                    .tasks
                    .iter()
                    .any(|run| Some(run.id) == message.task)
            })
    }
}

#[derive(Clone)]
pub struct Message {
    pub item_id: Option<String>,
    pub role: String,
    pub body: String,
    pub model: Option<String>,
    pub effort: Option<String>,
}

pub struct WorkerRecord {
    pub database: std::path::PathBuf,
    pub id: i64,
    pub provider: String,
    pub thread_id: Option<String>,
    pub pending: Option<String>,
}

#[derive(Clone)]
pub struct Submission {
    pub source: String,
    pub texts: Vec<String>,
}

#[derive(Clone)]
pub struct QueuedMessage {
    pub id: i64,
    pub body: String,
}

pub struct ProjectState {
    pub id: i64,
    pub active_mod_id: Option<i64>,
    pub mods: Vec<CodeMod>,
}

pub struct Store(pub(crate) Connection);

impl Store {
    pub fn local() -> io::Result<Self> {
        let dirs = ProjectDirs::from("", "", "sprowt-harness")
            .ok_or_else(|| io::Error::other("Cannot locate the local data directory."))?;
        Self::open(&dirs.data_local_dir().join("state.db"))
    }

    pub fn open(path: &Path) -> io::Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("Invalid database path."))?;
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        let connection = Connection::open(path).map_err(io::Error::other)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(io::Error::other)?;
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS projects (
                 id INTEGER PRIMARY KEY,
                 path TEXT NOT NULL UNIQUE,
                 active_mod_id INTEGER REFERENCES code_mods(id)
             );
             CREATE TABLE IF NOT EXISTS code_mods (
                 id INTEGER PRIMARY KEY,
                 project_id INTEGER NOT NULL REFERENCES projects(id),
                 name TEXT NOT NULL,
                 draft TEXT NOT NULL DEFAULT ''
             );
             CREATE TABLE IF NOT EXISTS messages (
                 id INTEGER PRIMARY KEY,
                 mod_id INTEGER NOT NULL REFERENCES code_mods(id),
                 body TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS queued_messages (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 mod_id INTEGER NOT NULL REFERENCES code_mods(id),
                 body TEXT NOT NULL,
                 position INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS queue_order ON queued_messages (mod_id, position);
             CREATE TABLE IF NOT EXISTS steering_requests (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 mod_id INTEGER NOT NULL REFERENCES code_mods(id)
             );
             CREATE TABLE IF NOT EXISTS steering_messages (
                 request_id INTEGER NOT NULL REFERENCES steering_requests(id),
                 position INTEGER NOT NULL,
                 body TEXT NOT NULL,
                 PRIMARY KEY (request_id, position)
             );",
            )
            .map_err(io::Error::other)?;
        let columns = connection
            .prepare("PRAGMA table_info(messages)")
            .map_err(io::Error::other)?
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(io::Error::other)?
            .collect::<Result<Vec<_>>>()
            .map_err(io::Error::other)?;
        for (name, sql) in [
            (
                "role",
                "ALTER TABLE messages ADD COLUMN role TEXT NOT NULL DEFAULT 'user'",
            ),
            ("item_id", "ALTER TABLE messages ADD COLUMN item_id TEXT"),
            ("model", "ALTER TABLE messages ADD COLUMN model TEXT"),
            ("effort", "ALTER TABLE messages ADD COLUMN effort TEXT"),
        ] {
            if !columns.iter().any(|column| column == name) {
                connection.execute_batch(sql).map_err(io::Error::other)?;
            }
        }
        connection
            .execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS message_items ON messages(mod_id, item_id);
            CREATE TABLE IF NOT EXISTS workers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                mod_id INTEGER NOT NULL REFERENCES code_mods(id),
                thread_id TEXT,
                pending TEXT
            );
            CREATE INDEX IF NOT EXISTS mod_workers ON workers(mod_id, id);",
            )
            .map_err(io::Error::other)?;
        for (table, column, sql) in [
            (
                "workers",
                "provider",
                "ALTER TABLE workers ADD COLUMN provider TEXT NOT NULL DEFAULT 'codex'",
            ),
            (
                "code_mods",
                "closed",
                "ALTER TABLE code_mods ADD COLUMN closed INTEGER NOT NULL DEFAULT 0",
            ),
            (
                "code_mods",
                "closed_at",
                "ALTER TABLE code_mods ADD COLUMN closed_at INTEGER",
            ),
            (
                "code_mods",
                "description",
                "ALTER TABLE code_mods ADD COLUMN description TEXT",
            ),
            (
                "workers",
                "role",
                "ALTER TABLE workers ADD COLUMN role TEXT NOT NULL DEFAULT 'executor'",
            ),
        ] {
            let columns = connection
                .prepare(&format!("PRAGMA table_info({table})"))
                .map_err(io::Error::other)?
                .query_map([], |row| row.get::<_, String>(1))
                .map_err(io::Error::other)?
                .collect::<Result<Vec<_>>>()
                .map_err(io::Error::other)?;
            if !columns.iter().any(|name| name == column) {
                connection.execute_batch(sql).map_err(io::Error::other)?;
            }
        }
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS plans (
            mod_id INTEGER PRIMARY KEY REFERENCES code_mods(id),
            status TEXT NOT NULL DEFAULT 'pending',
            source TEXT NOT NULL,
            attempt INTEGER NOT NULL DEFAULT 1,
            body TEXT, model TEXT, effort TEXT, routing TEXT
        );",
            )
            .map_err(io::Error::other)?;
        connection.execute_batch("CREATE TABLE IF NOT EXISTS executions (
            mod_id INTEGER PRIMARY KEY REFERENCES code_mods(id), workspace TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending', checks TEXT NOT NULL DEFAULT '[]', fingerprint TEXT
        );
        CREATE TABLE IF NOT EXISTS task_runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT, mod_id INTEGER NOT NULL REFERENCES code_mods(id), task_id TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending', attempt INTEGER NOT NULL DEFAULT 1, source TEXT NOT NULL UNIQUE,
            turn_id TEXT, summary TEXT NOT NULL DEFAULT '', checks TEXT NOT NULL DEFAULT '[]', UNIQUE(mod_id,task_id)
        );").map_err(io::Error::other)?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS mod_worktrees (
            mod_id INTEGER PRIMARY KEY REFERENCES code_mods(id), workspace TEXT NOT NULL
        );",
            )
            .map_err(io::Error::other)?;
        let columns = connection
            .prepare("PRAGMA table_info(executions)")
            .map_err(io::Error::other)?
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(io::Error::other)?
            .collect::<Result<Vec<_>>>()
            .map_err(io::Error::other)?;
        for (column, sql) in [
            (
                "backend",
                "ALTER TABLE executions ADD COLUMN backend TEXT NOT NULL DEFAULT 'local'",
            ),
            (
                "checks",
                "ALTER TABLE executions ADD COLUMN checks TEXT NOT NULL DEFAULT '[]'",
            ),
            (
                "fingerprint",
                "ALTER TABLE executions ADD COLUMN fingerprint TEXT",
            ),
        ] {
            if !columns.iter().any(|name| name == column) {
                connection.execute_batch(sql).map_err(io::Error::other)?;
            }
        }
        connection
            .execute_batch(crate::mailbox::SCHEMA)
            .map_err(io::Error::other)?;
        connection
            .execute_batch(crate::network::SCHEMA)
            .map_err(io::Error::other)?;
        let has_worker = connection
            .prepare("PRAGMA table_info(task_runs)")
            .map_err(io::Error::other)?
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(io::Error::other)?
            .collect::<Result<Vec<_>>>()
            .map_err(io::Error::other)?
            .iter()
            .any(|name| name == "worker_id");
        if !has_worker {
            connection
                .execute_batch("ALTER TABLE task_runs ADD COLUMN worker_id INTEGER")
                .map_err(io::Error::other)?;
        }
        let has_routing = connection
            .prepare("PRAGMA table_info(task_runs)")
            .map_err(io::Error::other)?
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(io::Error::other)?
            .collect::<Result<Vec<_>>>()
            .map_err(io::Error::other)?
            .iter()
            .any(|name| name == "routing");
        if !has_routing {
            connection
                .execute_batch("ALTER TABLE task_runs ADD COLUMN routing TEXT")
                .map_err(io::Error::other)?;
        }
        crate::repair::migrate(&connection).map_err(io::Error::other)?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS steering_deliveries (
            request_id INTEGER NOT NULL REFERENCES steering_requests(id) ON DELETE CASCADE,
            worker_id INTEGER NOT NULL REFERENCES workers(id),
            delivered INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(request_id,worker_id)
        );",
            )
            .map_err(io::Error::other)?;
        Ok(Self(connection))
    }

    pub fn load_project(&self, path: &Path) -> Result<ProjectState> {
        let path = path.to_string_lossy();
        self.0.execute(
            "INSERT OR IGNORE INTO projects (path) VALUES (?1)",
            [path.as_ref()],
        )?;
        let (id, active_mod_id) = self.0.query_row(
            "SELECT id, active_mod_id FROM projects WHERE path = ?1",
            [path.as_ref()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut statement = self
            .0
            .prepare("SELECT id, name, draft, COALESCE(description,name), closed FROM code_mods WHERE project_id = ?1 ORDER BY id")?;
        let mut mods = statement
            .query_map([id], |row| {
                Ok(CodeMod {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(3)?,
                    closed: row.get(4)?,
                    planning: None,
                    execution: None,
                    git_root: None,
                    draft: row.get(2)?,
                    messages: Vec::new(),
                    queue: Vec::new(),
                    steering: Vec::new(),
                    coordination: Vec::new(),
                })
            })?
            .collect::<Result<Vec<_>>>()?;
        for code_mod in &mut mods {
            code_mod.planning = self.planning(code_mod.id)?;
            code_mod.execution = self.execution(code_mod.id)?;
            code_mod.git_root = self.git_root(code_mod.id)?;
            code_mod.coordination = self.mailbox(code_mod.id)?;
            let mut statement = self.0.prepare(
                "SELECT item_id, role, body, model, effort FROM messages WHERE mod_id = ?1 ORDER BY id",
            )?;
            code_mod.messages = statement
                .query_map([code_mod.id], |row| {
                    Ok(Message {
                        item_id: row.get(0)?,
                        role: row.get(1)?,
                        body: row.get(2)?,
                        model: row.get(3)?,
                        effort: row.get(4)?,
                    })
                })?
                .collect::<Result<Vec<_>>>()?;
            let mut statement = self.0.prepare(
                "SELECT m.body FROM steering_messages m
                 JOIN steering_requests r ON r.id = m.request_id
                 WHERE r.mod_id = ?1 ORDER BY r.id, m.position",
            )?;
            code_mod.steering = statement
                .query_map([code_mod.id], |row| row.get(0))?
                .collect::<Result<Vec<_>>>()?;
            let mut statement = self.0.prepare(
                "SELECT id, body FROM queued_messages WHERE mod_id = ?1 ORDER BY position, id",
            )?;
            code_mod.queue = statement
                .query_map([code_mod.id], |row| {
                    Ok(QueuedMessage {
                        id: row.get(0)?,
                        body: row.get(1)?,
                    })
                })?
                .collect::<Result<Vec<_>>>()?;
        }
        Ok(ProjectState {
            id,
            active_mod_id,
            mods,
        })
    }

    pub fn create_mod(&mut self, project_id: i64, name: &str) -> Result<CodeMod> {
        self.create_mod_with_plan(project_id, name, true)
    }

    #[cfg(test)]
    fn legacy_mod(&mut self, project_id: i64, name: &str) -> Result<CodeMod> {
        self.create_mod_with_plan(project_id, name, false)
    }

    fn create_mod_with_plan(
        &mut self,
        project_id: i64,
        name: &str,
        planned: bool,
    ) -> Result<CodeMod> {
        let title = name
            .lines()
            .next()
            .unwrap_or(name)
            .chars()
            .take(80)
            .collect::<String>();
        let transaction = self.0.transaction()?;
        transaction.execute(
            "INSERT INTO code_mods (project_id, name, description) VALUES (?1, ?2, ?3)",
            params![project_id, title, name],
        )?;
        let id = transaction.last_insert_rowid();
        transaction.execute(
            "UPDATE projects SET active_mod_id = ?1 WHERE id = ?2",
            params![id, project_id],
        )?;
        let message = Message {
            item_id: Some(format!("description:{id}")),
            role: "user".into(),
            body: name.to_owned(),
            model: None,
            effort: None,
        };
        if planned {
            transaction.execute(
                "INSERT INTO plans(mod_id,source) VALUES (?1,?2)",
                params![id, plan_source(id, 1)],
            )?;
            transaction.execute(
                "INSERT INTO messages(mod_id,item_id,role,body) VALUES (?1,?2,'user',?3)",
                params![id, message.item_id, message.body],
            )?;
        }
        transaction.commit()?;
        Ok(CodeMod {
            id,
            name: title,
            description: name.to_owned(),
            closed: false,
            planning: self.planning(id)?,
            execution: None,
            git_root: None,
            draft: String::new(),
            messages: if planned { vec![message] } else { Vec::new() },
            queue: Vec::new(),
            steering: Vec::new(),
            coordination: Vec::new(),
        })
    }

    pub fn select_mod(&self, project_id: i64, mod_id: i64) -> Result<()> {
        let changed = self.0.execute(
            "UPDATE projects SET active_mod_id = ?1 WHERE id = ?2
             AND EXISTS (SELECT 1 FROM code_mods WHERE id = ?1 AND project_id = ?2)",
            params![mod_id, project_id],
        )?;
        if changed == 0 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok(())
    }

    pub fn delete_mod(&mut self, project_id: i64, mod_id: i64) -> Result<Option<i64>> {
        let transaction = self.0.transaction()?;
        let active: Option<i64> = transaction.query_row(
            "SELECT active_mod_id FROM projects WHERE id = ?1
             AND EXISTS (SELECT 1 FROM code_mods WHERE id = ?2 AND project_id = ?1)",
            params![project_id, mod_id],
            |row| row.get(0),
        )?;
        let selected = if active == Some(mod_id) {
            transaction.query_row(
                "SELECT id FROM code_mods WHERE project_id = ?1 AND id != ?2 AND closed=0 ORDER BY id LIMIT 1",
                params![project_id, mod_id],
                |row| row.get(0),
            ).optional()?
        } else {
            active
        };
        transaction.execute("DELETE FROM steering_deliveries WHERE request_id IN (SELECT id FROM steering_requests WHERE mod_id=?1)", [mod_id])?;
        transaction.execute(
            "DELETE FROM steering_messages WHERE request_id IN
             (SELECT id FROM steering_requests WHERE mod_id = ?1)",
            [mod_id],
        )?;
        for table in [
            "network_requests",
            "network_grants",
            "task_runs",
            "executions",
            "steering_requests",
            "queued_messages",
            "messages",
            "workers",
            "plans",
            "mod_worktrees",
        ] {
            transaction.execute(&format!("DELETE FROM {table} WHERE mod_id = ?1"), [mod_id])?;
        }
        transaction.execute(
            "UPDATE projects SET active_mod_id = ?1 WHERE id = ?2",
            params![selected, project_id],
        )?;
        transaction.execute("DELETE FROM code_mods WHERE id = ?1", [mod_id])?;
        transaction.commit()?;
        Ok(selected)
    }

    pub fn save_draft(&self, mod_id: i64, draft: &str) -> Result<()> {
        self.0.execute(
            "UPDATE code_mods SET draft = ?1 WHERE id = ?2",
            params![draft, mod_id],
        )?;
        Ok(())
    }

    pub fn close_mod(&mut self, project_id: i64, mod_id: i64) -> Result<Option<i64>> {
        let transaction = self.0.transaction()?;
        let changed = transaction.execute(
            "UPDATE code_mods SET closed=1,closed_at=unixepoch() WHERE id=?1 AND project_id=?2",
            params![mod_id, project_id],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        transaction.execute("UPDATE task_runs SET status='paused' WHERE mod_id=?1 AND status IN ('sending','running','checking')", [mod_id])?;
        transaction.execute("UPDATE executions SET status='blocked' WHERE mod_id=?1 AND status IN ('running','verifying')", [mod_id])?;
        transaction.execute("UPDATE plans SET status='paused' WHERE mod_id=?1 AND status IN ('pending','running','sending')", [mod_id])?;
        transaction.execute(
            "UPDATE workers SET thread_id=NULL,pending=NULL WHERE mod_id=?1",
            [mod_id],
        )?;
        transaction.execute("UPDATE projects SET active_mod_id=(SELECT id FROM code_mods WHERE project_id=?1 AND closed=0 ORDER BY id LIMIT 1) WHERE id=?1 AND active_mod_id=?2", params![project_id, mod_id])?;
        let selected = transaction.query_row(
            "SELECT active_mod_id FROM projects WHERE id=?1",
            [project_id],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(selected)
    }

    pub fn follow_up(&mut self, mod_id: i64, request: &str, queued: Option<i64>) -> Result<()> {
        let transaction = self.0.transaction()?;
        let (description, attempt): (String, i64) = transaction.query_row("SELECT COALESCE(m.description,m.name),COALESCE(p.attempt,0) FROM code_mods m LEFT JOIN plans p ON p.mod_id=m.id WHERE m.id=?1", [mod_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let source = plan_source(mod_id, attempt + 1);
        transaction.execute("UPDATE code_mods SET closed=0,closed_at=NULL,description=?2 WHERE id=?1", params![mod_id, format!("{description}\n\nUse the current source and finish any incomplete work. Plan this edit, including checks for the new behavior and regressions:\n{request}")])?;
        transaction.execute("DELETE FROM network_requests WHERE mod_id=?1", [mod_id])?;
        transaction.execute("DELETE FROM task_runs WHERE mod_id=?1", [mod_id])?;
        transaction.execute(
            "UPDATE executions SET status='planning',checks='[]',fingerprint=NULL WHERE mod_id=?1",
            [mod_id],
        )?;
        if let Some(id) = queued {
            let changed = transaction.execute(
                "DELETE FROM queued_messages WHERE mod_id=?1 AND id=?2 AND body=?3",
                params![mod_id, id, request],
            )?;
            if changed != 1 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
        }
        transaction.execute("INSERT INTO plans(mod_id,source,attempt) VALUES (?1,?2,?3) ON CONFLICT(mod_id) DO UPDATE SET status='pending',body=NULL,source=excluded.source,attempt=excluded.attempt,model=NULL,effort=NULL,routing=NULL", params![mod_id, source, attempt+1])?;
        transaction.execute(
            "UPDATE workers SET thread_id=NULL,pending=NULL WHERE mod_id=?1",
            [mod_id],
        )?;
        transaction.execute(
            "INSERT INTO messages(mod_id,item_id,role,body) VALUES (?1,?2,'user',?3)",
            params![mod_id, source, request],
        )?;
        transaction.commit()
    }

    pub fn reopen_mod(&self, mod_id: i64) -> Result<()> {
        self.0.execute(
            "UPDATE code_mods SET closed=0,closed_at=NULL WHERE id=?1",
            [mod_id],
        )?;
        self.0.execute(
            "UPDATE workers SET thread_id=NULL,pending=NULL WHERE mod_id=?1",
            [mod_id],
        )?;
        Ok(())
    }

    pub fn expired_worktrees(
        &self,
        project_id: i64,
        days: u32,
    ) -> Result<Vec<(i64, std::path::PathBuf)>> {
        if days == 0 {
            return Ok(Vec::new());
        }
        self.0.prepare("SELECT m.id,w.workspace FROM code_mods m JOIN mod_worktrees w ON w.mod_id=m.id WHERE m.project_id=?1 AND m.closed=1 AND m.closed_at IS NOT NULL AND m.closed_at <= unixepoch()-?2")?
            .query_map(params![project_id,i64::from(days)*86400], |row| Ok((row.get(0)?,std::path::PathBuf::from(row.get::<_,String>(1)?))))?.collect()
    }

    pub fn git_root(&self, mod_id: i64) -> Result<Option<std::path::PathBuf>> {
        self.0
            .query_row(
                "SELECT workspace FROM mod_worktrees WHERE mod_id=?1",
                [mod_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map(|path| path.map(Into::into))
    }

    pub fn save_git_root(&self, mod_id: i64, root: &Path) -> Result<()> {
        self.0.execute(
            "INSERT INTO mod_worktrees(mod_id,workspace) VALUES (?1,?2)",
            params![mod_id, root.to_string_lossy()],
        )?;
        Ok(())
    }

    pub fn enqueue(&mut self, mod_id: i64, body: &str) -> Result<QueuedMessage> {
        let transaction = self.0.transaction()?;
        transaction.execute(
            "INSERT INTO queued_messages (mod_id, body, position)
             SELECT ?1, ?2, COALESCE(MAX(position) + 1, 0) FROM queued_messages WHERE mod_id = ?1",
            params![mod_id, body],
        )?;
        let id = transaction.last_insert_rowid();
        transaction.execute("UPDATE code_mods SET draft = '' WHERE id = ?1", [mod_id])?;
        transaction.commit()?;
        Ok(QueuedMessage {
            id,
            body: body.to_owned(),
        })
    }

    pub fn edit_queued(&self, mod_id: i64, id: i64, body: &str) -> Result<()> {
        let changed = self.0.execute(
            "UPDATE queued_messages SET body = ?1 WHERE mod_id = ?2 AND id = ?3",
            params![body, mod_id, id],
        )?;
        if changed == 0 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok(())
    }

    pub fn remove_queued(&self, mod_id: i64, id: i64) -> Result<()> {
        let changed = self.0.execute(
            "DELETE FROM queued_messages WHERE mod_id = ?1 AND id = ?2",
            params![mod_id, id],
        )?;
        if changed == 0 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok(())
    }

    pub fn swap_queued(&mut self, mod_id: i64, first: i64, second: i64) -> Result<()> {
        let transaction = self.0.transaction()?;
        let position = |id| {
            transaction.query_row(
                "SELECT position FROM queued_messages WHERE mod_id = ?1 AND id = ?2",
                params![mod_id, id],
                |row| row.get::<_, i64>(0),
            )
        };
        let (first_position, second_position) = (position(first)?, position(second)?);
        transaction.execute(
            "UPDATE queued_messages SET position = CASE id WHEN ?1 THEN ?2 ELSE ?3 END
             WHERE mod_id = ?4 AND id IN (?1, ?5)",
            params![first, second_position, first_position, mod_id, second],
        )?;
        transaction.commit()
    }

    #[cfg(test)]
    pub fn request_steering(&mut self, mod_id: i64, ids: &[i64]) -> Result<Vec<String>> {
        self.steer_to(mod_id, ids, &[])
    }

    pub fn steer_to(&mut self, mod_id: i64, ids: &[i64], workers: &[i64]) -> Result<Vec<String>> {
        if ids.is_empty() {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        let transaction = self.0.transaction()?;
        let mut messages = {
            let mut statement = transaction.prepare(
                "SELECT id, body FROM queued_messages WHERE mod_id = ?1 ORDER BY position, id",
            )?;
            statement
                .query_map([mod_id], |row| {
                    Ok(QueuedMessage {
                        id: row.get(0)?,
                        body: row.get(1)?,
                    })
                })?
                .collect::<Result<Vec<_>>>()?
        };
        messages.retain(|message| ids.contains(&message.id));
        if messages.len() != ids.len() {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        transaction.execute(
            "INSERT INTO steering_requests (mod_id) VALUES (?1)",
            [mod_id],
        )?;
        let request_id = transaction.last_insert_rowid();
        for worker in workers {
            let valid: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM workers WHERE id=?1 AND mod_id=?2)",
                params![worker, mod_id],
                |r| r.get(0),
            )?;
            if !valid {
                return Err(rusqlite::Error::InvalidQuery);
            }
            transaction.execute(
                "INSERT INTO steering_deliveries(request_id,worker_id) VALUES (?1,?2)",
                params![request_id, worker],
            )?;
        }
        let mut bodies = Vec::new();
        for (position, message) in messages.into_iter().enumerate() {
            transaction.execute(
                "INSERT INTO steering_messages (request_id, position, body) VALUES (?1, ?2, ?3)",
                params![request_id, position as i64, message.body],
            )?;
            transaction.execute(
                "DELETE FROM queued_messages WHERE mod_id = ?1 AND id = ?2",
                params![mod_id, message.id],
            )?;
            bodies.push(message.body);
        }
        transaction.commit()?;
        Ok(bodies)
    }
    #[cfg(test)]
    pub fn worker(&self, mod_id: i64) -> Result<WorkerRecord> {
        self.worker_for(mod_id, Role::Executor)
    }

    #[cfg(test)]
    pub fn worker_for(&self, mod_id: i64, role: Role) -> Result<WorkerRecord> {
        self.worker_at(mod_id, role, 0)
    }

    #[cfg(test)]
    pub fn worker_at(&self, mod_id: i64, role: Role, slot: usize) -> Result<WorkerRecord> {
        self.worker_provider(mod_id, role, slot, "codex")
    }

    pub fn worker_provider(
        &self,
        mod_id: i64,
        role: Role,
        slot: usize,
        provider: &str,
    ) -> Result<WorkerRecord> {
        if !["codex", "muse"].contains(&provider) || role == Role::Planner && provider != "codex" {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let read = || {
            self.0.query_row("SELECT id, thread_id, pending FROM workers WHERE mod_id = ?1 AND role = ?2 AND provider = ?4 ORDER BY id LIMIT 1 OFFSET ?3", params![mod_id,role.name(),slot as i64,provider],
            |row| Ok(WorkerRecord { database:Path::new(self.0.path().unwrap()).to_owned(),
                id:row.get(0)?, provider:provider.into(), thread_id:row.get(1)?, pending:row.get(2)? })).optional()
        };
        if let Some(worker) = read()? {
            return Ok(worker);
        }
        self.0.execute(
            "INSERT INTO workers(mod_id,role,provider) VALUES (?1,?2,?3)",
            params![mod_id, role.name(), provider],
        )?;
        read()?.ok_or(rusqlite::Error::QueryReturnedNoRows)
    }

    pub fn save_thread(&self, worker: i64, thread: &str) -> Result<()> {
        self.0.execute(
            "UPDATE workers SET thread_id = ?1 WHERE id = ?2",
            params![thread, worker],
        )?;
        Ok(())
    }

    pub fn pending(&self, worker: i64, source: Option<&str>) -> Result<()> {
        self.0.execute(
            "UPDATE workers SET pending = ?1 WHERE id = ?2",
            params![source, worker],
        )?;
        Ok(())
    }

    pub fn is_pending(&self, mod_id: i64, source: &str) -> Result<bool> {
        self.0.query_row(
            "SELECT EXISTS(SELECT 1 FROM workers WHERE mod_id=?1 AND pending=?2)",
            params![mod_id, source],
            |row| row.get(0),
        )
    }

    pub fn submission(&self, mod_id: i64, source: &str) -> Result<Submission> {
        let id = source
            .rsplit('-')
            .next()
            .and_then(|id| i64::from_str_radix(id, 16).ok())
            .ok_or(rusqlite::Error::InvalidQuery)?;
        let texts = if source.starts_with("00000001-") {
            vec![self.0.query_row(
                "SELECT body FROM queued_messages WHERE mod_id = ?1 AND id = ?2",
                params![mod_id, id],
                |row| row.get(0),
            )?]
        } else if source.starts_with("00000002-") {
            let mut statement = self.0.prepare("SELECT m.body FROM steering_messages m JOIN steering_requests r ON r.id = m.request_id WHERE r.mod_id = ?1 AND r.id = ?2 ORDER BY m.position")?;
            statement
                .query_map(params![mod_id, id], |row| row.get(0))?
                .collect::<Result<Vec<_>>>()?
        } else if source.starts_with("00000003-") {
            vec![self.0.query_row("SELECT description FROM code_mods c JOIN plans p ON p.mod_id=c.id WHERE c.id=?1 AND p.source=?2", params![mod_id,source], |row| row.get(0))?]
        } else if source.starts_with("00000004-") {
            let execution = self
                .execution(mod_id)?
                .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
            let run = execution
                .tasks
                .iter()
                .find(|run| run.source == source)
                .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
            let plan = self
                .planning(mod_id)?
                .and_then(|p| p.plan)
                .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
            let task = plan
                .tasks
                .iter()
                .find(|task| task.id == run.task_id)
                .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
            let mut prompt = crate::execution::task_prompt(&plan, task, run.id);
            if let Some(repair) = &run.repair {
                prompt.push_str(&format!(
                    "\nAutomatic repair context: {}\n{} Preserve saved edits. The harness refreshed this task from the combined source. Reproduce the failure using the evidence and command (adapt task paths to /tasks/{}); keep any temporary reproduction scripts outside source. The repair's failing check is supplemental: do not replace your original task checks with it. The report checks array must contain your current task's declared checks, with their exact text and order. Integration and all final checks must pass again.",
                    serde_json::to_string(repair).unwrap(),
                    if task.id == repair.task { "Fix this regression within your original file scope." } else { "Rerun this affected task after the owner fixed the regression; keep your original file scope." },
                    run.id
                ));
            }
            let inbox = self.task_mail(mod_id, run.id)?;
            if !inbox.is_empty() {
                prompt.push_str(&format!(
                    "\nSaved inbox (acknowledge received IDs):\n{inbox}"
                ));
            }
            if let Some(update) = crate::mod_sync::load(&execution.workspace)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
            {
                prompt.push_str(&format!("\nIntegration context:\n{}", update.context));
            }
            vec![prompt]
        } else if source.starts_with("00000005-") {
            vec![self.mail_prompt(mod_id, id)?]
        } else {
            return Err(rusqlite::Error::InvalidQuery);
        };
        if texts.is_empty() {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok(Submission {
            source: source.to_owned(),
            texts,
        })
    }

    pub fn next_input(&self, mod_id: i64, steering: bool) -> Result<Option<Submission>> {
        let query = if steering {
            "SELECT id FROM steering_requests WHERE mod_id = ?1 ORDER BY id LIMIT 1"
        } else {
            "SELECT id FROM queued_messages WHERE mod_id = ?1 ORDER BY position, id LIMIT 1"
        };
        let id: Option<i64> = self
            .0
            .query_row(query, [mod_id], |row| row.get(0))
            .optional()?;
        id.map(|id| self.submission(mod_id, &source_id(id, steering)))
            .transpose()
    }

    pub fn next_steering(&self, mod_id: i64, worker: i64) -> Result<Option<Submission>> {
        let id: Option<i64> = self.0.query_row("SELECT r.id FROM steering_requests r
            WHERE r.mod_id=?1 AND (
                EXISTS(SELECT 1 FROM steering_deliveries d WHERE d.request_id=r.id AND d.worker_id=?2 AND d.delivered=0)
                OR NOT EXISTS(SELECT 1 FROM steering_deliveries d WHERE d.request_id=r.id))
            ORDER BY r.id LIMIT 1", params![mod_id,worker], |r| r.get(0)).optional()?;
        let input = id
            .map(|id| self.submission(mod_id, &source_id(id, true)))
            .transpose()?;
        if let Some(input) = &input {
            let assigned: bool = self.0.query_row(
                "SELECT EXISTS(SELECT 1 FROM steering_deliveries WHERE request_id=?1)",
                [id.unwrap()],
                |r| r.get(0),
            )?;
            if !assigned && self.is_pending(mod_id, &input.source)? {
                return Ok(None);
            }
        }
        Ok(input)
    }

    pub fn reject_steering(
        &mut self,
        mod_id: i64,
        worker: i64,
        source: &str,
    ) -> Result<Vec<QueuedMessage>> {
        let input = self.submission(mod_id, source)?;
        let id = i64::from_str_radix(source.rsplit('-').next().unwrap_or(""), 16)
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        let tx = self.0.transaction()?;
        let targeted: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM steering_deliveries WHERE request_id=?1)",
            [id],
            |r| r.get(0),
        )?;
        let returned: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM steering_deliveries WHERE request_id=?1 AND delivered=-1)",
            [id],
            |r| r.get(0),
        )?;
        if targeted && tx.execute("UPDATE steering_deliveries SET delivered=-1 WHERE request_id=?1 AND worker_id=?2 AND delivered=0", params![id,worker])? != 1 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let mut queued = Vec::new();
        if !returned {
            for body in input.texts {
                tx.execute("INSERT INTO queued_messages(mod_id,position,body) VALUES (?1,(SELECT COALESCE(MAX(position),-1)+1 FROM queued_messages WHERE mod_id=?1),?2)", params![mod_id,body])?;
                queued.push(QueuedMessage {
                    id: tx.last_insert_rowid(),
                    body,
                });
            }
        }
        let waiting: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM steering_deliveries WHERE request_id=?1 AND delivered=0)",
            [id],
            |r| r.get(0),
        )?;
        if !waiting {
            tx.execute("DELETE FROM steering_deliveries WHERE request_id=?1", [id])?;
            tx.execute("DELETE FROM steering_messages WHERE request_id=?1", [id])?;
            tx.execute("DELETE FROM steering_requests WHERE id=?1", [id])?;
        }
        tx.execute("UPDATE workers SET pending=NULL WHERE id=?1", [worker])?;
        tx.commit()?;
        Ok(queued)
    }

    pub fn steering(&self, mod_id: i64) -> Result<Vec<String>> {
        self.0.prepare("SELECT m.body FROM steering_messages m JOIN steering_requests r ON r.id=m.request_id WHERE r.mod_id=?1 ORDER BY r.id,m.position")?
            .query_map([mod_id], |r| r.get(0))?.collect()
    }

    pub fn acknowledge(&mut self, worker: i64, input: &Submission) -> Result<()> {
        let transaction = self.0.transaction()?;
        let mod_id: i64 = transaction.query_row(
            "SELECT mod_id FROM workers WHERE id = ?1 AND pending = ?2",
            params![worker, input.source],
            |row| row.get(0),
        )?;
        if input.source.starts_with("00000005-") {
            let id = i64::from_str_radix(input.source.rsplit('-').next().unwrap(), 16)
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            if transaction.execute(
                "UPDATE worker_messages SET delivered_by=COALESCE(delivered_by,?3)
                WHERE mod_id=?1 AND id=?2 AND recipient_task IN(
                    SELECT id FROM task_runs WHERE mod_id=?1 AND worker_id=?3)",
                params![mod_id, id, worker],
            )? != 1
            {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            transaction.execute("UPDATE workers SET pending=NULL WHERE id=?1", [worker])?;
            return transaction.commit();
        }
        if input.source.starts_with("00000003-") {
            let changed = transaction.execute(
                "UPDATE plans SET status='running' WHERE mod_id=?1 AND source=?2",
                params![mod_id, input.source],
            )?;
            if changed != 1 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            transaction.execute("UPDATE workers SET pending=NULL WHERE id=?1", [worker])?;
            return transaction.commit();
        }
        if input.source.starts_with("00000004-") {
            let changed = transaction.execute("UPDATE task_runs SET status='running' WHERE mod_id=?1 AND source=?2 AND status IN ('sending','running','paused')", params![mod_id,input.source])?;
            if changed != 1 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            transaction.execute("UPDATE workers SET pending=NULL WHERE id=?1", [worker])?;
            return transaction.commit();
        }
        let id = i64::from_str_radix(input.source.rsplit('-').next().unwrap(), 16)
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        transaction.execute("INSERT INTO messages(mod_id,item_id,role,body) VALUES (?1,?2,'user',?3) ON CONFLICT(mod_id,item_id) DO UPDATE SET body=excluded.body", params![mod_id,input.source,input.texts.join("\n\n")])?;
        let changed = if input.source.starts_with("00000001-") {
            transaction.execute(
                "DELETE FROM queued_messages WHERE mod_id=?1 AND id=?2",
                params![mod_id, id],
            )?
        } else {
            let belongs: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM steering_requests WHERE mod_id=?1 AND id=?2)",
                params![mod_id, id],
                |row| row.get(0),
            )?;
            if !belongs {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            let targeted: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM steering_deliveries WHERE request_id=?1)",
                [id],
                |r| r.get(0),
            )?;
            if targeted {
                let changed = transaction.execute("UPDATE steering_deliveries SET delivered=1 WHERE request_id=?1 AND worker_id=?2 AND delivered=0", params![id,worker])?;
                if changed != 1 {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                let waiting: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM steering_deliveries WHERE request_id=?1 AND delivered=0)", [id], |r| r.get(0))?;
                if waiting {
                    transaction.execute("UPDATE workers SET pending=NULL WHERE id=?1", [worker])?;
                    return transaction.commit();
                }
            }
            transaction.execute("DELETE FROM steering_deliveries WHERE request_id=?1", [id])?;
            transaction.execute("DELETE FROM steering_messages WHERE request_id=?1", [id])?;
            transaction.execute(
                "DELETE FROM steering_requests WHERE mod_id=?1 AND id=?2",
                params![mod_id, id],
            )?
        };
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        transaction.execute("UPDATE workers SET pending=NULL WHERE id=?1", [worker])?;
        transaction.commit()
    }

    pub fn planning(&self, mod_id: i64) -> Result<Option<Planning>> {
        self.0
            .query_row(
                "SELECT status,source,model,effort,routing,body FROM plans WHERE mod_id=?1",
                [mod_id],
                |row| {
                    let body: Option<String> = row.get(5)?;
                    let plan = body
                        .map(|body| {
                            serde_json::from_str::<Plan>(&body).map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    5,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })
                        })
                        .transpose()?;
                    Ok(Planning {
                        status: row.get(0)?,
                        source: row.get(1)?,
                        model: row.get(2)?,
                        effort: row.get(3)?,
                        routing: row.get(4)?,
                        plan,
                    })
                },
            )
            .optional()
    }

    pub fn planner_input(&self, mod_id: i64) -> Result<Option<Submission>> {
        let source: Option<String> = self
            .0
            .query_row(
                "SELECT source FROM plans WHERE mod_id=?1 AND status='pending'",
                [mod_id],
                |row| row.get(0),
            )
            .optional()?;
        source
            .map(|source| self.submission(mod_id, &source))
            .transpose()
    }

    pub fn retry_plan(&self, mod_id: i64) -> Result<()> {
        let attempt: i64 = self.0.query_row(
            "SELECT attempt FROM plans WHERE mod_id=?1",
            [mod_id],
            |row| row.get(0),
        )?;
        self.0.execute(
            "UPDATE plans SET status='pending',attempt=?2,source=?3 WHERE mod_id=?1",
            params![mod_id, attempt + 1, plan_source(mod_id, attempt + 1)],
        )?;
        Ok(())
    }

    pub fn planning_status(&self, mod_id: i64, status: &str) -> Result<()> {
        self.0.execute(
            "UPDATE plans SET status=?2 WHERE mod_id=?1",
            params![mod_id, status],
        )?;
        Ok(())
    }

    pub fn restart_plan(&mut self, mod_id: i64) -> Result<()> {
        let transaction = self.0.transaction()?;
        let attempt: i64 = transaction.query_row(
            "SELECT attempt FROM plans WHERE mod_id=?1",
            [mod_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "UPDATE plans SET status='pending',attempt=?2,source=?3,body=NULL WHERE mod_id=?1",
            params![mod_id, attempt + 1, plan_source(mod_id, attempt + 1)],
        )?;
        transaction.execute(
            "UPDATE workers SET thread_id=NULL,pending=NULL WHERE mod_id=?1 AND role='planner'",
            [mod_id],
        )?;
        transaction.commit()
    }

    pub fn planning_model(&self, mod_id: i64, selection: &Selection) -> Result<()> {
        self.0.execute(
            "UPDATE plans SET model=?2,effort=?3,routing=?4 WHERE mod_id=?1",
            params![mod_id, selection.model, selection.effort, selection.reason],
        )?;
        Ok(())
    }

    pub fn save_plan(&mut self, mod_id: i64, source: &str, plan: &Plan) -> Result<Message> {
        plan.validate().map_err(|_| rusqlite::Error::InvalidQuery)?;
        let transaction = self.0.transaction()?;
        let changed = transaction.execute(
            "UPDATE plans SET status='ready',body=?3 WHERE mod_id=?1 AND source=?2",
            params![mod_id, source, serde_json::to_string(plan).unwrap()],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        let message = Message {
            item_id: Some(format!("plan:{source}")),
            role: "planner".into(),
            body: plan.display(),
            model: None,
            effort: None,
        };
        transaction.execute("INSERT INTO messages(mod_id,item_id,role,body) VALUES (?1,?2,?3,?4) ON CONFLICT(mod_id,item_id) DO UPDATE SET body=excluded.body", params![mod_id,message.item_id,message.role,message.body])?;
        transaction.commit()?;
        Ok(message)
    }

    pub fn save_message(&self, mod_id: i64, message: &Message) -> Result<()> {
        self.0.execute("INSERT INTO messages(mod_id,item_id,role,body,model,effort) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(mod_id,item_id) DO UPDATE SET body=excluded.body,model=COALESCE(excluded.model,messages.model),effort=COALESCE(excluded.effort,messages.effort)", params![mod_id,message.item_id,message.role,message.body,message.model,message.effort])?;
        Ok(())
    }

    pub fn workspace_path(&self, mod_id: i64) -> io::Result<std::path::PathBuf> {
        let parent = Path::new(
            self.0
                .path()
                .ok_or_else(|| io::Error::other("Missing state path."))?,
        )
        .parent()
        .unwrap()
        .join("workspaces");
        fs::create_dir_all(&parent)?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        Ok(parent.join(format!("{mod_id}-{stamp}")))
    }

    pub fn project_path(&self, project_id: i64) -> io::Result<std::path::PathBuf> {
        let database = self
            .0
            .path()
            .ok_or_else(|| io::Error::other("Missing state path."))?;
        Ok(Path::new(database)
            .parent()
            .unwrap()
            .join("projects")
            .join(project_id.to_string()))
    }

    pub fn create_execution(&mut self, mod_id: i64, workspace: &Path, plan: &Plan) -> Result<()> {
        let transaction = self.0.transaction()?;
        transaction.execute(
            "INSERT INTO executions(mod_id,workspace,backend) VALUES (?1,?2,'apple-container') ON CONFLICT(mod_id) DO UPDATE SET status='pending',checks='[]',fingerprint=NULL,backend='apple-container'",
            params![mod_id, workspace.to_string_lossy()],
        )?;
        transaction.execute(
            "UPDATE executions SET repair_count=0 WHERE mod_id=?1",
            [mod_id],
        )?;
        for task in &plan.tasks {
            transaction.execute(
                "INSERT INTO task_runs(mod_id,task_id,source) VALUES (?1,?2,?3)",
                params![mod_id, task.id, format!("pending:{mod_id}:{}", task.id)],
            )?;
            let id = transaction.last_insert_rowid();
            transaction.execute(
                "UPDATE task_runs SET source=?2 WHERE id=?1",
                params![id, task_source(id, 1)],
            )?;
        }
        transaction.execute(
            "UPDATE workers SET thread_id=NULL,pending=NULL WHERE mod_id=?1 AND role='executor'",
            [mod_id],
        )?;
        transaction.commit()
    }

    pub fn install_update(
        &mut self,
        mod_id: i64,
        root: &Path,
        update: &crate::mod_sync::Update,
    ) -> Result<()> {
        let source = format!("upstream:{}", update.target);
        let transaction = self.0.transaction()?;
        let current: Option<String> = transaction
            .query_row("SELECT source FROM plans WHERE mod_id=?1", [mod_id], |r| {
                r.get(0)
            })
            .optional()?;
        if current.as_deref() == Some(&source) {
            return transaction.commit();
        }
        transaction.execute("DELETE FROM network_requests WHERE mod_id=?1", [mod_id])?;
        transaction.execute("DELETE FROM task_runs WHERE mod_id=?1", [mod_id])?;
        transaction.execute("INSERT INTO plans(mod_id,source,status,body) VALUES (?1,?2,'ready',?3) ON CONFLICT(mod_id) DO UPDATE SET source=excluded.source,status='ready',body=excluded.body,model=NULL,effort=NULL,routing=NULL", params![mod_id,source,serde_json::to_string(&update.plan).unwrap()])?;
        transaction.execute("INSERT INTO executions(mod_id,workspace,backend) VALUES (?1,?2,'apple-container') ON CONFLICT(mod_id) DO UPDATE SET status='pending',checks='[]',fingerprint=NULL,backend='apple-container'", params![mod_id,root.to_string_lossy()])?;
        transaction.execute(
            "UPDATE executions SET repair_count=0 WHERE mod_id=?1",
            [mod_id],
        )?;
        for task in &update.plan.tasks {
            transaction.execute(
                "INSERT INTO task_runs(mod_id,task_id,source) VALUES (?1,?2,?3)",
                params![mod_id, task.id, format!("pending:{source}")],
            )?;
            let id = transaction.last_insert_rowid();
            transaction.execute(
                "UPDATE task_runs SET source=?2 WHERE id=?1",
                params![id, task_source(id, 1)],
            )?;
        }
        transaction.execute(
            "UPDATE workers SET thread_id=NULL,pending=NULL WHERE mod_id=?1",
            [mod_id],
        )?;
        transaction.execute("INSERT INTO messages(mod_id,item_id,role,body) VALUES (?1,?2,'harness',?3) ON CONFLICT(mod_id,item_id) DO NOTHING", params![mod_id,source,format!("Updating from {} · {} conflicts · rechecking combined changes", update.branch,update.conflicts.len())])?;
        transaction.commit()
    }

    pub fn execution(&self, mod_id: i64) -> Result<Option<Execution>> {
        let result = self
            .0
            .query_row(
                "SELECT workspace,status,checks,fingerprint,backend FROM executions WHERE mod_id=?1",
                [mod_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((workspace, status, checks, fingerprint, backend)) = result else {
            return Ok(None);
        };
        let mut statement = self.0.prepare("SELECT id,task_id,status,source,turn_id,summary,checks,worker_id,routing,repair FROM task_runs WHERE mod_id=?1 ORDER BY id")?;
        let tasks = statement
            .query_map([mod_id], |row| {
                let checks: String = row.get(6)?;
                Ok(TaskRun {
                    repair: row
                        .get::<_, Option<String>>(9)?
                        .map(|text| {
                            serde_json::from_str(&text).map_err(|_| rusqlite::Error::InvalidQuery)
                        })
                        .transpose()?,
                    selection: row
                        .get::<_, Option<String>>(8)?
                        .map(|text| {
                            serde_json::from_str(&text).map_err(|_| rusqlite::Error::InvalidQuery)
                        })
                        .transpose()?,
                    worker: row.get(7)?,
                    id: row.get(0)?,
                    task_id: row.get(1)?,
                    status: row.get(2)?,
                    source: row.get(3)?,
                    turn: row.get(4)?,
                    summary: row.get(5)?,
                    checks: serde_json::from_str(&checks).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            6,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                })
            })?
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(Execution {
            workspace: workspace.into(),
            backend,
            status,
            tasks,
            checks: serde_json::from_str(&checks).map_err(|_| rusqlite::Error::InvalidQuery)?,
            fingerprint,
        }))
    }

    pub fn task_model(
        &self,
        mod_id: i64,
        worker: i64,
        source: &str,
        selection: &Selection,
    ) -> Result<()> {
        let changed = self.0.execute(
            "UPDATE task_runs SET routing=?4 WHERE mod_id=?1 AND source=?2 AND worker_id=?3 AND status='sending'",
            params![mod_id, source, worker, serde_json::to_string(selection).unwrap()],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok(())
    }

    pub fn task_input(
        &mut self,
        mod_id: i64,
        worker: i64,
        plan: &Plan,
    ) -> Result<Option<Submission>> {
        let execution = self
            .execution(mod_id)?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        if matches!(
            execution.status.as_str(),
            "applied" | "review" | "verifying"
        ) || execution
            .tasks
            .iter()
            .filter(|run| ["sending", "running", "checking"].contains(&run.status.as_str()))
            .count()
            >= 2
            || execution.tasks.iter().any(|run| {
                run.worker == Some(worker)
                    && ["sending", "running", "checking"].contains(&run.status.as_str())
            })
        {
            return Ok(None);
        }
        let provider: String = self.0.query_row(
            "SELECT provider FROM workers WHERE id=?1 AND mod_id=?2",
            params![worker, mod_id],
            |row| row.get(0),
        )?;
        let mut available = plan.clone();
        available.tasks.retain(|task| task.worker == provider);
        if execution
            .tasks
            .iter()
            .any(|run| run.status == "repair_wait")
        {
            available.tasks.retain(|task| {
                execution
                    .tasks
                    .iter()
                    .any(|run| run.task_id == task.id && run.repair.is_some())
            });
        }
        let Some(task) = execution.next_task(&available, worker) else {
            return Ok(None);
        };
        let input = self.submission(mod_id, &task.source)?;
        let transaction = self.0.transaction()?;
        transaction.execute(
            "UPDATE task_runs SET status='sending',worker_id=?2 WHERE id=?1 AND status='pending'",
            params![task.id, worker],
        )?;
        transaction.execute(
            "UPDATE workers SET pending=?2 WHERE id=?1",
            params![worker, input.source],
        )?;
        transaction.execute(
            "UPDATE executions SET status='running' WHERE mod_id=?1",
            [mod_id],
        )?;
        transaction.commit()?;
        Ok(Some(input))
    }

    pub fn task_status(
        &self,
        mod_id: i64,
        source: &str,
        status: &str,
        turn: Option<&str>,
    ) -> Result<()> {
        let changed = self.0.execute("UPDATE task_runs SET status=?3,turn_id=COALESCE(?4,turn_id) WHERE mod_id=?1 AND source=?2", params![mod_id,source,status,turn])?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok(())
    }

    pub fn finish_task(
        &mut self,
        mod_id: i64,
        source: &str,
        status: &str,
        summary: &str,
        checks: &[crate::execution::CheckResult],
    ) -> Result<()> {
        let transaction = self.0.transaction()?;
        let changed = transaction.execute(
            "UPDATE task_runs SET status=?3,summary=?4,checks=?5 WHERE mod_id=?1 AND source=?2",
            params![
                mod_id,
                source,
                status,
                summary,
                serde_json::to_string(checks).unwrap()
            ],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        transaction.execute("UPDATE executions SET status=CASE WHEN EXISTS(SELECT 1 FROM task_runs WHERE mod_id=?1 AND status='repair_wait') THEN 'repairing' WHEN EXISTS(SELECT 1 FROM task_runs WHERE mod_id=?1 AND status IN ('blocked','paused','waiting')) THEN 'blocked' ELSE 'running' END WHERE mod_id=?1", [mod_id])?;
        transaction.commit()
    }

    pub fn retry_tasks(&mut self, mod_id: i64) -> Result<()> {
        let execution = self
            .execution(mod_id)?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        let transaction = self.0.transaction()?;
        transaction.execute(
            "UPDATE task_runs SET status='repair_wait' WHERE mod_id=?1 AND status='repair_paused'",
            [mod_id],
        )?;
        for task in execution
            .tasks
            .iter()
            .filter(|task| ["blocked", "paused", "waiting"].contains(&task.status.as_str()))
        {
            let attempt: i64 = transaction.query_row(
                "SELECT attempt FROM task_runs WHERE id=?1",
                [task.id],
                |row| row.get(0),
            )?;
            transaction.execute("UPDATE task_runs SET status='pending',attempt=?2,source=?3,turn_id=NULL,summary='',checks='[]' WHERE id=?1", params![task.id,attempt+1,task_source(task.id,attempt+1)])?;
            transaction.execute(
                "UPDATE workers SET pending=NULL WHERE mod_id=?1 AND pending=?2",
                params![mod_id, task.source],
            )?;
        }
        transaction.commit()
    }

    pub fn move_to_vm(&mut self, mod_id: i64) -> Result<()> {
        let transaction = self.0.transaction()?;
        let changed = transaction.execute("UPDATE executions SET backend='apple-container',status='ready',checks='[]',fingerprint=NULL WHERE mod_id=?1 AND backend='local' AND status!='applied'", [mod_id])?;
        if changed == 0 {
            return transaction.commit();
        }
        let mut statement =
            transaction.prepare("SELECT id,attempt FROM task_runs WHERE mod_id=?1")?;
        let tasks = statement
            .query_map([mod_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>>>()?;
        drop(statement);
        for (id, attempt) in tasks {
            transaction.execute("UPDATE task_runs SET status='pending',attempt=?2,source=?3,turn_id=NULL,summary='',checks='[]' WHERE id=?1", params![id,attempt+1,task_source(id,attempt+1)])?;
        }
        transaction.execute(
            "UPDATE workers SET thread_id=NULL,pending=NULL WHERE mod_id=?1 AND role='executor'",
            [mod_id],
        )?;
        transaction.commit()
    }

    pub fn execution_status(&self, mod_id: i64, status: &str) -> Result<()> {
        self.0.execute(
            "UPDATE executions SET status=?2 WHERE mod_id=?1",
            params![mod_id, status],
        )?;
        Ok(())
    }

    pub fn execution_checks(
        &self,
        mod_id: i64,
        status: &str,
        checks: &[crate::execution::CheckResult],
        fingerprint: Option<&str>,
    ) -> Result<()> {
        self.0.execute(
            "UPDATE executions SET status=?2,checks=?3,fingerprint=?4 WHERE mod_id=?1",
            params![
                mod_id,
                status,
                serde_json::to_string(checks).unwrap(),
                fingerprint
            ],
        )?;
        Ok(())
    }
}

pub(crate) fn task_source(id: i64, attempt: i64) -> String {
    format!(
        "00000004-{:04x}-4{:03x}-8000-{id:012x}",
        attempt & 0xffff,
        (attempt >> 16) & 0xfff
    )
}

pub fn source_id(id: i64, steering: bool) -> String {
    format!(
        "{:08x}-0000-4000-8000-{id:012x}",
        if steering { 2 } else { 1 }
    )
}

fn plan_source(mod_id: i64, attempt: i64) -> String {
    format!(
        "00000003-{:04x}-4{:03x}-8000-{mod_id:012x}",
        attempt & 0xffff,
        (attempt >> 16) & 0xfff
    )
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    pub struct TestData(pub PathBuf);

    impl TestData {
        pub fn new() -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            Self(
                std::env::temp_dir()
                    .join(format!("sprowt-test-{}-{stamp}-{id}", std::process::id())),
            )
        }

        pub fn store(&self) -> super::Store {
            super::Store::open(&self.0.join("state.db")).unwrap()
        }
    }

    impl Drop for TestData {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{test_support::TestData, *};
    use serde_json::json;

    #[test]
    fn two_workers_claim_independent_tasks_and_keep_retry_ownership() {
        let data = TestData::new();
        let mut store = data.store();
        let project = store.load_project(Path::new("/parallel")).unwrap();
        let m = store.create_mod(project.id, "Build two parts").unwrap();
        let plan = Plan::parse(&serde_json::json!({"summary":"Two parts", "tasks":[
            {"id":"a","title":"A","outcome":"A ready","files":["a.txt"],"depends_on":[],"coordination":[{"task":"b","topic":"Shared behavior and readiness"}],"worker":"codex","checks":["A works"]},
            {"id":"b","title":"B","outcome":"B ready","files":["b.txt"],"depends_on":[],"worker":"codex","checks":["B works"]},
            {"id":"c","title":"Combine","outcome":"Combined","files":["c.txt"],"depends_on":["a","b"],"worker":"codex","checks":["Both work"]}
        ]}).to_string()).unwrap();
        store
            .save_plan(m.id, &m.planning.as_ref().unwrap().source, &plan)
            .unwrap();
        store
            .create_execution(m.id, &data.0.join("work"), &plan)
            .unwrap();
        let a = store.worker_at(m.id, Role::Executor, 0).unwrap().id;
        let b = store.worker_at(m.id, Role::Executor, 1).unwrap().id;
        let first = store.task_input(m.id, a, &plan).unwrap().unwrap();
        assert!(store.task_input(m.id, a, &plan).unwrap().is_none());
        let second = store.task_input(m.id, b, &plan).unwrap().unwrap();
        assert_ne!(first.source, second.source);
        assert!(store.task_input(m.id, b, &plan).unwrap().is_none());
        store
            .finish_task(m.id, &first.source, "blocked", "A stopped", &[])
            .unwrap();
        store
            .finish_task(m.id, &second.source, "done", "B ready", &[])
            .unwrap();
        assert!(store.task_input(m.id, b, &plan).unwrap().is_none());
        assert_eq!(store.execution(m.id).unwrap().unwrap().status, "blocked");
        drop(store);
        let mut store = data.store();
        store.retry_tasks(m.id).unwrap();
        assert!(store.task_input(m.id, b, &plan).unwrap().is_none());
        let retry = store.task_input(m.id, a, &plan).unwrap().unwrap();
        assert_ne!(retry.source, first.source);
        assert_eq!(
            crate::task_worktree::task_id(&retry.source).unwrap(),
            crate::task_worktree::task_id(&first.source).unwrap()
        );
        store
            .finish_task(m.id, &retry.source, "done", "A ready", &[])
            .unwrap();
        let third = store.task_input(m.id, b, &plan).unwrap().unwrap();
        assert!(third.texts[0].contains("Combined"));
        assert_eq!(
            store
                .execution(m.id)
                .unwrap()
                .unwrap()
                .tasks
                .iter()
                .filter(|r| r.status == "done")
                .count(),
            2
        );
    }

    #[test]
    fn broadcast_steering_waits_for_each_worker_acknowledgment() {
        let data = TestData::new();
        let mut store = data.store();
        let project = store.load_project(Path::new("/steering-two")).unwrap();
        let m = store.create_mod(project.id, "Two workers").unwrap();
        let a = store.worker_at(m.id, Role::Executor, 0).unwrap().id;
        let b = store.worker_at(m.id, Role::Executor, 1).unwrap().id;
        let message = store.enqueue(m.id, "Keep it small").unwrap();
        store.steer_to(m.id, &[message.id], &[a, b]).unwrap();
        let input = store.next_steering(m.id, a).unwrap().unwrap();
        for worker in [a, b] {
            store.pending(worker, Some(&input.source)).unwrap();
        }
        store.acknowledge(a, &input).unwrap();
        assert!(store.next_steering(m.id, a).unwrap().is_none());
        assert_eq!(store.steering(m.id).unwrap(), ["Keep it small"]);
        drop(store);
        let mut store = data.store();
        assert_eq!(
            store.next_steering(m.id, b).unwrap().unwrap().source,
            input.source
        );
        store.acknowledge(b, &input).unwrap();
        assert!(store.steering(m.id).unwrap().is_empty());
        let state = store.load_project(Path::new("/steering-two")).unwrap();
        assert_eq!(
            state.mods[0]
                .messages
                .iter()
                .filter(|m| m.item_id.as_deref() == Some(&input.source))
                .count(),
            1
        );
        let message = store.enqueue(m.id, "Only B").unwrap();
        store.steer_to(m.id, &[message.id], &[b]).unwrap();
        assert!(store.next_steering(m.id, a).unwrap().is_none());
        assert!(store.next_steering(m.id, b).unwrap().is_some());
        store.delete_mod(project.id, m.id).unwrap();
    }

    #[test]
    fn rejected_broadcast_returns_to_queue_once_without_claiming_delivery() {
        let data = TestData::new();
        let mut store = data.store();
        let project = store.load_project(Path::new("/rejected-steering")).unwrap();
        let m = store.create_mod(project.id, "Two workers").unwrap();
        let a = store.worker_at(m.id, Role::Executor, 0).unwrap().id;
        let b = store.worker_at(m.id, Role::Executor, 1).unwrap().id;
        let message = store.enqueue(m.id, "Change direction").unwrap();
        store.steer_to(m.id, &[message.id], &[a, b]).unwrap();
        let input = store.next_steering(m.id, a).unwrap().unwrap();
        assert_eq!(
            store.reject_steering(m.id, a, &input.source).unwrap().len(),
            1
        );
        assert!(store.next_steering(m.id, a).unwrap().is_none());
        assert!(store.next_steering(m.id, b).unwrap().is_some());
        assert!(
            store
                .reject_steering(m.id, b, &input.source)
                .unwrap()
                .is_empty()
        );
        let state = store.load_project(Path::new("/rejected-steering")).unwrap();
        assert_eq!(state.mods[0].queue.len(), 1);
        assert!(state.mods[0].steering.is_empty());
        assert!(
            !state.mods[0]
                .messages
                .iter()
                .any(|m| m.item_id.as_deref() == Some(&input.source))
        );
    }

    #[test]
    fn closed_worktree_retention_is_scoped_and_reopening_resets_its_clock() {
        let data = TestData::new();
        let mut store = data.store();
        let first = store.load_project(Path::new("/first")).unwrap();
        let second = store.load_project(Path::new("/second")).unwrap();
        let a = store.create_mod(first.id, "A").unwrap();
        let b = store.create_mod(second.id, "B").unwrap();
        for (project, code_mod) in [(first.id, a.id), (second.id, b.id)] {
            store
                .save_git_root(code_mod, Path::new("/saved/worktree"))
                .unwrap();
            store.close_mod(project, code_mod).unwrap();
            store
                .0
                .execute(
                    "UPDATE code_mods SET closed_at=unixepoch()-31*86400 WHERE id=?1",
                    [code_mod],
                )
                .unwrap();
        }
        assert!(store.expired_worktrees(first.id, 0).unwrap().is_empty());
        assert_eq!(
            store.expired_worktrees(first.id, 30).unwrap(),
            [(a.id, std::path::PathBuf::from("/saved/worktree"))]
        );
        assert!(store.expired_worktrees(first.id, 32).unwrap().is_empty());
        store.reopen_mod(a.id).unwrap();
        assert!(store.expired_worktrees(first.id, 30).unwrap().is_empty());
        store.close_mod(first.id, a.id).unwrap();
        assert!(store.expired_worktrees(first.id, 30).unwrap().is_empty());
        assert_eq!(store.expired_worktrees(second.id, 30).unwrap()[0].0, b.id);
    }

    #[test]
    fn changed_queue_request_rolls_back_the_whole_edit_round() {
        let data = TestData::new();
        let mut store = data.store();
        let project = store.load_project(Path::new("/edit")).unwrap();
        let code_mod = store.create_mod(project.id, "Add greeting").unwrap();
        let request = store.enqueue(code_mod.id, "First request").unwrap();
        store.save_draft(code_mod.id, "keep draft").unwrap();
        store
            .edit_queued(code_mod.id, request.id, "Edited request")
            .unwrap();
        assert!(
            store
                .follow_up(code_mod.id, "First request", Some(request.id))
                .is_err()
        );
        let state = data.store().load_project(Path::new("/edit")).unwrap();
        let saved = &state.mods[0];
        assert_eq!(saved.description, code_mod.description);
        assert_eq!(
            saved.planning.as_ref().unwrap().source,
            code_mod.planning.as_ref().unwrap().source
        );
        assert_eq!(saved.messages.len(), 1);
        assert_eq!(saved.queue[0].body, "Edited request");
        assert_eq!(saved.draft, "keep draft");
    }

    #[test]
    fn moving_legacy_execution_to_linux_keeps_work_and_replaces_old_verification() {
        let data = TestData::new();
        let mut store = data.store();
        let project = store.load_project(Path::new("/migration-test")).unwrap();
        let code_mod = store.create_mod(project.id, "Add greeting").unwrap();
        let plan = Plan::parse(r#"{"summary":"Greeting","tasks":[{"id":"one","title":"Greeting","outcome":"Print hello","files":["hello.py"],"depends_on":[],"worker":"codex","checks":["prints hello"]}]}"#).unwrap();
        let root = data.0.join("workspace");
        fs::create_dir_all(root.join("work")).unwrap();
        fs::write(root.join("work/hello.py"), "preserved").unwrap();
        store.create_execution(code_mod.id, &root, &plan).unwrap();
        let run = store.execution(code_mod.id).unwrap().unwrap().tasks[0].clone();
        let worker = store.worker(code_mod.id).unwrap();
        store.save_thread(worker.id, "host-thread").unwrap();
        store
            .finish_task(code_mod.id, &run.source, "done", "Done", &[])
            .unwrap();
        store
            .execution_checks(code_mod.id, "review", &[], Some("old-host-verification"))
            .unwrap();
        store
            .0
            .execute(
                "UPDATE executions SET backend='local' WHERE mod_id=?1",
                [code_mod.id],
            )
            .unwrap();
        store.enqueue(code_mod.id, "follow up").unwrap();
        store.save_draft(code_mod.id, "draft").unwrap();
        store.move_to_vm(code_mod.id).unwrap();
        let execution = data.store().execution(code_mod.id).unwrap().unwrap();
        assert_eq!(execution.backend, "apple-container");
        assert_eq!(execution.tasks[0].status, "pending");
        assert_ne!(execution.tasks[0].source, run.source);
        assert!(execution.fingerprint.is_none());
        assert!(store.worker(code_mod.id).unwrap().thread_id.is_none());
        assert_eq!(
            fs::read_to_string(root.join("work/hello.py")).unwrap(),
            "preserved"
        );
        let state = store.load_project(Path::new("/migration-test")).unwrap();
        assert_eq!(state.mods[0].draft, "draft");
        assert_eq!(state.mods[0].queue[0].body, "follow up");
        store.move_to_vm(code_mod.id).unwrap();
        assert_eq!(
            store.execution(code_mod.id).unwrap().unwrap().tasks[0].source,
            execution.tasks[0].source
        );
    }

    #[test]
    fn execution_orders_dependencies_and_retries_only_unfinished_tasks_after_reopening() {
        let data = TestData::new();
        let mut store = data.store();
        let project = store.load_project(Path::new("/execution-test")).unwrap();
        let code_mod = store.create_mod(project.id, "Build greeting").unwrap();
        let source = code_mod.planning.as_ref().unwrap().source.clone();
        let plan = Plan::parse(r#"{"summary":"Greeting","tasks":[{"id":"first","title":"Greeting","outcome":"Print hello","files":["hello.py"],"depends_on":[],"worker":"codex","checks":["prints hello"]},{"id":"second","title":"Verify","outcome":"Check greeting","files":["test.py"],"depends_on":["first"],"worker":"codex","checks":["tests pass"]}]}"#).unwrap();
        store.save_plan(code_mod.id, &source, &plan).unwrap();
        let worker = store.worker(code_mod.id).unwrap();
        store.save_thread(worker.id, "old-readonly-thread").unwrap();
        store
            .create_execution(code_mod.id, Path::new("/private/mod-one"), &plan)
            .unwrap();
        assert!(store.worker(code_mod.id).unwrap().thread_id.is_none());
        let first = store
            .task_input(code_mod.id, worker.id, &plan)
            .unwrap()
            .unwrap();
        assert!(
            store
                .task_input(code_mod.id, worker.id, &plan)
                .unwrap()
                .is_none()
        );
        store.acknowledge(worker.id, &first).unwrap();
        store
            .task_status(code_mod.id, &first.source, "running", Some("turn-one"))
            .unwrap();
        store
            .finish_task(code_mod.id, &first.source, "done", "Done", &[])
            .unwrap();
        let second = store
            .task_input(code_mod.id, worker.id, &plan)
            .unwrap()
            .unwrap();
        assert_ne!(first.source, second.source);
        store
            .finish_task(code_mod.id, &second.source, "paused", "Interrupted", &[])
            .unwrap();
        drop(store);
        let mut store = data.store();
        store.retry_tasks(code_mod.id).unwrap();
        let execution = store.execution(code_mod.id).unwrap().unwrap();
        assert_eq!(execution.tasks[0].status, "done");
        assert_eq!(execution.tasks[0].turn.as_deref(), Some("turn-one"));
        assert_ne!(execution.tasks[1].source, second.source);
        assert_eq!(
            crate::task_worktree::task_id(&execution.tasks[1].source).unwrap(),
            crate::task_worktree::task_id(&second.source).unwrap()
        );
        assert_eq!(execution.tasks[1].status, "pending");
        assert!(store.worker(code_mod.id).unwrap().pending.is_none());
        let next = store
            .task_input(code_mod.id, worker.id, &plan)
            .unwrap()
            .unwrap();
        store.acknowledge(worker.id, &next).unwrap();
        store
            .finish_task(code_mod.id, &next.source, "done", "Done", &[])
            .unwrap();
        assert_eq!(
            store.execution(code_mod.id).unwrap().unwrap().status,
            "running"
        );
        store
            .execution_checks(
                code_mod.id,
                "review",
                &[crate::execution::CheckResult {
                    task: None,
                    check: "tests pass".into(),
                    command: vec!["/usr/bin/true".into()],
                    exit_code: Some(0),
                    output: String::new(),
                }],
                Some("verified-snapshot"),
            )
            .unwrap();
        assert_eq!(
            store
                .load_project(Path::new("/execution-test"))
                .unwrap()
                .mods[0]
                .execution
                .as_ref()
                .unwrap()
                .checks
                .len(),
            1
        );
        assert_eq!(
            store
                .load_project(Path::new("/execution-test"))
                .unwrap()
                .mods[0]
                .messages
                .len(),
            2
        );
        store.delete_mod(project.id, code_mod.id).unwrap();
        assert!(store.execution(code_mod.id).unwrap().is_none());
        assert_eq!(
            store
                .0
                .query_row("SELECT COUNT(*) FROM task_runs", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn planning_persists_roles_and_rolls_back_an_incomplete_save() {
        let data = TestData::new();
        let project_path = Path::new("/planned-project");
        let mut store = data.store();
        let project = store.load_project(project_path).unwrap();
        let code_mod = store
            .create_mod(project.id, "Add greeting\nKeep output concise")
            .unwrap();
        assert_eq!(code_mod.name, "Add greeting");
        let planner = store.worker_for(code_mod.id, Role::Planner).unwrap();
        let executor = store.worker(code_mod.id).unwrap();
        assert_ne!(planner.id, executor.id);
        store.save_thread(planner.id, "planner-thread").unwrap();
        store.save_thread(executor.id, "executor-thread").unwrap();
        let input = store.planner_input(code_mod.id).unwrap().unwrap();
        store.pending(planner.id, Some(&input.source)).unwrap();
        store.acknowledge(planner.id, &input).unwrap();
        store
            .planning_model(code_mod.id, &Selection::planner())
            .unwrap();
        let plan = Plan::parse(
            &json!({"summary":"Greeting","tasks":[{
            "id":"1","title":"Greeting","outcome":"Show greeting","files":["src/main.rs"],
            "depends_on":[],"worker":"codex","checks":["Run greeting test"]}]})
            .to_string(),
        )
        .unwrap();
        store.0.execute_batch("CREATE TRIGGER reject_plan BEFORE INSERT ON messages WHEN NEW.role='planner' BEGIN SELECT RAISE(ABORT,'save failure'); END;").unwrap();
        assert!(store.save_plan(code_mod.id, &input.source, &plan).is_err());
        let planning = store.planning(code_mod.id).unwrap().unwrap();
        assert_eq!(planning.status, "running");
        assert!(planning.plan.is_none());
        store.0.execute_batch("DROP TRIGGER reject_plan").unwrap();
        store.save_plan(code_mod.id, &input.source, &plan).unwrap();
        assert!(
            store
                .save_plan(code_mod.id, "wrong request", &plan)
                .is_err()
        );
        drop(store);
        let mut store = data.store();
        let state = store.load_project(project_path).unwrap();
        assert_eq!(
            state.mods[0].description,
            "Add greeting\nKeep output concise"
        );
        assert_eq!(state.mods[0].messages.len(), 2);
        assert_eq!(state.mods[0].planning.as_ref().unwrap().status, "ready");
        assert_eq!(
            state.mods[0].planning.as_ref().unwrap().model.as_deref(),
            Some("gpt-6-astra")
        );
        assert_eq!(
            store
                .worker_for(code_mod.id, Role::Planner)
                .unwrap()
                .thread_id
                .as_deref(),
            Some("planner-thread")
        );
        assert_eq!(
            store.worker(code_mod.id).unwrap().thread_id.as_deref(),
            Some("executor-thread")
        );
        store.delete_mod(project.id, code_mod.id).unwrap();
        assert!(store.planning(code_mod.id).unwrap().is_none());
        assert_eq!(
            store
                .0
                .query_row("SELECT COUNT(*) FROM workers", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    fn history(store: &Store, mod_id: i64, body: &str) {
        store
            .0
            .execute(
                "INSERT INTO messages (mod_id, body) VALUES (?1, ?2)",
                params![mod_id, body],
            )
            .unwrap();
    }

    #[test]
    fn projects_and_mods_survive_reopening_without_mixing_messages() {
        let data = TestData::new();
        let mut store = data.store();
        let first = store.load_project(Path::new("/project-a")).unwrap();
        let second = store.load_project(Path::new("/project-b")).unwrap();
        let a = store.legacy_mod(first.id, "feature A").unwrap();
        let b = store.legacy_mod(first.id, "feature B").unwrap();
        let c = store.legacy_mod(second.id, "feature C").unwrap();
        history(&store, a.id, "first\nmessage");
        history(&store, b.id, "different message");
        history(&store, c.id, "another project");
        store
            .save_draft(a.id, "draft with 'quotes' and 界")
            .unwrap();
        store.select_mod(first.id, a.id).unwrap();
        assert!(store.select_mod(first.id, c.id).is_err());
        let other_writer = data.store();
        history(&other_writer, a.id, "second message");
        drop(other_writer);
        drop(store);

        let reopened = data.store();
        let first = reopened.load_project(Path::new("/project-a")).unwrap();
        assert_eq!(first.active_mod_id, Some(a.id));
        assert_eq!(first.mods.len(), 2);
        assert_eq!(
            first.mods[0]
                .messages
                .iter()
                .map(|message| message.body.as_str())
                .collect::<Vec<_>>(),
            ["first\nmessage", "second message"]
        );
        assert_eq!(
            first.mods[1]
                .messages
                .iter()
                .map(|message| message.body.as_str())
                .collect::<Vec<_>>(),
            ["different message"]
        );
        let second = reopened.load_project(Path::new("/project-b")).unwrap();
        assert_eq!(second.mods.len(), 1);
        assert_eq!(
            second.mods[0]
                .messages
                .iter()
                .map(|message| message.body.as_str())
                .collect::<Vec<_>>(),
            ["another project"]
        );
    }

    #[test]
    fn failed_submission_rolls_back_message_and_keeps_draft() {
        let data = TestData::new();
        let mut store = data.store();
        let project = store.load_project(Path::new("/project")).unwrap();
        let code_mod = store.legacy_mod(project.id, "feature").unwrap();
        store.save_draft(code_mod.id, "valuable draft").unwrap();
        store
            .0
            .execute_batch(
                "CREATE TRIGGER reject_clear BEFORE UPDATE OF draft ON code_mods
            BEGIN SELECT RAISE(ABORT, 'simulated write failure'); END;",
            )
            .unwrap();
        assert!(store.enqueue(code_mod.id, "valuable draft").is_err());
        let state = store.load_project(Path::new("/project")).unwrap();
        assert!(state.mods[0].messages.is_empty());
        assert!(state.mods[0].queue.is_empty());
        assert_eq!(state.mods[0].draft, "valuable draft");
    }

    #[test]
    fn queue_edits_order_and_removals_survive_reopening_without_crossing_mods() {
        let data = TestData::new();
        let mut store = data.store();
        let project = store.load_project(Path::new("/project")).unwrap();
        let a = store.legacy_mod(project.id, "feature A").unwrap();
        let b = store.legacy_mod(project.id, "feature B").unwrap();
        history(&store, a.id, "previous conversation");
        let first = store.enqueue(a.id, "first").unwrap();
        let second = store.enqueue(a.id, "second").unwrap();
        let third = store.enqueue(a.id, "third").unwrap();
        let other = store.enqueue(b.id, "another mod").unwrap();
        store.save_draft(a.id, "keep this draft").unwrap();
        store
            .edit_queued(a.id, second.id, "edited\n界 'quotes'")
            .unwrap();
        store.swap_queued(a.id, first.id, third.id).unwrap();
        assert!(store.edit_queued(a.id, other.id, "wrong mod").is_err());
        assert!(store.remove_queued(a.id, other.id).is_err());
        assert!(store.swap_queued(a.id, first.id, other.id).is_err());
        drop(store);

        let mut store = data.store();
        let state = store.load_project(Path::new("/project")).unwrap();
        let queue = &state.mods[0].queue;
        assert_eq!(
            queue
                .iter()
                .map(|message| message.body.as_str())
                .collect::<Vec<_>>(),
            ["third", "edited\n界 'quotes'", "first"]
        );
        assert_eq!(
            state.mods[0]
                .messages
                .iter()
                .map(|message| message.body.as_str())
                .collect::<Vec<_>>(),
            ["previous conversation"]
        );
        assert_eq!(state.mods[0].draft, "keep this draft");
        assert_eq!(state.mods[1].queue[0].body, "another mod");
        store.remove_queued(a.id, second.id).unwrap();
        data.store().enqueue(a.id, "fourth").unwrap();
        store.0.execute_batch("CREATE TRIGGER reject_move BEFORE UPDATE OF position ON queued_messages BEGIN SELECT RAISE(ABORT, 'simulated write failure'); END;").unwrap();
        assert!(store.swap_queued(a.id, first.id, third.id).is_err());
        drop(store);
        let state = data.store().load_project(Path::new("/project")).unwrap();
        assert_eq!(
            state.mods[0]
                .queue
                .iter()
                .map(|message| message.body.as_str())
                .collect::<Vec<_>>(),
            ["third", "first", "fourth"]
        );
    }

    #[test]
    fn steering_preserves_queue_order_and_mod_boundaries_after_restart() {
        let data = TestData::new();
        let mut store = data.store();
        let project = store.load_project(Path::new("/project")).unwrap();
        let a = store.legacy_mod(project.id, "A").unwrap();
        let b = store.legacy_mod(project.id, "B").unwrap();
        let first = store.enqueue(a.id, "first").unwrap();
        let second = store.enqueue(a.id, "second").unwrap();
        let third = store.enqueue(a.id, "third\n界").unwrap();
        let other = store.enqueue(b.id, "another mod").unwrap();
        store.save_draft(a.id, "keep draft").unwrap();
        history(&store, a.id, "existing history");
        store.swap_queued(a.id, first.id, third.id).unwrap();
        assert!(store.request_steering(a.id, &[]).is_err());
        assert!(store.request_steering(a.id, &[first.id, other.id]).is_err());
        assert!(store.request_steering(a.id, &[first.id, first.id]).is_err());
        assert_eq!(
            store.request_steering(a.id, &[first.id, third.id]).unwrap(),
            ["third\n界", "first"]
        );
        assert_eq!(
            store.request_steering(a.id, &[second.id]).unwrap(),
            ["second"]
        );
        let requests: i64 = store
            .0
            .query_row(
                "SELECT COUNT(*) FROM steering_requests WHERE mod_id = ?1",
                [a.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(requests, 2);
        drop(store);
        let state = data.store().load_project(Path::new("/project")).unwrap();
        assert!(state.mods[0].queue.is_empty());
        assert_eq!(state.mods[0].steering, ["third\n界", "first", "second"]);
        assert_eq!(state.mods[0].draft, "keep draft");
        assert_eq!(
            state.mods[0]
                .messages
                .iter()
                .map(|message| message.body.as_str())
                .collect::<Vec<_>>(),
            ["existing history"]
        );
        assert_eq!(state.mods[1].queue[0].body, "another mod");
        assert!(state.mods[1].steering.is_empty());
    }

    #[test]
    fn failed_steering_rolls_back_the_entire_selection() {
        let data = TestData::new();
        let mut store = data.store();
        let project = store.load_project(Path::new("/project")).unwrap();
        let code_mod = store.legacy_mod(project.id, "feature").unwrap();
        let first = store.enqueue(code_mod.id, "first").unwrap();
        let second = store.enqueue(code_mod.id, "second").unwrap();
        store.0.execute_batch("CREATE TRIGGER reject_steering BEFORE INSERT ON steering_messages WHEN NEW.position = 1 BEGIN SELECT RAISE(ABORT, 'simulated write failure'); END;").unwrap();
        assert!(
            store
                .request_steering(code_mod.id, &[first.id, second.id])
                .is_err()
        );
        let state = data.store().load_project(Path::new("/project")).unwrap();
        assert_eq!(
            state.mods[0]
                .queue
                .iter()
                .map(|message| message.body.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert!(state.mods[0].steering.is_empty());
        let requests: i64 = store
            .0
            .query_row("SELECT COUNT(*) FROM steering_requests", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(requests, 0);
    }

    #[test]
    fn adding_steering_storage_keeps_existing_queue_history_and_draft() {
        let data = TestData::new();
        let mut store = data.store();
        let project = store.load_project(Path::new("/project")).unwrap();
        let code_mod = store.legacy_mod(project.id, "existing mod").unwrap();
        store.enqueue(code_mod.id, "existing instruction").unwrap();
        store.save_draft(code_mod.id, "existing draft").unwrap();
        history(&store, code_mod.id, "existing history");
        store
            .0
            .execute_batch("DROP TABLE steering_messages; DROP TABLE steering_requests;")
            .unwrap();
        drop(store);
        let state = data.store().load_project(Path::new("/project")).unwrap();
        assert_eq!(state.mods[0].queue[0].body, "existing instruction");
        assert_eq!(
            state.mods[0]
                .messages
                .iter()
                .map(|message| message.body.as_str())
                .collect::<Vec<_>>(),
            ["existing history"]
        );
        assert_eq!(state.mods[0].draft, "existing draft");
        assert!(state.mods[0].steering.is_empty());
    }

    #[test]
    fn adding_queue_storage_keeps_legacy_history_and_drafts() {
        let data = TestData::new();
        let mut store = data.store();
        let project = store.load_project(Path::new("/project")).unwrap();
        let code_mod = store.legacy_mod(project.id, "existing feature").unwrap();
        history(&store, code_mod.id, "existing message");
        store.save_draft(code_mod.id, "existing draft").unwrap();
        store
            .0
            .execute_batch("DROP TABLE queued_messages;")
            .unwrap();
        drop(store);
        let state = data.store().load_project(Path::new("/project")).unwrap();
        assert_eq!(
            state.mods[0]
                .messages
                .iter()
                .map(|message| message.body.as_str())
                .collect::<Vec<_>>(),
            ["existing message"]
        );
        assert_eq!(state.mods[0].draft, "existing draft");
        assert!(state.mods[0].queue.is_empty());
    }
    #[test]
    fn accepted_input_is_atomic_and_workers_have_independent_identities() {
        let data = TestData::new();
        let project = Path::new("/worker-project");
        let mut store = data.store();
        let state = store.load_project(project).unwrap();
        let code_mod = store.legacy_mod(state.id, "feature").unwrap();
        let worker = store.worker(code_mod.id).unwrap();
        store
            .0
            .execute("INSERT INTO workers(mod_id) VALUES (?1)", [code_mod.id])
            .unwrap();
        assert_eq!(
            store
                .0
                .query_row(
                    "SELECT COUNT(*) FROM workers WHERE mod_id=?1",
                    [code_mod.id],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            2
        );
        let queued = store.enqueue(code_mod.id, "inspect the code").unwrap();
        let input = store.next_input(code_mod.id, false).unwrap().unwrap();
        store.pending(worker.id, Some(&input.source)).unwrap();
        store.0.execute_batch("CREATE TRIGGER reject_ack BEFORE DELETE ON queued_messages BEGIN SELECT RAISE(ABORT, 'simulated failure'); END;").unwrap();
        assert!(store.acknowledge(worker.id, &input).is_err());
        assert_eq!(
            store.worker(code_mod.id).unwrap().pending.as_deref(),
            Some(input.source.as_str())
        );
        let state = store.load_project(project).unwrap();
        assert_eq!(state.mods[0].queue[0].id, queued.id);
        assert!(state.mods[0].messages.is_empty());
        store.0.execute_batch("DROP TRIGGER reject_ack;").unwrap();
        store.acknowledge(worker.id, &input).unwrap();
        store.save_thread(worker.id, "codex-conversation").unwrap();
        drop(store);
        let store = data.store();
        let state = store.load_project(project).unwrap();
        assert!(state.mods[0].queue.is_empty());
        assert_eq!(state.mods[0].messages[0].role, "user");
        assert_eq!(
            store.worker(code_mod.id).unwrap().thread_id.as_deref(),
            Some("codex-conversation")
        );
        assert!(store.worker(code_mod.id).unwrap().pending.is_none());
    }

    #[test]
    fn steering_acknowledgment_is_scoped_and_message_updates_do_not_duplicate() {
        let data = TestData::new();
        let mut store = data.store();
        let project = Path::new("/steer-project");
        let state = store.load_project(project).unwrap();
        let a = store.legacy_mod(state.id, "a").unwrap();
        let b = store.legacy_mod(state.id, "b").unwrap();
        let worker_a = store.worker(a.id).unwrap();
        let worker_b = store.worker(b.id).unwrap();
        let queued = store.enqueue(a.id, "make it short").unwrap();
        store.request_steering(a.id, &[queued.id]).unwrap();
        let input = store.next_input(a.id, true).unwrap().unwrap();
        store.pending(worker_b.id, Some(&input.source)).unwrap();
        assert!(store.acknowledge(worker_b.id, &input).is_err());
        store.pending(worker_a.id, Some(&input.source)).unwrap();
        store.acknowledge(worker_a.id, &input).unwrap();
        assert!(store.next_input(a.id, true).unwrap().is_none());
        for body in ["partial", "complete reply"] {
            store
                .save_message(
                    a.id,
                    &Message {
                        item_id: Some("agent-1".into()),
                        role: "codex".into(),
                        body: body.into(),
                        model: Some("executor-model".into()),
                        effort: Some("high".into()),
                    },
                )
                .unwrap();
        }
        let state = store.load_project(project).unwrap();
        assert_eq!(state.mods[0].messages.len(), 2);
        assert_eq!(state.mods[0].messages[1].body, "complete reply");
        assert_eq!(state.mods[0].messages[1].role, "codex");
        assert_eq!(
            state.mods[0].messages[1].model.as_deref(),
            Some("executor-model")
        );
        assert_eq!(state.mods[0].messages[1].effort.as_deref(), Some("high"));
        assert!(state.mods[1].messages.is_empty());
    }
    #[test]
    fn worker_migration_preserves_legacy_history_queue_and_draft() {
        let data = TestData::new();
        let project = Path::new("/legacy-worker-project");
        let mut store = data.store();
        let state = store.load_project(project).unwrap();
        let code_mod = store.legacy_mod(state.id, "existing feature").unwrap();
        history(&store, code_mod.id, "old conversation");
        store.enqueue(code_mod.id, "existing instruction").unwrap();
        store.save_draft(code_mod.id, "unfinished draft").unwrap();
        store.0.execute_batch("DROP INDEX message_items; ALTER TABLE messages DROP COLUMN item_id; ALTER TABLE messages DROP COLUMN role; ALTER TABLE messages DROP COLUMN effort; DROP TABLE workers;").unwrap();
        drop(store);
        let store = data.store();
        let state = store.load_project(project).unwrap();
        assert_eq!(state.mods[0].messages[0].body, "old conversation");
        assert_eq!(state.mods[0].messages[0].role, "user");
        assert!(state.mods[0].messages[0].item_id.is_none());
        assert!(state.mods[0].messages[0].effort.is_none());
        assert_eq!(state.mods[0].queue[0].body, "existing instruction");
        assert_eq!(state.mods[0].draft, "unfinished draft");
        assert!(store.worker(code_mod.id).unwrap().thread_id.is_none());
    }
    #[test]
    fn deleting_a_mod_removes_its_state_and_preserves_other_projects() {
        let data = TestData::new();
        let mut store = data.store();
        let first = store.load_project(Path::new("/delete-first")).unwrap();
        let second = store.load_project(Path::new("/delete-second")).unwrap();
        let a = store.legacy_mod(first.id, "a").unwrap();
        let b = store.legacy_mod(first.id, "b").unwrap();
        let c = store.legacy_mod(second.id, "c").unwrap();
        for id in [a.id, b.id, c.id] {
            history(&store, id, "saved history");
            let queued = store.enqueue(id, "steering instruction").unwrap();
            store.request_steering(id, &[queued.id]).unwrap();
            store.enqueue(id, "queued instruction").unwrap();
            store.save_draft(id, "saved draft").unwrap();
            let worker = store.worker(id).unwrap();
            store.save_thread(worker.id, "saved conversation").unwrap();
        }
        store
            .0
            .execute("INSERT INTO workers(mod_id) VALUES (?1)", [a.id])
            .unwrap();
        store.select_mod(first.id, a.id).unwrap();
        assert!(store.delete_mod(second.id, a.id).is_err());
        assert_eq!(store.delete_mod(first.id, a.id).unwrap(), Some(b.id));
        for table in [
            "workers",
            "messages",
            "queued_messages",
            "steering_requests",
        ] {
            assert_eq!(
                store
                    .0
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE mod_id=?1"),
                        [a.id],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
        }
        assert_eq!(
            store
                .0
                .query_row("SELECT COUNT(*) FROM steering_messages", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
        let state = store.load_project(Path::new("/delete-first")).unwrap();
        assert_eq!(state.active_mod_id, Some(b.id));
        assert_eq!(state.mods.len(), 1);
        assert_eq!(state.mods[0].draft, "saved draft");
        assert_eq!(store.delete_mod(first.id, b.id).unwrap(), None);
        drop(store);
        let store = data.store();
        let state = store.load_project(Path::new("/delete-first")).unwrap();
        assert!(state.mods.is_empty());
        assert!(state.active_mod_id.is_none());
        let state = store.load_project(Path::new("/delete-second")).unwrap();
        assert_eq!(state.active_mod_id, Some(c.id));
        assert_eq!(state.mods[0].messages[0].body, "saved history");
        assert_eq!(state.mods[0].queue[0].body, "queued instruction");
        assert_eq!(state.mods[0].steering, ["steering instruction"]);
        assert_eq!(state.mods[0].draft, "saved draft");
    }

    #[test]
    fn a_failed_delete_rolls_back_history_queue_steering_workers_and_selection() {
        let data = TestData::new();
        let mut store = data.store();
        let project = Path::new("/failed-delete");
        let state = store.load_project(project).unwrap();
        let code_mod = store.legacy_mod(state.id, "keep this mod").unwrap();
        history(&store, code_mod.id, "keep history");
        let queued = store.enqueue(code_mod.id, "keep steering").unwrap();
        store.request_steering(code_mod.id, &[queued.id]).unwrap();
        let queued = store.enqueue(code_mod.id, "keep queue").unwrap();
        store.save_draft(code_mod.id, "keep draft").unwrap();
        let worker = store.worker(code_mod.id).unwrap();
        store.save_thread(worker.id, "keep conversation").unwrap();
        let source = source_id(queued.id, false);
        store.pending(worker.id, Some(&source)).unwrap();
        store.0.execute_batch("CREATE TRIGGER reject_delete BEFORE DELETE ON code_mods BEGIN SELECT RAISE(ABORT, 'simulated failure'); END;").unwrap();
        assert!(store.delete_mod(state.id, code_mod.id).is_err());
        drop(store);
        let store = data.store();
        let state = store.load_project(project).unwrap();
        assert_eq!(state.active_mod_id, Some(code_mod.id));
        assert_eq!(state.mods[0].messages[0].body, "keep history");
        assert_eq!(state.mods[0].queue[0].body, "keep queue");
        assert_eq!(state.mods[0].steering, ["keep steering"]);
        assert_eq!(state.mods[0].draft, "keep draft");
        let restored = store.worker(code_mod.id).unwrap();
        assert_eq!(restored.id, worker.id);
        assert_eq!(restored.thread_id.as_deref(), Some("keep conversation"));
        assert_eq!(restored.pending.as_deref(), Some(source.as_str()));
    }
    #[test]
    fn providers_keep_identity_and_receive_only_their_tasks() {
        let data = test_support::TestData::new();
        let mut store = data.store();
        let project = store.load_project(Path::new("/mixed-project")).unwrap();
        let m = store.create_mod(project.id, "Mixed providers").unwrap();
        let plan = Plan::parse(&serde_json::json!({"summary":"Two tasks","tasks":[
            {"id":"a","title":"A","outcome":"A","files":["a.txt"],"depends_on":[],"worker":"muse","checks":["A works"]},
            {"id":"b","title":"B","outcome":"B","files":["b.txt"],"depends_on":[],"worker":"codex","checks":["B works"]}
        ]}).to_string()).unwrap();
        store
            .save_plan(m.id, &m.planning.as_ref().unwrap().source, &plan)
            .unwrap();
        store
            .create_execution(m.id, &data.0.join("work"), &plan)
            .unwrap();
        let codex = store.worker_at(m.id, Role::Executor, 0).unwrap();
        let muse = store
            .worker_provider(m.id, Role::Executor, 0, "muse")
            .unwrap();
        assert_ne!(codex.id, muse.id);
        let c = store.task_input(m.id, codex.id, &plan).unwrap().unwrap();
        let a = store.task_input(m.id, muse.id, &plan).unwrap().unwrap();
        let execution = store.execution(m.id).unwrap().unwrap();
        assert_eq!(
            execution
                .tasks
                .iter()
                .find(|r| r.source == c.source)
                .unwrap()
                .task_id,
            "b"
        );
        assert_eq!(
            execution
                .tasks
                .iter()
                .find(|r| r.source == a.source)
                .unwrap()
                .task_id,
            "a"
        );
        drop(store);
        let store = data.store();
        let restored = store
            .worker_provider(m.id, Role::Executor, 0, "muse")
            .unwrap();
        assert_eq!(restored.id, muse.id);
        assert_eq!(restored.provider, "muse");
        assert!(
            store
                .worker_provider(m.id, Role::Planner, 0, "muse")
                .is_err()
        );
    }
}
