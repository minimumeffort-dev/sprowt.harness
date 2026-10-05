use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    sync::atomic::AtomicBool,
};

use serde::{Deserialize, Serialize};

use crate::{
    sandbox::Sandbox,
    workspace::{self, Snapshot},
};

const GIT: &str = "/opt/sprowt-git/repo";

#[derive(Default, Deserialize, Serialize)]
pub struct TaskWorktrees {
    pub round: Vec<i64>,
    #[serde(default)]
    pub owners: BTreeMap<i64, i64>,
    pub active: Option<i64>,
    pub integrated: BTreeSet<i64>,
    #[serde(default)]
    pub cleaned: bool,
}

pub fn task_id(source: &str) -> io::Result<i64> {
    if !source.starts_with("00000004-") {
        return Err(io::Error::other("Invalid task identity."));
    }
    source
        .rsplit('-')
        .next()
        .and_then(|id| i64::from_str_radix(id, 16).ok())
        .filter(|id| *id > 0)
        .ok_or_else(|| io::Error::other("Invalid task identity."))
}

pub fn worker_home(id: i64) -> String {
    format!("/home/sprowt/workers/{id}")
}

pub fn folder(id: i64) -> String {
    format!("/tasks/{id}")
}

impl Sandbox {
    pub(crate) fn import_update(&mut self, cancelled: &AtomicBool) -> io::Result<()> {
        let Some(mut update) = crate::mod_sync::load(self.root())? else {
            return Ok(());
        };
        if update.imported {
            return Ok(());
        }
        let source = workspace::source_state(&self.root().join("work"))?;
        self.replace_guest_source("/workspace", &source, cancelled)?;
        if self.guest_exists(&format!("{GIT}/HEAD"))? {
            self.commit_source(None, &source, cancelled)?;
        }
        for id in self.tasks.round.clone() {
            self.remove_task(id, cancelled)?;
            self.git(
                None,
                &["update-ref", "-d", &format!("refs/heads/task/{id}")],
                cancelled,
            )?;
        }
        self.tasks = TaskWorktrees::default();
        let drafts = self.root().join("task-drafts");
        if drafts.exists() {
            fs::remove_dir_all(drafts)?;
        }
        self.save_tasks()?;
        if self.guest_exists(&format!("{GIT}/HEAD"))? {
            self.checkpoint_tasks(cancelled)?;
        }
        update.imported = true;
        crate::mod_sync::save(self.root(), &update)
    }

    pub fn assign_task(&mut self, id: i64, worker: i64, cancelled: &AtomicBool) -> io::Result<()> {
        self.tasks.owners.insert(id, worker);
        self.guest(&["/bin/mkdir", "-p", &worker_home(worker)], cancelled)?;
        self.activate_task(id, cancelled)?;
        self.save_tasks()
    }

    pub fn runtime_home(&self) -> String {
        self.tasks
            .active
            .and_then(|id| self.tasks.owners.get(&id).copied())
            .map_or_else(|| "/home/sprowt".into(), worker_home)
    }

    pub fn task_folder(&self) -> String {
        self.tasks
            .active
            .map_or_else(|| "/workspace".into(), folder)
    }

