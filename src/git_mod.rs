use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::workspace::{self, Review};

#[derive(Clone, Deserialize, Serialize)]
pub struct GitMod {
    pub repo: PathBuf,
    pub branch: String,
    pub base: String,
    pub base_branch: Option<String>,
    pub phase: String,
    pub fingerprint: Option<String>,
    pub head: Option<String>,
    pub pr: Option<String>,
    pub draft: bool,
    pub removing: bool,
    #[serde(default)]
    pub publishing: bool,
}

pub enum Result {
    Prepared,
    Snapshot,
    Reviewed(Review),
    Published,
    Removed,
}

pub struct Job {
    pub label: &'static str,
    receiver: Receiver<io::Result<Result>>,
    cancelled: Arc<AtomicBool>,
    task: Option<JoinHandle<()>>,
}

impl Job {
    pub fn start(
        label: &'static str,
        work: impl FnOnce(&AtomicBool) -> io::Result<Result> + Send + 'static,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let task = thread::spawn(move || {
            let _ = sender.send(work(&flag));
        });
        Self {
            label,
            receiver,
            cancelled,
            task: Some(task),
        }
    }

    pub fn poll(&self) -> Option<io::Result<Result>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(io::Error::other(
                "Git operation stopped; Ctrl+R retries.",
            ))),
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

pub fn project_root(project: &Path) -> Option<PathBuf> {
    let output = workspace::trusted_git()
        .arg("-C")
        .arg(project)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()))
        .and_then(|path| path.canonicalize().ok())
}

pub fn checkout(root: &Path) -> PathBuf {
    root.join("checkout")
}

pub fn load(root: &Path) -> io::Result<GitMod> {
    serde_json::from_slice(&fs::read(root.join("git-mod.json"))?).map_err(io::Error::other)
}

fn save(root: &Path, state: &GitMod) -> io::Result<()> {
    let next = root.join("git-mod-next.json");
    fs::write(&next, serde_json::to_vec(state)?)?;
    private(&next)?;
    fs::File::open(&next)?.sync_all()?;
    fs::rename(next, root.join("git-mod.json"))
}

fn private(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if path.is_dir() { 0o700 } else { 0o600 }),
        )?;
    }
    Ok(())
}

