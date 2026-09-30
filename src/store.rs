use std::{fs, io, path::Path, time::Duration};

use directories::ProjectDirs;
use rusqlite::{Connection, OptionalExtension, Result, params};

pub struct CodeMod {
    pub id: i64,
    pub name: String,
    pub draft: String,
    pub messages: Vec<Message>,
    pub queue: Vec<QueuedMessage>,
    pub steering: Vec<String>,
}

#[derive(Clone)]
pub struct Message {
    pub item_id: Option<String>,
    pub role: String,
    pub body: String,
}

pub struct WorkerRecord {
    pub id: i64,
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

pub struct Store(Connection);

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
            .prepare("SELECT id, name, draft FROM code_mods WHERE project_id = ?1 ORDER BY id")?;
        let mut mods = statement
            .query_map([id], |row| {
                Ok(CodeMod {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    draft: row.get(2)?,
                    messages: Vec::new(),
                    queue: Vec::new(),
                    steering: Vec::new(),
                })
            })?
            .collect::<Result<Vec<_>>>()?;
        for code_mod in &mut mods {
            let mut statement = self.0.prepare(
                "SELECT item_id, role, body FROM messages WHERE mod_id = ?1 ORDER BY id",
            )?;
            code_mod.messages = statement
                .query_map([code_mod.id], |row| {
                    Ok(Message {
                        item_id: row.get(0)?,
                        role: row.get(1)?,
                        body: row.get(2)?,
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
        let transaction = self.0.transaction()?;
        transaction.execute(
            "INSERT INTO code_mods (project_id, name) VALUES (?1, ?2)",
            params![project_id, name],
        )?;
        let id = transaction.last_insert_rowid();
        transaction.execute(
            "UPDATE projects SET active_mod_id = ?1 WHERE id = ?2",
            params![id, project_id],
        )?;
        transaction.commit()?;
        Ok(CodeMod {
            id,
            name: name.to_owned(),
            draft: String::new(),
            messages: Vec::new(),
            queue: Vec::new(),
            steering: Vec::new(),
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

    pub fn save_draft(&self, mod_id: i64, draft: &str) -> Result<()> {
        self.0.execute(
            "UPDATE code_mods SET draft = ?1 WHERE id = ?2",
            params![draft, mod_id],
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

    pub fn request_steering(&mut self, mod_id: i64, ids: &[i64]) -> Result<Vec<String>> {
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
    pub fn worker(&self, mod_id: i64) -> Result<WorkerRecord> {
        let read = || {
            self.0.query_row("SELECT id, thread_id, pending FROM workers WHERE mod_id = ?1 ORDER BY id LIMIT 1", [mod_id],
            |row| Ok(WorkerRecord { id:row.get(0)?, thread_id:row.get(1)?, pending:row.get(2)? })).optional()
        };
        if let Some(worker) = read()? {
            return Ok(worker);
        }
        self.0
            .execute("INSERT INTO workers(mod_id) VALUES (?1)", [mod_id])?;
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

    pub fn acknowledge(&mut self, worker: i64, input: &Submission) -> Result<()> {
        let transaction = self.0.transaction()?;
        let mod_id: i64 = transaction.query_row(
            "SELECT mod_id FROM workers WHERE id = ?1 AND pending = ?2",
            params![worker, input.source],
            |row| row.get(0),
        )?;
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

    pub fn save_message(&self, mod_id: i64, message: &Message) -> Result<()> {
        self.0.execute("INSERT INTO messages(mod_id,item_id,role,body) VALUES (?1,?2,?3,?4) ON CONFLICT(mod_id,item_id) DO UPDATE SET body=excluded.body", params![mod_id,message.item_id,message.role,message.body])?;
        Ok(())
    }
}

pub fn source_id(id: i64, steering: bool) -> String {
    format!(
        "{:08x}-0000-4000-8000-{id:012x}",
        if steering { 2 } else { 1 }
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
        let a = store.create_mod(first.id, "feature A").unwrap();
        let b = store.create_mod(first.id, "feature B").unwrap();
        let c = store.create_mod(second.id, "feature C").unwrap();
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
        let code_mod = store.create_mod(project.id, "feature").unwrap();
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
        let a = store.create_mod(project.id, "feature A").unwrap();
        let b = store.create_mod(project.id, "feature B").unwrap();
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
        let a = store.create_mod(project.id, "A").unwrap();
        let b = store.create_mod(project.id, "B").unwrap();
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
        let code_mod = store.create_mod(project.id, "feature").unwrap();
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
        let code_mod = store.create_mod(project.id, "existing mod").unwrap();
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
        let code_mod = store.create_mod(project.id, "existing feature").unwrap();
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
        let code_mod = store.create_mod(state.id, "feature").unwrap();
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
        let a = store.create_mod(state.id, "a").unwrap();
        let b = store.create_mod(state.id, "b").unwrap();
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
                    },
                )
                .unwrap();
        }
        let state = store.load_project(project).unwrap();
        assert_eq!(state.mods[0].messages.len(), 2);
        assert_eq!(state.mods[0].messages[1].body, "complete reply");
        assert_eq!(state.mods[0].messages[1].role, "codex");
        assert!(state.mods[1].messages.is_empty());
    }
    #[test]
    fn worker_migration_preserves_legacy_history_queue_and_draft() {
        let data = TestData::new();
        let project = Path::new("/legacy-worker-project");
        let mut store = data.store();
        let state = store.load_project(project).unwrap();
        let code_mod = store.create_mod(state.id, "existing feature").unwrap();
        history(&store, code_mod.id, "old conversation");
        store.enqueue(code_mod.id, "existing instruction").unwrap();
        store.save_draft(code_mod.id, "unfinished draft").unwrap();
        store.0.execute_batch("DROP INDEX message_items; ALTER TABLE messages DROP COLUMN item_id; ALTER TABLE messages DROP COLUMN role; DROP TABLE workers;").unwrap();
        drop(store);
        let store = data.store();
        let state = store.load_project(project).unwrap();
        assert_eq!(state.mods[0].messages[0].body, "old conversation");
        assert_eq!(state.mods[0].messages[0].role, "user");
        assert!(state.mods[0].messages[0].item_id.is_none());
        assert_eq!(state.mods[0].queue[0].body, "existing instruction");
        assert_eq!(state.mods[0].draft, "unfinished draft");
        assert!(store.worker(code_mod.id).unwrap().thread_id.is_none());
    }
}