    pub(crate) fn restore_tasks(&mut self, cancelled: &AtomicBool) -> io::Result<()> {
        let path = self.root().join("tasks.json");
        if !path.exists() {
            return Ok(());
        }
        self.tasks = serde_json::from_slice(&fs::read(path)?)?;
        if self.tasks.round.iter().any(|id| *id <= 0)
            || self
                .tasks
                .active
                .is_some_and(|id| !self.tasks.round.contains(&id))
            || self
                .tasks
                .integrated
                .iter()
                .any(|id| !self.tasks.round.contains(id))
        {
            return Err(io::Error::other("Invalid saved task worktrees."));
        }
        self.guest(
            &["/bin/mkdir", "-p", "/tasks", "/opt/sprowt-git"],
            cancelled,
        )?;
        if !self.guest_exists("/opt/sprowt-git/ready")? {
            let bundle = self.root().join("tasks.bundle");
            if !bundle.exists() {
                return Err(io::Error::other(
                    "Task checkpoint is missing; saved source is retained.",
                ));
            }
            self.copy_in(&bundle, "/opt/sprowt-git/restore.bundle", cancelled)?;
            self.guest(&["/bin/rm", "-rf", GIT, "/tasks"], cancelled)?;
            self.guest(&["/bin/mkdir", "-p", "/tasks"], cancelled)?;
            self.git(None, &["init", "--bare", GIT], cancelled)?;
            self.git(
                None,
                &[
                    "fetch",
                    "/opt/sprowt-git/restore.bundle",
                    "refs/heads/*:refs/heads/*",
                ],
                cancelled,
            )?;
            // The imported review snapshot may include unfinished edits; the bundle retains them.
            self.git(None, &["config", "core.bare", "false"], cancelled)?;
            self.git(
                None,
                &["symbolic-ref", "HEAD", "refs/heads/integration"],
                cancelled,
            )?;
            self.replace_guest_source("/workspace", &Vec::new(), cancelled)?;
            self.git(None, &["reset", "--hard", "integration"], cancelled)?;
            self.guest(&["/usr/bin/touch", "/opt/sprowt-git/ready"], cancelled)?;
            self.guest(&["/bin/rm", "/opt/sprowt-git/restore.bundle"], cancelled)?;
        }
        for id in self.tasks.round.clone() {
            if self.tasks.cleaned {
                break;
            }
            if self.git_status(
                None,
                &[
                    "show-ref",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/task/{id}"),
                ],
            )? {
                self.ensure_task(id, cancelled)?;
            }
        }
        Ok(())
    }

    pub fn prepare_tasks(&mut self, ids: &[i64], cancelled: &AtomicBool) -> io::Result<()> {
        if ids.is_empty() || ids.iter().any(|id| *id <= 0) {
            return Err(io::Error::other("Execution needs valid task identities."));
        }
        if self.tasks.round == ids {
            return Ok(());
        }
        self.export(cancelled)?;
        let source = workspace::source_state(&self.root().join("work"))?;
        self.guest(
            &["/bin/mkdir", "-p", "/tasks", "/opt/sprowt-git"],
            cancelled,
        )?;
        if !self.guest_exists(&format!("{GIT}/HEAD"))? {
            if self.guest_exists("/workspace/.git")? {
                return Err(io::Error::other(
                    "Unexpected guest Git metadata; source is retained.",
                ));
            }
            self.git(
                None,
                &[
                    "init",
                    "--initial-branch=integration",
                    "--separate-git-dir",
                    GIT,
                    "/workspace",
                ],
                cancelled,
            )?;
        } else {
            // A revised plan starts from the latest saved edits, including a paused task.
            self.replace_guest_source("/workspace", &source, cancelled)?;
        }
        self.commit_source(None, &source, cancelled)?;
        self.guest(&["/usr/bin/touch", "/opt/sprowt-git/ready"], cancelled)?;
        for id in self.tasks.round.clone() {
            self.remove_task(id, cancelled)?;
            self.git(
                None,
                &["update-ref", "-d", &format!("refs/heads/task/{id}")],
                cancelled,
            )?;
        }
        let drafts = self.root().join("task-drafts");
        if drafts.exists() {
            fs::remove_dir_all(drafts)?;
        }
        self.tasks = TaskWorktrees {
            round: ids.to_vec(),
            ..Default::default()
        };
        self.save_tasks()?;
        self.checkpoint_tasks(cancelled)
    }

    pub fn activate_task(&mut self, id: i64, cancelled: &AtomicBool) -> io::Result<()> {
        if !self.tasks.round.contains(&id) {
            return Err(io::Error::other("Task does not belong to this execution."));
        }
        self.ensure_task(id, cancelled)?;
        let dir = self.root().join("task-drafts");
        fs::create_dir_all(&dir)?;
        let base = dir.join(format!("{id}-base.json"));
        if !base.exists() {
            fs::write(
                base,
                serde_json::to_vec(&self.snapshot("/workspace", cancelled)?)?,
            )?;
        }
        if self.tasks.active == Some(id) {
            return Ok(());
        }
        self.tasks.active = Some(id);
        self.tasks.cleaned = false;
        self.save_tasks()?;
        self.export(cancelled)?;
        Ok(())
    }