fn run(mut command: Command, root: &Path, cancelled: &AtomicBool) -> io::Result<Vec<u8>> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(io::Error::other("Git operation paused; Ctrl+R retries."));
    }
    let out = root.join("git-output");
    let err = root.join("git-error");
    let stdout = fs::File::create(&out)?;
    let stderr = fs::File::create(&err)?;
    private(&out)?;
    private(&err)?;
    command.stdout(stdout).stderr(stderr);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let mut child = command.spawn()?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return fs::read(&out);
            }
            let message = fs::read_to_string(&err).unwrap_or_default();
            return Err(io::Error::other(if message.trim().is_empty() {
                "Git/GitHub command failed.".into()
            } else {
                message.chars().take(2000).collect::<String>()
            }));
        }
        if cancelled.load(Ordering::Relaxed) || started.elapsed() > Duration::from_secs(120) {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other(
                "Git operation stopped or timed out; Ctrl+R retries.",
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn git(root: &Path, path: &Path, args: &[&str], cancelled: &AtomicBool) -> io::Result<String> {
    let mut command = safe_git(path)?;
    command.arg("-C").arg(path).args(args);
    String::from_utf8(run(command, root, cancelled)?)
        .map(|text| text.trim_end().to_owned())
        .map_err(io::Error::other)
}

fn safe_git(path: &Path) -> io::Result<Command> {
    let filters = workspace::trusted_git()
        .arg("-C")
        .arg(path)
        .args([
            "config",
            "--name-only",
            "--get-regexp",
            "^filter\\..*\\.(clean|smudge|process|required)$",
        ])
        .output()?;
    let mut command = workspace::trusted_git();
    command.args(["-c", "core.fsmonitor=false"]);
    for name in String::from_utf8_lossy(&filters.stdout).lines() {
        command.arg("-c").arg(format!(
            "{name}={}",
            if name.ends_with(".required") {
                "false"
            } else {
                ""
            }
        ));
    }
    Ok(command)
}

pub fn prepare(project: &Path, root: &Path, cancelled: &AtomicBool) -> io::Result<()> {
    fs::create_dir_all(root)?;
    private(root)?;
    let mut state = if root.join("git-mod.json").exists() {
        load(root)?
    } else {
        let repo = project_root(project)
            .ok_or_else(|| io::Error::other("This project needs a Git repository."))?;
        let base = git(
            root,
            &repo,
            &["rev-parse", "--verify", "HEAD^{commit}"],
            cancelled,
        )
        .map_err(|_| {
            io::Error::other("Create an initial Git commit before creating a code mod.")
        })?;
        let base_branch = git(
            root,
            &repo,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            cancelled,
        )
        .ok();
        let suffix = root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("Invalid mod folder."))?;
        let state = GitMod {
            repo,
            branch: format!("sprowt/mod-{suffix}"),
            base,
            base_branch,
            phase: "preparing".into(),
            fingerprint: None,
            head: None,
            pr: None,
            draft: false,
            removing: false,
            publishing: false,
        };
        save(root, &state)?;
        state
    };
    if state.phase == "cleaned" || (state.pr.is_some() && !checkout(root).exists()) {
        return Ok(());
    }
    if state.phase != "preparing" {
        return validate(root, &state, cancelled);
    }
    let path = checkout(root);
    let tree_types = git(
        root,
        &state.repo,
        &["ls-tree", "-r", &state.base],
        cancelled,
    )?;
    if tree_types
        .lines()
        .any(|line| !line.starts_with("100644 blob ") && !line.starts_with("100755 blob "))
    {
        return Err(io::Error::other(
            "Worktree setup requires regular files; links and submodules are unsupported.",
        ));
    }
    if !path.join(".git").is_file() {
        let existing = git(
            root,
            &state.repo,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/heads/{}", state.branch),
            ],
            cancelled,
        )
        .ok();
        if existing.as_ref().is_some_and(|head| *head != state.base) {
            return Err(io::Error::other(
                "Generated branch changed during setup; work is retained.",
            ));
        }
        let mut command = workspace::trusted_git();
        command
            .arg("-C")
            .arg(&state.repo)
            .args(["worktree", "add", "--no-checkout"]);
        if existing.is_none() {
            command.args(["-b", &state.branch]);
        }
        command.arg(&path).arg(if existing.is_some() {
            &state.branch
        } else {
            &state.base
        });
        run(command, root, cancelled)?;
    }
    validate(root, &state, cancelled)?;
    // Materialize blobs without running project Git filters on the host.
    let mut command = workspace::trusted_git();
    command
        .arg("-C")
        .arg(&path)
        .args(["ls-tree", "-r", "-z", &state.base]);
    let tree = run(command, root, cancelled)?;
    let mut files = Vec::new();
    for entry in tree
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let tab = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| io::Error::other("Invalid Git tree."))?;
        let metadata = std::str::from_utf8(&entry[..tab])
            .map_err(io::Error::other)?
            .split_whitespace()
            .collect::<Vec<_>>();
        if metadata.len() != 3 || !["100644", "100755"].contains(&metadata[0]) {
            return Err(io::Error::other(
                "Worktree setup requires regular files; links and submodules are unsupported.",
            ));
        }
        let relative =
            PathBuf::from(std::str::from_utf8(&entry[tab + 1..]).map_err(io::Error::other)?);
        if relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
            || relative.starts_with(".git")
        {
            return Err(io::Error::other("Invalid Git source path."));
        }
        files.push((relative, metadata[2].to_owned(), metadata[0] == "100755"));
    }
    git(root, &path, &["read-tree", &state.base], cancelled)?;
    let objects = root.join("git-objects");
    fs::write(
        &objects,
        files
            .iter()
            .map(|(_, hash, _)| format!("{hash}\n"))
            .collect::<String>(),
    )?;
    private(&objects)?;
    let mut command = workspace::trusted_git();
    command
        .arg("-C")
        .arg(&path)
        .args(["cat-file", "--batch"])
        .stdin(fs::File::open(&objects)?);
    let blobs = run(command, root, cancelled)?;
    let mut remaining = blobs.as_slice();
    for (relative, _hash, executable) in files {
        if cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::other("Worktree setup paused; Ctrl+R retries."));
        }
        let target = path.join(relative);
        let end = remaining
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or_else(|| io::Error::other("Invalid Git blob response."))?;
        let size = std::str::from_utf8(&remaining[..end])
            .map_err(io::Error::other)?
            .split_whitespace()
            .nth(2)
            .ok_or_else(|| io::Error::other("Missing Git blob size."))?
            .parse::<usize>()
            .map_err(io::Error::other)?;
        remaining = &remaining[end + 1..];
        if remaining.len() <= size {
            return Err(io::Error::other("Truncated Git blob response."));
        }
        let bytes = &remaining[..size];
        remaining = &remaining[size + 1..];
        if target.exists() {
            if fs::symlink_metadata(&target)?.file_type().is_symlink()
                || fs::read(&target)? != bytes
            {
                return Err(io::Error::other(
                    "Worktree setup found changed files; existing work is retained.",
                ));
            }
        } else {
            fs::create_dir_all(target.parent().unwrap())?;
            fs::write(&target, bytes)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    &target,
                    fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
                )?;
            }
        }
    }
    state.phase = "ready".into();
    save(root, &state)
}

