use rusqlite::{OptionalExtension, Result, params};
use serde::{Deserialize, Serialize};

use crate::{
    plan::Role,
    router::Selection,
    store::{Store, WorkerRecord},
};

pub mod catalog;
mod scheduling;
#[cfg(test)]
pub(crate) mod tests;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Codex,
    Muse,
}
impl Provider {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Muse => "muse",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModelChoice {
    pub provider: Provider,
    pub model: String,
    pub effort: String,
}
impl ModelChoice {
    pub fn selection(&self) -> Selection {
        Selection {
            pinned: true,
            model: self.model.clone(),
            effort: self.effort.clone(),
            reason: "Selected worker model".into(),
            evidence: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Allocation {
    pub choice: ModelChoice,
    pub count: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkerSettings {
    pub planner: ModelChoice,
    pub executors: Vec<Allocation>,
    pub reviewer: ModelChoice,
}
impl Default for WorkerSettings {
    fn default() -> Self {
        let choice = |model: &str, effort: &str| ModelChoice {
            provider: Provider::Codex,
            model: model.into(),
            effort: effort.into(),
        };
        Self {
            planner: choice("gpt-6-astra", "xhigh"),
            executors: vec![Allocation {
                choice: choice("gpt-6.1-sol", "high"),
                count: 2,
            }],
            reviewer: choice("gpt-6.1-sol", "xhigh"),
        }
    }
}
impl WorkerSettings {
    pub fn capacity(&self) -> usize {
        self.executors.iter().map(|a| a.count).sum()
    }
    pub fn choices(&self) -> impl Iterator<Item = &ModelChoice> {
        std::iter::once(&self.planner)
            .chain(self.executors.iter().map(|a| &a.choice))
            .chain(std::iter::once(&self.reviewer))
    }
    pub fn preferred(&self, role: Role) -> &ModelChoice {
        match role {
            Role::Planner => &self.planner,
            Role::Reviewer => &self.reviewer,
            Role::Executor => &self.executors[0].choice,
        }
    }
    pub fn validate(&self, catalog: &catalog::Catalog) -> std::result::Result<(), String> {
        if self.executors.is_empty()
            || self.executors.len() > 16
            || self.executors.iter().any(|a| a.count == 0 || a.count > 16)
            || self.capacity() > 16
        {
            return Err("Choose between 1 and 16 simultaneous execution workers.".into());
        }
        for (i, allocation) in self.executors.iter().enumerate() {
            if self.executors[..i]
                .iter()
                .any(|a| a.choice == allocation.choice)
            {
                return Err("Combine identical execution models into one row.".into());
            }
        }
        for choice in self.choices() {
            catalog.validate(choice)?;
        }
        Ok(())
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}
fn decode<T: serde::de::DeserializeOwned>(text: String) -> Result<T> {
    serde_json::from_str(&text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

impl Store {
    pub(crate) fn worker_settings_schema(&self) -> Result<()> {
        self.0.execute_batch("CREATE TABLE IF NOT EXISTS worker_settings (mod_id INTEGER PRIMARY KEY REFERENCES code_mods(id) ON DELETE CASCADE, settings TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS worker_team_drafts (project_id INTEGER PRIMARY KEY REFERENCES projects(id), settings TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS worker_choices (worker_id INTEGER PRIMARY KEY REFERENCES workers(id) ON DELETE CASCADE, choice TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS worker_role_assignments (worker_id INTEGER PRIMARY KEY REFERENCES workers(id) ON DELETE CASCADE, source TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS quota_workers (worker_id INTEGER PRIMARY KEY REFERENCES workers(id) ON DELETE CASCADE);
            CREATE TABLE IF NOT EXISTS quota_cooldowns (provider TEXT NOT NULL, bucket TEXT NOT NULL, model TEXT, resets_at_ms INTEGER, observed_at_ms INTEGER NOT NULL, PRIMARY KEY(provider,bucket));")
    }
    pub fn worker_settings(&self, mod_id: i64) -> Result<Option<WorkerSettings>> {
        self.0
            .query_row(
                "SELECT settings FROM worker_settings WHERE mod_id=?1",
                [mod_id],
                |r| r.get(0),
            )
            .optional()?
            .map(decode)
            .transpose()
    }
    pub fn team_draft(&self, project: i64) -> Result<Option<WorkerSettings>> {
        self.0
            .query_row(
                "SELECT settings FROM worker_team_drafts WHERE project_id=?1",
                [project],
                |r| r.get(0),
            )
            .optional()?
            .map(decode)
            .transpose()
    }
    pub fn save_worker_settings(
        &self,
        mod_id: Option<i64>,
        project: i64,
        settings: &WorkerSettings,
    ) -> Result<()> {
        let json = serde_json::to_string(settings).unwrap();
        match mod_id {
            Some(id) => {
                self.0.execute("INSERT INTO worker_settings(mod_id,settings) VALUES(?1,?2) ON CONFLICT(mod_id) DO UPDATE SET settings=excluded.settings",params![id,json])?;
            }
            None => {
                self.0.execute("INSERT INTO worker_team_drafts(project_id,settings) VALUES(?1,?2) ON CONFLICT(project_id) DO UPDATE SET settings=excluded.settings",params![project,json])?;
            }
        }
        Ok(())
    }
    pub fn worker_choice(&self, worker: i64) -> Result<Option<ModelChoice>> {
        self.0
            .query_row(
                "SELECT choice FROM worker_choices WHERE worker_id=?1",
                [worker],
                |r| r.get(0),
            )
            .optional()?
            .map(decode)
            .transpose()
    }
    pub(crate) fn new_model_worker(
        &self,
        mod_id: i64,
        role: Role,
        choice: &ModelChoice,
    ) -> Result<WorkerRecord> {
        let tx = self.0.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO workers(mod_id,role,provider) VALUES(?1,?2,?3)",
            params![mod_id, role.name(), choice.provider.name()],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO worker_choices(worker_id,choice) VALUES(?1,?2)",
            params![id, serde_json::to_string(choice).unwrap()],
        )?;
        if role != Role::Executor {
            let source = match role {
                Role::Planner => self.planning(mod_id)?.map(|p| p.source),
                Role::Reviewer => self.review_state(mod_id)?.map(|r| r.source),
                Role::Executor => None,
            };
            if let Some(source) = source {
                tx.execute(
                    "INSERT INTO worker_role_assignments(worker_id,source) VALUES(?1,?2)",
                    params![id, source],
                )?;
            }
        }
        tx.commit()?;
        Ok(WorkerRecord {
            database: std::path::Path::new(self.0.path().unwrap()).into(),
            id,
            provider: choice.provider.name().into(),
            thread_id: None,
            pending: None,
        })
    }
    pub fn executor_capacity(&self, mod_id: i64) -> Result<usize> {
        Ok(self
            .worker_settings(mod_id)?
            .map_or(crate::scheduler::CAPACITY, |s| s.capacity()))
    }
    pub(crate) fn quota_blocked(&self, choice: &ModelChoice) -> Result<bool> {
        self.0.query_row("SELECT EXISTS(SELECT 1 FROM quota_cooldowns WHERE provider=?1 AND (model IS NULL OR model=?3) AND (resets_at_ms IS NULL OR resets_at_ms>?2))",params![choice.provider.name(),now_ms(),choice.model],|r|r.get(0))
    }
    pub(crate) fn save_quota(&self, provider: &str, quota: &crate::quota::Exhausted) -> Result<()> {
        self.0.execute("INSERT INTO quota_cooldowns(provider,bucket,model,resets_at_ms,observed_at_ms) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(provider,bucket) DO UPDATE SET model=excluded.model,resets_at_ms=excluded.resets_at_ms,observed_at_ms=excluded.observed_at_ms",params![provider,quota.bucket,quota.model,quota.resets_at_ms,now_ms()])?;
        Ok(())
    }
    pub(crate) fn observe_quota(
        &self,
        provider: &str,
        observations: &[crate::quota::Observation],
    ) -> Result<()> {
        let tx = self.0.unchecked_transaction()?;
        if provider == "codex"
            && let Some(observed) = observations.iter().map(|o| o.observed_at_ms).min()
        {
            tx.execute("DELETE FROM quota_cooldowns WHERE provider=?1 AND bucket='unknown' AND observed_at_ms<?2",params![provider,observed])?;
        }
        for observation in observations {
            if observation.exhausted {
                tx.execute("INSERT INTO quota_cooldowns(provider,bucket,model,resets_at_ms,observed_at_ms) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(provider,bucket) DO UPDATE SET model=excluded.model,resets_at_ms=excluded.resets_at_ms,observed_at_ms=excluded.observed_at_ms WHERE excluded.observed_at_ms>=quota_cooldowns.observed_at_ms",params![provider,observation.bucket,observation.model,observation.resets_at_ms,observation.observed_at_ms])?;
            } else {
                tx.execute("DELETE FROM quota_cooldowns WHERE provider=?1 AND bucket=?2 AND observed_at_ms<?3",params![provider,observation.bucket,observation.observed_at_ms])?;
            }
        }
        tx.commit()
    }
    pub(crate) fn quota_workers(&self, mod_id: i64) -> Result<Vec<(i64, Role)>> {
        self.0.prepare("SELECT w.id,w.role FROM workers w JOIN quota_workers q ON q.worker_id=w.id WHERE w.mod_id=?1 ORDER BY w.id")?.query_map([mod_id],|r|{let role:String=r.get(1)?;Ok((r.get(0)?,match role.as_str(){"planner"=>Role::Planner,"reviewer"=>Role::Reviewer,_=>Role::Executor}))})?.collect()
    }
    pub(crate) fn mark_quota_worker(&self, worker: i64) -> Result<()> {
        self.0.execute(
            "INSERT OR IGNORE INTO quota_workers(worker_id) VALUES(?1)",
            [worker],
        )?;
        Ok(())
    }

    pub(crate) fn accepted_instruction_context(&self, mod_id: i64) -> Result<String> {
        let texts = self.0.prepare("SELECT body FROM messages WHERE mod_id=?1 AND role='user' AND (item_id LIKE '00000001-%' OR item_id LIKE '00000002-%') ORDER BY id")?
            .query_map([mod_id], |r| r.get::<_,String>(0))?.collect::<Result<Vec<_>>>()?;
        Ok(if texts.is_empty() {
            String::new()
        } else {
            format!(
                "Previously accepted user instructions, in order (saved context, not new delivery; later corrections take precedence):\n{}",
                texts.join("\n\n")
            )
        })
    }

    pub(crate) fn bind_role_worker(&self, worker: i64, mod_id: i64, role: Role) -> Result<()> {
        let source = match role {
            Role::Planner => self.planning(mod_id)?.map(|p| p.source),
            Role::Reviewer => self.review_state(mod_id)?.map(|r| r.source),
            Role::Executor => None,
        };
        if let Some(source) = source {
            self.0.execute("INSERT INTO worker_role_assignments(worker_id,source) VALUES(?1,?2) ON CONFLICT(worker_id) DO UPDATE SET source=excluded.source",params![worker,source])?;
        }
        Ok(())
    }

    pub(crate) fn role_worker(
        &self,
        mod_id: i64,
        role: Role,
        muse: bool,
    ) -> Result<Option<WorkerRecord>> {
        let Some(settings) = self.worker_settings(mod_id)? else {
            return self.worker_provider(mod_id, role, 0, "codex").map(Some);
        };
        // A pending receipt must reconnect its original session before any new allocation.
        let existing=self.0.query_row("SELECT id,provider,thread_id,pending FROM workers WHERE mod_id=?1 AND role=?2 ORDER BY id DESC LIMIT 1",params![mod_id,role.name()],|r|Ok(WorkerRecord{database:std::path::Path::new(self.0.path().unwrap()).into(),id:r.get(0)?,provider:r.get(1)?,thread_id:r.get(2)?,pending:r.get(3)?})).optional()?;
        if existing.as_ref().is_some_and(|r| r.pending.is_some()) {
            return Ok(existing);
        }
        if let Some(record) = &existing {
            let source = match role {
                Role::Planner => self.planning(mod_id)?.map(|p| p.source),
                Role::Reviewer => self.review_state(mod_id)?.map(|r| r.source),
                Role::Executor => None,
            };
            let assigned: Option<String> = self
                .0
                .query_row(
                    "SELECT source FROM worker_role_assignments WHERE worker_id=?1",
                    [record.id],
                    |r| r.get(0),
                )
                .optional()?;
            if source.is_some() && source == assigned {
                return Ok(existing);
            }
        }
        let mut choices = vec![settings.preferred(role)];
        for choice in settings.choices() {
            if !choices.contains(&choice) {
                choices.push(choice);
            }
        }
        let mut chosen = None;
        for choice in choices {
            if (muse || choice.provider == Provider::Codex) && !self.quota_blocked(choice)? {
                chosen = Some(choice);
                break;
            }
        }
        let Some(choice) = chosen else {
            return Ok(None);
        };
        self.new_model_worker(mod_id, role, choice).map(Some)
    }
}