    fn ensure_task(&self, id: i64, cancelled: &AtomicBool) -> io::Result<()> {
        let path = folder(id);
        if self.guest_exists(&format!("{path}/.git"))? {
            return Ok(());
        }
        let branch = format!("task/{id}");
        if self.git_status(
            None,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )? {
            self.git(None, &["worktree", "add", &path, &branch], cancelled)
        } else {
            self.git(
                None,
                &["worktree", "add", "-b", &branch, &path, "integration"],
                cancelled,
            )
        }
    }

    pub fn integrate_task(&mut self, id: i64, cancelled: &AtomicBool) -> io::Result<()> {
        if self.tasks.active != Some(id) {
            return Err(io::Error::other("The verified task is no longer active."));
        }
        self.export(cancelled)?;
        if self
            .git(
                None,
                &["merge", "--no-ff", "--no-edit", &format!("task/{id}")],
                cancelled,
            )
            .is_err()
        {
            let keep = AtomicBool::new(false);
            self.git(None, &["merge", "--abort"], &keep)?;
            // Bring non-conflicting changes and conflict markers into the saved draft.
            let merged = self.git(
                Some(id),
                &["merge", "--no-ff", "--no-edit", "integration"],
                &keep,
            );
            if merged.is_err() && !self.guest_exists(&format!("{GIT}/worktrees/{id}/MERGE_HEAD"))? {
                merged?;
            }
            self.export(&keep)?;
            return Err(io::Error::other(
                "Task changes conflict with the combined source. Both versions are saved; Ctrl+R retries from the draft with conflict markers, or send edits to replan.",
            ));
        }
        self.tasks.integrated.insert(id);
        self.tasks.active = None;
        self.save_tasks()?;
        self.export(cancelled)?;
        Ok(())
    }

    pub fn verify_execution(
        &mut self,
        source: &str,
        checks: &[crate::execution::Check],
        cancelled: &AtomicBool,
    ) -> io::Result<(Snapshot, Vec<crate::execution::CheckResult>)> {
        let keep = AtomicBool::new(false);
        if !source.starts_with("final:") {
            let id = task_id(source)?;
            self.activate_task(id, cancelled)?;
            let (before, results) = self.verify(checks, cancelled)?;
            let unchanged = before == self.snapshot(&folder(id), &keep)?;
            let passed = unchanged
                && !checks.is_empty()
                && results.len() == checks.len()
                && results.iter().all(|result| result.exit_code == Some(0));
            if passed {
                self.integrate_task(id, cancelled)?;
                return Ok((self.snapshot("/workspace", &keep)?, results));
            }
            let mut results = results;
            if !unchanged && let Some(result) = results.last_mut() {
                result.exit_code = None;
                result.output =
                    "Verification changed source files. Fix the check before retrying.".into();
            }
            return Ok((before, results));
        }
        let combined = self.snapshot("/workspace", cancelled)?;
        let mut results = Vec::new();
        for group in checks.chunk_by(|a, b| a.task == b.task) {
            if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            if let Some(id) = group[0].task {
                self.prepare_final_check(id, cancelled)?;
            } else {
                self.tasks.active = None;
            }
            let (_, mut checked) = self.verify(group, cancelled)?;
            if let Some(result) = checked.last_mut()
                && self.snapshot(&self.task_folder(), &keep)? != combined
            {
                result.exit_code = None;
                result.output =
                    "Verification changed source files. Fix the check before retrying.".into();
            }
            let failed = checked.len() != group.len()
                || checked.iter().any(|result| result.exit_code != Some(0));
            results.extend(checked);
            if failed {
                break;
            }
        }
        let passed = !checks.is_empty()
            && results.len() == checks.len()
            && results.iter().all(|result| result.exit_code == Some(0));
        self.finish_checks(passed, &keep)?;
        Ok((combined, results))
    }

    pub fn prepare_final_check(&mut self, id: i64, cancelled: &AtomicBool) -> io::Result<()> {
        if !self.tasks.integrated.contains(&id) {
            return Err(io::Error::other(
                "Cannot verify a task that has not been combined.",
            ));
        }
        self.ensure_task(id, cancelled)?;
        self.git(Some(id), &["reset", "--hard", "integration"], cancelled)?;
        self.tasks.active = Some(id);
        self.tasks.cleaned = false;
        self.save_tasks()
    }