fn validate(root: &Path, state: &GitMod, cancelled: &AtomicBool) -> io::Result<()> {
    let path = checkout(root);
    if fs::symlink_metadata(&path)?.file_type().is_symlink()
        || git(
            root,
            &path,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            cancelled,
        )? != state.branch
    {
        return Err(io::Error::other(
            "The mod worktree or branch changed; work is retained.",
        ));
    }
    let common = git(
        root,
        &path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        cancelled,
    )?;
    let expected = git(
        root,
        &state.repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        cancelled,
    )?;
    if Path::new(&common).canonicalize()? != Path::new(&expected).canonicalize()? {
        return Err(io::Error::other(
            "The mod worktree belongs to a different repository.",
        ));
    }
    Ok(())
}

fn stage(root: &Path, path: &Path, review: &Review, cancelled: &AtomicBool) -> io::Result<()> {
    for relative in review.paths() {
        let name = relative
            .to_str()
            .ok_or_else(|| io::Error::other("Invalid source filename."))?;
        let file = path.join(relative);
        if file.is_file() {
            let mut command = workspace::trusted_git();
            command
                .arg("-C")
                .arg(path)
                .args(["hash-object", "--no-filters", "-w", "--"])
                .arg(&file);
            let hash =
                String::from_utf8(run(command, root, cancelled)?).map_err(io::Error::other)?;
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                fs::metadata(&file)?.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            git(
                root,
                path,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    if executable { "100755" } else { "100644" },
                    hash.trim(),
                    name,
                ],
                cancelled,
            )?;
        } else {
            git(
                root,
                path,
                &["update-index", "--force-remove", "--", name],
                cancelled,
            )?;
        }
    }
    Ok(())
}

fn gh(
    root: &Path,
    state: &GitMod,
    program: &Path,
    args: &[&str],
    cancelled: &AtomicBool,
) -> io::Result<String> {
    let mut command = Command::new(program);
    command
        .current_dir(&state.repo)
        .stdin(Stdio::null())
        .env_remove("GH_REPO")
        .env("GH_PROMPT_DISABLED", "1")
        .args(args);
    String::from_utf8(run(command, root, cancelled)?)
        .map(|text| text.trim().to_owned())
        .map_err(io::Error::other)
}

pub fn publish(
    root: &Path,
    description: &str,
    draft: bool,
    program: &Path,
    cancelled: &AtomicBool,
) -> io::Result<String> {
    let mut state = load(root)?;
    if let Some(pr) = &state.pr {
        return Ok(pr.clone());
    }
    validate(root, &state, cancelled)?;
    let path = checkout(root);
    let review = workspace::review(root)?;
    if review.count() == 0 {
        return Err(io::Error::other("No source changes to publish."));
    }
    let head = git(root, &path, &["rev-parse", "HEAD"], cancelled)?;
    if state.phase == "ready" {
        if head != state.base
            || !git(root, &path, &["status", "--porcelain"], cancelled)?.is_empty()
        {
            return Err(io::Error::other(
                "The mod worktree changed outside the harness; reconcile it before publishing.",
            ));
        }
        state.phase = "exporting".into();
        state.fingerprint = Some(review.fingerprint.clone());
        state.draft = draft;
        save(root, &state)?;
    }
    if state.fingerprint.as_deref() != Some(&review.fingerprint) {
        return Err(io::Error::other(
            "Source changed during publication; saved work is retained.",
        ));
    }
    if state.phase == "exporting" {
        if head == state.base {
            review.export(&path, root)?;
            stage(root, &path, &review, cancelled)?;
            let title = description
                .lines()
                .next()
                .unwrap_or("Code mod")
                .chars()
                .take(120)
                .collect::<String>();
            git(root, &path, &["commit", "-m", &title], cancelled)?;
        }
        state.head = Some(git(root, &path, &["rev-parse", "HEAD"], cancelled)?);
        state.phase = "committed".into();
        save(root, &state)?;
    }
    if state.head.as_deref() != Some(&git(root, &path, &["rev-parse", "HEAD"], cancelled)?)
        || !git(root, &path, &["status", "--porcelain"], cancelled)?.is_empty()
    {
        return Err(io::Error::other(
            "Published worktree changed; source is retained.",
        ));
    }
    if workspace::fingerprint(&workspace::source_state(&path)?)? != review.fingerprint {
        return Err(io::Error::other(
            "Mod source differs from the reviewed result; publication paused.",
        ));
    }
    let changed = git(
        root,
        &path,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--name-only",
            "-z",
            &state.base,
            "HEAD",
        ],
        cancelled,
    )?;
    let mut actual = changed
        .split('\0')
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    actual.sort_unstable();
    let mut expected = review
        .paths()
        .map(|path| path.to_str().unwrap())
        .collect::<Vec<_>>();
    expected.sort_unstable();
    if actual != expected {
        return Err(io::Error::other(
            "Mod commit contains changes outside the reviewed diff.",
        ));
    }
    let origin = git(
        root,
        &state.repo,
        &["remote", "get-url", "origin"],
        cancelled,
    )?;
    let slug = origin
        .strip_prefix("https://github.com/")
        .or_else(|| origin.strip_prefix("git@github.com:"))
        .or_else(|| origin.strip_prefix("ssh://git@github.com/"))
        .map(|name| name.trim_end_matches(".git"))
        .filter(|name| {
            name.split('/').count() == 2
                && name.split('/').all(|part| {
                    !part.is_empty()
                        && part
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                })
        })
        .ok_or_else(|| io::Error::other("PR publication needs a github.com origin remote."))?;
    let repository: serde_json::Value = serde_json::from_str(&gh(
        root,
        &state,
        program,
        &[
            "repo",
            "view",
            slug,
            "--json",
            "nameWithOwner,defaultBranchRef",
        ],
        cancelled,
    )?)?;
    let repo = repository["nameWithOwner"]
        .as_str()
        .ok_or_else(|| io::Error::other("Cannot identify the project GitHub repository."))?;
    let base = state
        .base_branch
        .as_deref()
        .or_else(|| repository["defaultBranchRef"]["name"].as_str())
        .ok_or_else(|| io::Error::other("Choose a PR base branch."))?;
    let prs: serde_json::Value = serde_json::from_str(&gh(
        root,
        &state,
        program,
        &[
            "pr",
            "list",
            "--repo",
            repo,
            "--head",
            &state.branch,
            "--base",
            base,
            "--state",
            "all",
            "--json",
            "url,headRefOid",
        ],
        cancelled,
    )?)?;
    let existing = prs
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| item["url"].as_str())
        .map(str::to_owned);
    if existing.is_some() && prs[0]["headRefOid"].as_str() != state.head.as_deref() {
        return Err(io::Error::other(
            "Existing PR has different commits; work is retained.",
        ));
    }
    if existing.is_none() {
        let destination = format!("HEAD:refs/heads/{}", state.branch);
        let mut command = Command::new("git");
        command
            .stdin(Stdio::null())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["-c", "core.hooksPath=/dev/null", "-C"])
            .arg(&path)
            .args(["push", "origin", &destination]);
        run(command, root, cancelled)?;
        state.phase = "pushed".into();
        save(root, &state)?;
    }
    let pr = if let Some(url) = existing {
        url
    } else {
        let body = root.join("pr-body.md");
        fs::write(
            &body,
            format!(
                "## goal\n\n{description}\n\n## verification\n\n{}\n",
                if state.draft {
                    "Work in progress. Review the diff and saved check results."
                } else {
                    "Plan tasks and the combined checks passed in the mod’s Linux VM."
                }
            ),
        )?;
        private(&body)?;
        let title = description
            .lines()
            .next()
            .unwrap_or("Code mod")
            .chars()
            .take(120)
            .collect::<String>();
        let mut args = vec![
            "pr",
            "create",
            "--repo",
            repo,
            "--base",
            base,
            "--head",
            &state.branch,
            "--title",
            &title,
            "--body-file",
            body.to_str().unwrap(),
        ];
        if state.draft {
            args.push("--draft");
        }
        gh(root, &state, program, &args, cancelled)?
    };
    if !pr.starts_with("https://") {
        return Err(io::Error::other(
            "GitHub did not return a PR URL; retry publication.",
        ));
    }
    state.pr = Some(pr.clone());
    state.publishing = false;
    state.phase = "published".into();
    save(root, &state)?;
    Ok(pr)
}