    pub fn finish_checks(&mut self, passed: bool, cancelled: &AtomicBool) -> io::Result<()> {
        self.tasks.active = None;
        self.save_tasks()?;
        self.export(cancelled)?;
        if passed {
            for id in self.tasks.round.clone() {
                self.remove_task(id, cancelled)?;
            }
            self.tasks.cleaned = true;
            self.save_tasks()?;
        }
        Ok(())
    }

    fn remove_task(&self, id: i64, cancelled: &AtomicBool) -> io::Result<()> {
        if self.guest_exists(&format!("{GIT}/worktrees/{id}"))? {
            self.git(
                None,
                &["worktree", "remove", "--force", &folder(id)],
                cancelled,
            )?;
        }
        Ok(())
    }

    pub(crate) fn save_draft(&self, source: &Snapshot) -> io::Result<()> {
        if let Some(id) = self
            .tasks
            .active
            .filter(|id| !self.tasks.integrated.contains(id))
        {
            let dir = self.root().join("task-drafts");
            fs::create_dir_all(&dir)?;
            let next = dir.join(format!("{id}.next"));
            fs::write(&next, serde_json::to_vec(source)?)?;
            fs::rename(next, dir.join(format!("{id}.json")))?;
        }
        Ok(())
    }

    pub(crate) fn review_source(&mut self, cancelled: &AtomicBool) -> io::Result<Snapshot> {
        let mut combined = self
            .snapshot("/workspace", cancelled)?
            .into_iter()
            .map(|(path, bytes, mode)| (path, (bytes, mode)))
            .collect::<BTreeMap<_, _>>();
        for id in &self.tasks.round {
            if self.tasks.integrated.contains(id) {
                continue;
            }
            let dir = self.root().join("task-drafts");
            let draft = dir.join(format!("{id}.json"));
            let base = dir.join(format!("{id}-base.json"));
            if !draft.exists() || !base.exists() {
                continue;
            }
            let mut base: Snapshot = serde_json::from_slice(&fs::read(base)?)?;
            let mut draft: Snapshot = serde_json::from_slice(&fs::read(draft)?)?;
            base.retain(|(path, _, _)| workspace::source_path(self.root(), path));
            draft.retain(|(path, _, _)| workspace::source_path(self.root(), path));
            let base: BTreeMap<_, _> = base.into_iter().map(|(p, b, m)| (p, (b, m))).collect();
            let draft: BTreeMap<_, _> = draft.into_iter().map(|(p, b, m)| (p, (b, m))).collect();
            for path in base.keys().chain(draft.keys()).collect::<BTreeSet<_>>() {
                if base.get(path) == draft.get(path) {
                    continue;
                }
                let current = combined.get(path);
                let changed = draft.get(path);
                if current != base.get(path)
                    && current != changed
                    && !changed.is_some_and(|(bytes, _)| bytes.starts_with(b"<<<<<<<"))
                {
                    let left = current.map_or(&[][..], |(bytes, _)| bytes.as_slice());
                    let right = changed.map_or(&[][..], |(bytes, _)| bytes.as_slice());
                    let (Ok(left), Ok(right)) =
                        (std::str::from_utf8(left), std::str::from_utf8(right))
                    else {
                        return Err(io::Error::other(
                            "Unfinished tasks conflict in a binary file. Both branches are saved; resolve the task before closing or replanning.",
                        ));
                    };
                    let mode = changed.or(current).map_or(0o644, |(_, mode)| *mode);
                    let bytes = format!(
                        "<<<<<<< combined draft\n{left}\n=======\n{right}\n>>>>>>> task {id}\n"
                    )
                    .into_bytes();
                    combined.insert(path.clone(), (bytes, mode));
                } else if let Some(file) = changed {
                    combined.insert(path.clone(), file.clone());
                } else {
                    combined.remove(path);
                }
            }
        }
        Ok(combined.into_iter().map(|(p, (b, m))| (p, b, m)).collect())
    }