pub fn cleanup(root: &Path, cancelled: &AtomicBool) -> io::Result<()> {
    let mut state = load(root)?;
    if state.phase == "cleaned" {
        return Ok(());
    }
    if state.pr.is_none() && !["ready", "preparing"].contains(&state.phase.as_str()) {
        return Err(io::Error::other("Publish saved changes before cleanup."));
    }
    let path = checkout(root);
    if state.pr.is_none() && root.join("work").exists() && workspace::review(root)?.count() > 0 {
        return Err(io::Error::other(
            "The mod has source changes to preserve before cleanup.",
        ));
    }
    if path.exists() {
        validate(root, &state, cancelled)?;
        if !git(root, &path, &["status", "--porcelain"], cancelled)?.is_empty() {
            return Err(io::Error::other(
                "Worktree has unsaved changes; cleanup paused.",
            ));
        }
        if state.pr.is_none() && git(root, &path, &["rev-parse", "HEAD"], cancelled)? != state.base
        {
            return Err(io::Error::other(
                "Worktree has unpublished commits; cleanup paused.",
            ));
        }
    }
    crate::sandbox::delete(root)?;
    if path.exists() {
        let mut command = safe_git(&state.repo)?;
        command
            .arg("-C")
            .arg(&state.repo)
            .args(["worktree", "remove"])
            .arg(&path);
        run(command, root, cancelled)?;
    }
    if state.pr.is_none()
        && git(
            root,
            &state.repo,
            &[
                "show-ref",
                "--verify",
                &format!("refs/heads/{}", state.branch),
            ],
            cancelled,
        )
        .is_ok()
    {
        git(
            root,
            &state.repo,
            &["branch", "-d", &state.branch],
            cancelled,
        )?;
    }
    state.phase = "cleaned".into();
    save(root, &state)
}

pub fn mark_removing(root: &Path) -> io::Result<()> {
    let mut state = load(root)?;
    state.removing = true;
    save(root, &state)
}

pub fn mark_publishing(root: &Path) -> io::Result<()> {
    let mut state = load(root)?;
    state.publishing = true;
    save(root, &state)
}