    pub fn checkpoint_task(&mut self, id: i64) -> io::Result<()> {
        if !self.tasks.integrated.contains(&id)
            && self.guest_exists(&format!("{}/.git", folder(id)))?
        {
            self.tasks.active = Some(id);
            self.export(&AtomicBool::new(false))?;
        }
        Ok(())
    }

    pub(crate) fn checkpoint_tasks(&mut self, cancelled: &AtomicBool) -> io::Result<()> {
        if self.tasks.round.is_empty() {
            return Ok(());
        }
        self.git(
            None,
            &[
                "bundle",
                "create",
                "/opt/sprowt-git/checkpoint.bundle",
                "--all",
            ],
            cancelled,
        )?;
        self.copy_out(
            "/opt/sprowt-git/checkpoint.bundle",
            &self.root().join("tasks.bundle"),
            cancelled,
        )
    }

    pub(crate) fn commit_source(
        &self,
        id: Option<i64>,
        source: &Snapshot,
        cancelled: &AtomicBool,
    ) -> io::Result<()> {
        // Checkpoints contain exact source; runtime files remain on disk.
        self.git(id, &["read-tree", "--empty"], cancelled)?;
        let paths = self.root().join("task-paths");
        let mut bytes = Vec::new();
        for (path, _, _) in source {
            bytes.extend_from_slice(path.as_os_str().as_encoded_bytes());
            bytes.push(0);
        }
        if !bytes.is_empty() {
            fs::write(&paths, bytes)?;
            self.copy_in(&paths, "/opt/sprowt-git/paths", cancelled)?;
            self.git(
                id,
                &[
                    "add",
                    "--force",
                    "--pathspec-from-file=/opt/sprowt-git/paths",
                    "--pathspec-file-nul",
                ],
                cancelled,
            )?;
            fs::remove_file(paths)?;
        }
        let branch = id.map_or_else(|| "integration".into(), |id| format!("task/{id}"));
        if self.git_status(
            id,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )? && self.git_status(
            id,
            &["diff", "--cached", "--quiet", "--no-ext-diff", "HEAD"],
        )? {
            return Ok(());
        }
        self.git(
            id,
            &["commit", "--allow-empty", "-m", "Save task source"],
            cancelled,
        )
    }

    fn save_tasks(&self) -> io::Result<()> {
        let path = self.root().join("tasks-next.json");
        fs::write(&path, serde_json::to_vec(&self.tasks)?)?;
        fs::rename(path, self.root().join("tasks.json"))
    }

    fn git(&self, id: Option<i64>, args: &[&str], cancelled: &AtomicBool) -> io::Result<()> {
        let command = Self::git_command(id, args);
        self.guest(
            &command.iter().map(String::as_str).collect::<Vec<_>>(),
            cancelled,
        )
    }

    fn git_status(&self, id: Option<i64>, args: &[&str]) -> io::Result<bool> {
        let command = Self::git_command(id, args);
        self.guest_status(&command.iter().map(String::as_str).collect::<Vec<_>>())
    }

    fn git_command(id: Option<i64>, args: &[&str]) -> Vec<String> {
        let dir = id.map_or_else(|| GIT.into(), |id| format!("{GIT}/worktrees/{id}"));
        let cwd = id.map_or_else(|| "/workspace".into(), folder);
        let mut command = vec![
            "/usr/bin/env",
            "-i",
            "PATH=/usr/bin:/bin",
            "HOME=/opt/sprowt-git",
            "GIT_CONFIG_NOSYSTEM=1",
            "GIT_CONFIG_GLOBAL=/dev/null",
            "GIT_TERMINAL_PROMPT=0",
            "GIT_ATTR_NOSYSTEM=1",
            "/usr/bin/git",
            "--literal-pathspecs",
            "-c",
            "user.name=Sprowt",
            "-c",
            "user.email=sprowt@localhost",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.attributesFile=/dev/null",
            "-c",
            "commit.gpgSign=false",
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.file.allow=always",
            "-c",
            "submodule.recurse=false",
            "-c",
            "gc.auto=0",
        ];
        if args.first() != Some(&"init") {
            command.extend(["--git-dir", &dir, "--work-tree", &cwd]);
        }
        command.extend_from_slice(args);
        command.into_iter().map(str::to_owned).collect()
    }
}