pub fn remove_files(root: &Path, cancelled: &AtomicBool) -> io::Result<()> {
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_name() == "git-mod.json" {
            continue;
        }
        if cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::other("Mod removal paused; Ctrl+R retries."));
        }
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::test_support::TestData;

    fn fixture() -> (TestData, PathBuf, PathBuf, PathBuf) {
        let data = TestData::new();
        fs::create_dir_all(&data.0).unwrap();
        let root = data.0.join("workspaces").join(data.0.file_name().unwrap());
        fs::create_dir_all(&root).unwrap();
        let repo = data.0.join("repo");
        fs::create_dir(&repo).unwrap();
        let flag = AtomicBool::new(false);
        git(&root, &repo, &["init", "--initial-branch", "main"], &flag).unwrap();
        fs::write(repo.join("a.txt"), "original\n").unwrap();
        fs::write(repo.join("delete.txt"), "delete me\n").unwrap();
        fs::write(repo.join(".env"), "fixture-secret\n").unwrap();
        fs::write(repo.join(".gitattributes"), "*.txt filter=fixture\n").unwrap();
        git(&root, &repo, &["add", "."], &flag).unwrap();
        git(&root, &repo, &["commit", "-m", "Baseline"], &flag).unwrap();
        let bare = data.0.join("remote.git");
        fs::create_dir(&bare).unwrap();
        git(&root, &bare, &["init", "--bare"], &flag).unwrap();
        git(
            &root,
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/fixture/project.git",
            ],
            &flag,
        )
        .unwrap();
        git(
            &root,
            &repo,
            &["config", "remote.origin.pushurl", bare.to_str().unwrap()],
            &flag,
        )
        .unwrap();
        let program = data.0.join("fake-gh");
        let pr = data.0.join("pr-created");
        let failed = data.0.join("fail-once");
        fs::write(&program, format!(r#"#!/bin/sh
case "$1 $2" in
  "repo view") printf '%s\n' '{{"nameWithOwner":"fixture/project","defaultBranchRef":{{"name":"main"}}}}' ;;
  "pr list") if [ -f '{pr}' ]; then printf '[{{"url":"https://github.com/fixture/project/pull/1","headRefOid":"%s"}}]\n' "$(cat '{pr}')"; else printf '%s\n' '[]'; fi ;;
  "pr create")
    printf '%s\n' "$@" >> '{log}'
    git -C '{checkout}' rev-parse HEAD > '{pr}'
    if [ -f '{failed}' ]; then rm '{failed}'; printf '%s\n' 'connection interrupted after PR creation' >&2; exit 1; fi
    printf '%s\n' 'https://github.com/fixture/project/pull/1' ;;
  *) exit 1 ;;
esac
"#, pr=pr.display(), failed=failed.display(), log=data.0.join("gh-args").display(), checkout=checkout(&root).display())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        }
        (data, repo, root, program)
    }

    fn edited(root: &Path) {
        workspace::create(&checkout(root), root).unwrap();
        fs::write(root.join("work/a.txt"), "changed\n").unwrap();
        fs::write(root.join("work/new.txt"), "new\n").unwrap();
        fs::remove_file(root.join("work/delete.txt")).unwrap();
    }

    #[test]
    fn worktree_uses_committed_source_and_never_runs_host_git_filters() {
        let (data, repo, root, _) = fixture();
        let flag = AtomicBool::new(false);
        let marker = data.0.join("filter-ran");
        let filter = format!("touch '{}'; cat", marker.display());
        git(
            &root,
            &repo,
            &["config", "filter.fixture.clean", &filter],
            &flag,
        )
        .unwrap();
        git(
            &root,
            &repo,
            &["config", "filter.fixture.smudge", &filter],
            &flag,
        )
        .unwrap();
        fs::write(repo.join("a.txt"), "uncommitted\n").unwrap();
        fs::write(repo.join("untracked.txt"), "local\n").unwrap();
        prepare(&repo, &root, &flag).unwrap();
        assert_eq!(
            fs::read_to_string(checkout(&root).join("a.txt")).unwrap(),
            "original\n"
        );
        assert!(!checkout(&root).join("untracked.txt").exists());
        workspace::create(&checkout(&root), &root).unwrap();
        assert!(!root.join("work/.env").exists());
        assert!(!root.join("work/.git").exists());
        assert_eq!(
            fs::read_to_string(checkout(&root).join(".env")).unwrap(),
            "fixture-secret\n"
        );
        assert!(!marker.exists());
        prepare(&repo, &root, &flag).unwrap();
        cleanup(&root, &flag).unwrap();
        assert!(!marker.exists());
        assert_eq!(
            fs::read_to_string(repo.join("a.txt")).unwrap(),
            "uncommitted\n"
        );
    }

    #[test]
    fn unknown_pr_outcome_retries_without_duplicate_pr_or_commits() {
        let (data, repo, root, program) = fixture();
        let flag = AtomicBool::new(false);
        prepare(&repo, &root, &flag).unwrap();
        let baseline = load(&root).unwrap().base;
        edited(&root);
        mark_publishing(&root).unwrap();
        fs::write(data.0.join("fail-once"), "").unwrap();
        assert!(publish(&root, "A small change", false, &program, &flag).is_err());
        assert_eq!(load(&root).unwrap().phase, "pushed");
        let commit = load(&root).unwrap().head.unwrap();
        assert_eq!(
            git(&root, &repo, &["rev-parse", "HEAD"], &flag).unwrap(),
            baseline
        );
        let url = publish(&root, "A small change", false, &program, &flag).unwrap();
        assert_eq!(url, "https://github.com/fixture/project/pull/1");
        assert_eq!(load(&root).unwrap().head.as_deref(), Some(commit.as_str()));
        assert_eq!(
            fs::read_to_string(data.0.join("gh-args"))
                .unwrap()
                .lines()
                .filter(|line| *line == "create")
                .count(),
            1
        );
        let branch = load(&root).unwrap().branch;
        assert_eq!(
            git(
                &root,
                &data.0.join("remote.git"),
                &["rev-parse", &format!("refs/heads/{branch}")],
                &flag
            )
            .unwrap(),
            commit
        );
        cleanup(&root, &flag).unwrap();
        cleanup(&root, &flag).unwrap();
        assert!(!checkout(&root).exists());
        assert_eq!(
            git(&root, &repo, &["rev-parse", &branch], &flag).unwrap(),
            commit
        );
        assert!(root.join("work/new.txt").exists());
        assert_eq!(
            fs::read_to_string(repo.join("a.txt")).unwrap(),
            "original\n"
        );
    }

    #[test]
    fn unfinished_changes_publish_as_draft_before_cleanup() {
        let (data, repo, root, program) = fixture();
        let flag = AtomicBool::new(false);
        prepare(&repo, &root, &flag).unwrap();
        edited(&root);
        mark_removing(&root).unwrap();
        assert!(cleanup(&root, &flag).is_err());
        publish(&root, "Partial work", true, &program, &flag).unwrap();
        assert!(
            fs::read_to_string(data.0.join("gh-args"))
                .unwrap()
                .contains("--draft")
        );
        cleanup(&root, &flag).unwrap();
        assert!(load(&root).unwrap().removing);
        assert!(load(&root).unwrap().pr.is_some());
    }

    #[test]
    #[ignore = "runs a temporary Apple Container VM through worktree export and PR cleanup"]
    fn vm_source_becomes_a_pr_and_cleanup_removes_the_vm_and_worktree() {
        let (_data, repo, root, program) = fixture();
        let flag = AtomicBool::new(false);
        prepare(&repo, &root, &flag).unwrap();
        workspace::create(&checkout(&root), &root).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> io::Result<()> {
                let mut vm =
                    crate::sandbox::Sandbox::prepare(&root, &flag, |label| eprintln!("{label}"))?;
                let check = crate::execution::Check {
                check: "Source copy and guest edits".into(),
                    command: vec!["/bin/sh".into(), "-c".into(), "test ! -f .git && test ! -e .git/HEAD && test ! -e .env && test \"$(cat a.txt)\" = original && printf 'VM edit\\n' > a.txt && printf 'VM new\\n' > guest.txt && rm delete.txt".into()],
            };
                let (_, checks) = vm.verify(&[check], &flag)?;
                assert_eq!(checks[0].exit_code, Some(0), "{}", checks[0].output);
                vm.export(&flag)?;
                drop(vm);
                assert_eq!(fs::read_to_string(repo.join("a.txt"))?, "original\n");
                assert_eq!(fs::read_to_string(root.join("work/a.txt"))?, "VM edit\n");
                publish(&root, "Change in VM", false, &program, &flag)?;
                cleanup(&root, &flag)?;
                assert!(!root.join("vm.json").exists() && !checkout(&root).exists());
                assert_eq!(load(&root)?.phase, "cleaned");
                assert!(root.join("work/guest.txt").exists());
                let output = Command::new("container")
                    .args(["list", "--all", "--format", "json"])
                    .output()?;
                assert!(output.status.success());
                let containers: serde_json::Value = serde_json::from_slice(&output.stdout)?;
                let name = format!("sprowt-{}", root.file_name().unwrap().to_str().unwrap());
                assert!(
                    !containers
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|vm| vm["id"] == name)
                );
                Ok(())
            },
        ));
        crate::sandbox::delete(&root).unwrap();
        result.unwrap().unwrap();
    }

    #[test]
    fn reviewed_source_changes_block_a_pending_publication_retry() {
        let (_data, repo, root, program) = fixture();
        let flag = AtomicBool::new(false);
        prepare(&repo, &root, &flag).unwrap();
        edited(&root);
        let mut state = load(&root).unwrap();
        state.phase = "exporting".into();
        state.fingerprint = Some(workspace::review(&root).unwrap().fingerprint);
        save(&root, &state).unwrap();
        fs::write(root.join("work/a.txt"), "later edit\n").unwrap();
        assert!(
            publish(&root, "Changed review", false, &program, &flag)
                .unwrap_err()
                .to_string()
                .contains("Source changed")
        );
        assert_eq!(
            git(&root, &checkout(&root), &["rev-parse", "HEAD"], &flag).unwrap(),
            state.base
        );
        assert!(checkout(&root).exists());
    }

    #[test]
    fn setup_recovers_an_existing_generated_branch_and_partial_checkout() {
        let (_data, repo, root, _) = fixture();
        let flag = AtomicBool::new(false);
        prepare(&repo, &root, &flag).unwrap();
        let mut state = load(&root).unwrap();
        git(
            &root,
            &repo,
            &["worktree", "remove", checkout(&root).to_str().unwrap()],
            &flag,
        )
        .unwrap();
        state.phase = "preparing".into();
        save(&root, &state).unwrap();
        prepare(&repo, &root, &flag).unwrap();
        fs::remove_file(checkout(&root).join("a.txt")).unwrap();
        save(&root, &state).unwrap();
        prepare(&repo, &root, &flag).unwrap();
        assert_eq!(
            fs::read_to_string(checkout(&root).join("a.txt")).unwrap(),
            "original\n"
        );
        cleanup(&root, &flag).unwrap();
    }

    #[test]
    fn partial_export_and_commit_checkpoint_recover_without_losing_source() {
        let (_data, repo, root, program) = fixture();
        let flag = AtomicBool::new(false);
        prepare(&repo, &root, &flag).unwrap();
        edited(&root);
        let review = workspace::review(&root).unwrap();
        let mut state = load(&root).unwrap();
        state.phase = "exporting".into();
        state.fingerprint = Some(review.fingerprint.clone());
        save(&root, &state).unwrap();
        fs::write(checkout(&root).join("a.txt"), "changed\n").unwrap();
        publish(&root, "Recover partial export", false, &program, &flag).unwrap();
        state = load(&root).unwrap();
        state.pr = None;
        state.phase = "exporting".into();
        state.head = None;
        save(&root, &state).unwrap();
        let head = git(&root, &checkout(&root), &["rev-parse", "HEAD"], &flag).unwrap();
        publish(&root, "Recover commit checkpoint", false, &program, &flag).unwrap();
        assert_eq!(load(&root).unwrap().head.as_deref(), Some(head.as_str()));
        cleanup(&root, &flag).unwrap();
    }

    #[test]
    fn cancelling_background_commands_is_bounded() {
        let data = TestData::new();
        fs::create_dir_all(&data.0).unwrap();
        let root = data.0.clone();
        let job = Job::start("waiting", move |cancelled| {
            let mut command = Command::new("/bin/sleep");
            command.arg("30");
            run(command, &root, cancelled)?;
            Ok(Result::Prepared)
        });
        thread::sleep(Duration::from_millis(100));
        let started = Instant::now();
        drop(job);
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
