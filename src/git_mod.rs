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
    #[serde(default)]
    pub closing: bool,
    #[serde(default)]
    pub discarding: bool,
    #[serde(default)]
    pub continuing: bool,
    #[serde(default)]
    pub published_head: Option<String>,
    #[serde(default)]
    pub checkpoint: Option<String>,
}

impl GitMod {
    pub fn published(&self) -> bool {
        self.pr.is_some()
            && !self.continuing
            && matches!(self.phase.as_str(), "published" | "cleaned")
    }
}

pub enum Result {
    Prepared,
    Refreshed,
    Adopted,
    RepositoryConnected,
    Snapshot,
    Reviewed(Review),
    Published,
    Continued,
    Reopened,
    Pruned,
    Closed,
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

pub fn has_commit(project: &Path) -> bool {
    workspace::trusted_git()
        .arg("-C")
        .arg(project)
        .args(["rev-parse", "--verify", "HEAD^{commit}"])
        .output()
        .is_ok_and(|output| output.status.success())
}

pub fn has_origin(project: &Path) -> bool {
    workspace::trusted_git()
        .arg("-C")
        .arg(project)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .is_ok_and(|output| output.status.success())
}

pub fn initialize(project: &Path, root: &Path, cancelled: &AtomicBool) -> io::Result<()> {
    let project_path = project.canonicalize()?;
    if project_root(project).is_some_and(|repo| repo != project_path) {
        return Err(io::Error::other(
            "The project now belongs to a different repository. Reopen from its root.",
        ));
    }
    if has_commit(project) {
        return Ok(());
    }
    if !root.join("base.git").exists() {
        return Err(io::Error::other(
            "Review the starting files before Git setup.",
        ));
    }
    if project_root(project).is_none() {
        git(
            root,
            project,
            &["init", "--initial-branch", "main"],
            cancelled,
        )?;
    }
    let index = root.join("initial-index");
    let indexed = |args: &[&str]| -> io::Result<String> {
        let mut command = safe_git(project)?;
        command
            .env("GIT_INDEX_FILE", &index)
            .arg("-C")
            .arg(project)
            .args(args);
        String::from_utf8(run(command, root, cancelled)?)
            .map(|text| text.trim().to_owned())
            .map_err(io::Error::other)
    };
    indexed(&["read-tree", "--empty"])?;
    private(&index)?;
    for (path, _, mode) in workspace::source_state(&root.join("before"))? {
        let file = root.join("before").join(&path);
        let hash = git(
            root,
            project,
            &[
                "hash-object",
                "--no-filters",
                "-w",
                "--",
                file.to_str().unwrap(),
            ],
            cancelled,
        )?;
        indexed(&[
            "update-index",
            "--add",
            "--cacheinfo",
            if mode & 0o111 != 0 {
                "100755"
            } else {
                "100644"
            },
            &hash,
            path.to_str().unwrap(),
        ])?;
    }
    let tree = indexed(&["write-tree"])?;
    let base = git(
        root,
        project,
        &["commit-tree", &tree, "-m", "Initial project"],
        cancelled,
    )?;
    let branch = git(root, project, &["symbolic-ref", "HEAD"], cancelled)?;
    git(
        root,
        project,
        &["update-ref", &branch, &base, ""],
        cancelled,
    )?;
    git(root, project, &["read-tree", &base], cancelled)?;
    fs::remove_file(index)?;
    Ok(())
}

pub fn adopt(project: &Path, root: &Path, cancelled: &AtomicBool) -> io::Result<()> {
    initialize(project, root, cancelled)?;
    prepare(project, root, cancelled)?;
    let baseline = workspace::fingerprint(&workspace::source_state(&root.join("before"))?)?;
    if workspace::fingerprint(&workspace::source_state(&checkout(root))?)? != baseline {
        return Err(io::Error::other(
            "The repository baseline differs from this codemod's saved starting files. Work is retained.",
        ));
    }
    fs::write(root.join("snapshot-ready"), b"ready")?;
    if root.join("project-setup").exists() {
        fs::remove_file(root.join("project-setup"))?;
    }
    Ok(())
}

pub fn setup_pending(root: &Path) -> Option<bool> {
    fs::read_to_string(root.join("project-setup"))
        .ok()
        .map(|kind| kind == "saved")
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

pub(crate) fn run(
    mut command: Command,
    root: &Path,
    cancelled: &AtomicBool,
) -> io::Result<Vec<u8>> {
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

pub(crate) fn git(
    root: &Path,
    path: &Path,
    args: &[&str],
    cancelled: &AtomicBool,
) -> io::Result<String> {
    let mut command = safe_git(path)?;
    command.arg("-C").arg(path).args(args);
    String::from_utf8(run(command, root, cancelled)?)
        .map(|text| text.trim_end().to_owned())
        .map_err(io::Error::other)
}

pub(crate) fn safe_git(path: &Path) -> io::Result<Command> {
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
    let mut command = workspace::exact_git(path)?;
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
        .map_err(|_| io::Error::other("Create an initial Git commit before creating a codemod."))?;
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
            closing: false,
            discarding: false,
            continuing: false,
            published_head: None,
            checkpoint: None,
        };
        save(root, &state)?;
        state
    };
    if state.phase == "cleaned" || (state.published() && !checkout(root).exists()) {
        return Ok(());
    }
    if state.phase != "preparing" {
        return validate(root, &state, cancelled);
    }
    let start = state
        .checkpoint
        .as_ref()
        .or(state.head.as_ref())
        .unwrap_or(&state.base)
        .clone();
    let path = checkout(root);
    let tree_types = git(root, &state.repo, &["ls-tree", "-r", &start], cancelled)?;
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
        if existing.as_ref().is_some_and(|head| *head != start) {
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
            &start
        });
        run(command, root, cancelled)?;
    }
    validate(root, &state, cancelled)?;
    // Materialize blobs without running project Git filters on the host.
    let mut command = workspace::trusted_git();
    command
        .arg("-C")
        .arg(&path)
        .args(["ls-tree", "-r", "-z", &start]);
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
    git(root, &path, &["read-tree", &start], cancelled)?;
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

pub fn refresh(project: &Path, root: &Path, cancelled: &AtomicBool) -> io::Result<()> {
    let mut state = load(root)?;
    if state.phase != "ready"
        || state.head.is_some()
        || state.pr.is_some()
        || state.checkpoint.is_some()
        || root.join("work").exists()
        || root.join("snapshot-ready").exists()
        || root.join("vm.json").exists()
    {
        return Err(io::Error::other(
            "Only an unstarted codemod can refresh its starting source.",
        ));
    }
    validate(root, &state, cancelled)?;
    let marker = root.join("refresh-target");
    let path = checkout(root);
    let target = if marker.exists() {
        fs::read_to_string(&marker)?
    } else {
        if git(root, &path, &["rev-parse", "HEAD"], cancelled)? != state.base
            || !git(
                root,
                &path,
                &[
                    "status",
                    "--porcelain",
                    "--untracked-files=all",
                    "--ignored",
                ],
                cancelled,
            )?
            .is_empty()
        {
            return Err(io::Error::other(
                "The codemod worktree has outside changes. Its source is retained.",
            ));
        }
        let branch = git(
            root,
            project,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            cancelled,
        )
        .ok();
        if branch != state.base_branch {
            return Err(io::Error::other(
                "The project branch changed. Start a new codemod from that branch.",
            ));
        }
        crate::git_sync::project(project, root, cancelled)?;
        let target = git(root, project, &["rev-parse", "HEAD^{commit}"], cancelled)?;
        if target == state.base {
            return Ok(());
        }
        if git(
            root,
            project,
            &["merge-base", "--is-ancestor", &state.base, &target],
            cancelled,
        )
        .is_err()
        {
            return Err(io::Error::other(
                "The project's history changed. Start a new codemod; saved work is retained.",
            ));
        }
        fs::write(&marker, &target)?;
        target
    };
    let head = git(root, &path, &["rev-parse", "HEAD"], cancelled)?;
    if head != state.base && head != target {
        return Err(io::Error::other(
            "The codemod branch changed during refresh. Source is retained.",
        ));
    }
    git(
        root,
        &path,
        &["read-tree", "-m", "-u", &state.base, &target],
        cancelled,
    )?;
    if head != target {
        git(
            root,
            &path,
            &["update-ref", "HEAD", &target, &state.base],
            cancelled,
        )?;
    }
    state.base = target;
    save(root, &state)?;
    fs::remove_file(marker)
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

#[derive(Clone, Deserialize, Serialize)]
pub struct RepositoryRequest {
    pub slug: String,
    pub create: bool,
}

#[derive(Deserialize, Serialize)]
struct RepositorySetup {
    request: RepositoryRequest,
    created: bool,
    complete: bool,
}

pub fn repository_pending(root: &Path) -> Option<RepositoryRequest> {
    let setup: RepositorySetup =
        serde_json::from_slice(&fs::read(root.join("repository-setup.json")).ok()?).ok()?;
    (!setup.complete).then_some(setup.request)
}

pub fn valid_slug(slug: &str) -> bool {
    slug.split('/').count() == 2
        && slug.split('/').all(|part| {
            !part.is_empty()
                && ![".", ".."].contains(&part)
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
}

pub fn connect_repository(
    root: &Path,
    request: RepositoryRequest,
    program: &Path,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    if !valid_slug(&request.slug) {
        return Err(io::Error::other(
            "Use owner/repository, for example minimumeffort-dev/todo.",
        ));
    }
    let state = load(root)?;
    let checkpoint = root.join("repository-setup.json");
    let mut setup: RepositorySetup = fs::read(&checkpoint)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(|saved: &RepositorySetup| {
            saved.request.slug == request.slug && saved.request.create == request.create
        })
        .unwrap_or(RepositorySetup {
            request,
            created: false,
            complete: false,
        });
    let url = format!("https://github.com/{}.git", setup.request.slug);
    if has_origin(&state.repo)
        && git(
            root,
            &state.repo,
            &["config", "--get", "remote.origin.url"],
            cancelled,
        )? != url
    {
        return Err(io::Error::other(
            "This project already has a different origin remote. Work is retained.",
        ));
    }
    fs::write(&checkpoint, serde_json::to_vec(&setup)?)?;
    private(&checkpoint)?;
    if setup.request.create && !setup.created {
        gh(
            root,
            &state,
            program,
            &["repo", "create", &setup.request.slug, "--private"],
            cancelled,
        )
        .map_err(|error| {
            io::Error::other(format!(
                "{error}\nIf the repository already exists, choose connect existing."
            ))
        })?;
        setup.created = true;
        fs::write(&checkpoint, serde_json::to_vec(&setup)?)?;
    }
    let repository: serde_json::Value = serde_json::from_str(&gh(
        root,
        &state,
        program,
        &[
            "repo",
            "view",
            &setup.request.slug,
            "--json",
            "nameWithOwner,isEmpty,isPrivate,defaultBranchRef",
        ],
        cancelled,
    )?)?;
    if repository["nameWithOwner"]
        .as_str()
        .is_none_or(|name| !name.eq_ignore_ascii_case(&setup.request.slug))
        || (setup.request.create && repository["isPrivate"] != true)
    {
        return Err(io::Error::other(
            "GitHub did not confirm the requested repository and visibility.",
        ));
    }
    let base = state.base_branch.as_deref().unwrap_or("main");
    let destination = format!("{}:refs/heads/{base}", state.base);
    let mut command = Command::new("git");
    command
        .stdin(Stdio::null())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["-c", "core.hooksPath=/dev/null", "-C"])
        .arg(&state.repo);
    if repository["isEmpty"] == true {
        command.args(["push", &url, &destination]);
        run(command, root, cancelled)?;
    } else {
        command.args(["fetch", "--no-tags", &url, base]);
        run(command, root, cancelled)?;
        if git(
            root,
            &state.repo,
            &["merge-base", &state.base, "FETCH_HEAD"],
            cancelled,
        )
        .is_err()
        {
            return Err(io::Error::other(
                "This repository has unrelated history. Clone it to a new folder or choose an empty repository. Saved work is retained.",
            ));
        }
    }
    if !has_origin(&state.repo) {
        git(
            root,
            &state.repo,
            &["remote", "add", "origin", &url],
            cancelled,
        )?;
    }
    setup.complete = true;
    fs::write(&checkpoint, serde_json::to_vec(&setup)?)?;
    Ok(())
}

pub fn publish(
    root: &Path,
    description: &str,
    draft: bool,
    program: &Path,
    cancelled: &AtomicBool,
) -> io::Result<String> {
    let mut state = load(root)?;
    if state.published()
        && let Some(pr) = &state.pr
    {
        return Ok(pr.clone());
    }
    if state.draft != draft {
        state.draft = draft;
        save(root, &state)?;
    }
    let previous_pr = if state.pr.is_some() {
        Some(open_pr(root, &state, program, cancelled)?)
    } else {
        None
    };
    validate(root, &state, cancelled)?;
    let path = checkout(root);
    let review = workspace::review(root)?;
    if review.count() == 0 && state.pr.is_none() {
        return Err(io::Error::other("No source changes to publish."));
    }
    let head = git(root, &path, &["rev-parse", "HEAD"], cancelled)?;
    if state.phase == "ready" {
        if head
            != *state
                .checkpoint
                .as_ref()
                .or(state.head.as_ref())
                .unwrap_or(&state.base)
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
        if head
            == *state
                .checkpoint
                .as_ref()
                .or(state.head.as_ref())
                .unwrap_or(&state.base)
        {
            transfer(root, &state, cancelled)?;
            let title = description
                .lines()
                .next()
                .unwrap_or("Codemod")
                .chars()
                .take(120)
                .collect::<String>();
            if !git(root, &path, &["diff", "--cached", "--name-only"], cancelled)?.is_empty() {
                git(root, &path, &["commit", "-m", &title], cancelled)?;
            }
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
        &["config", "--get", "remote.origin.url"],
        cancelled,
    )?;
    let slug = origin
        .strip_prefix("https://github.com/")
        .or_else(|| origin.strip_prefix("git@github.com:"))
        .or_else(|| origin.strip_prefix("ssh://git@github.com/"))
        .map(|name| name.trim_end_matches(".git"))
        .filter(|name| valid_slug(name))
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
            "url,headRefOid,state,isDraft",
        ],
        cancelled,
    )?)?;
    let existing = prs
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| item["url"].as_str())
        .map(str::to_owned);
    let existing_draft = existing.as_ref().and_then(|_| prs[0]["isDraft"].as_bool());
    if existing.is_some() && prs[0]["state"] != "OPEN" {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "This PR is merged or closed. Start a new mod from the updated project.",
        ));
    }
    if previous_pr.is_some() && existing.as_deref() != state.pr.as_deref() {
        return Err(io::Error::other(
            "Cannot find the saved PR on this branch. Work is retained.",
        ));
    }
    if existing.is_some()
        && prs[0]["headRefOid"].as_str() != state.head.as_deref()
        && !(previous_pr.is_some()
            && prs[0]["headRefOid"].as_str() == state.published_head.as_deref())
    {
        return Err(io::Error::other(
            "Existing PR has different commits; work is retained.",
        ));
    }
    if existing.is_none() || prs[0]["headRefOid"].as_str() != state.head.as_deref() {
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
            .unwrap_or("Codemod")
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
    if existing_draft.is_some_and(|was_draft| was_draft != draft) {
        let mut args = vec!["pr", "ready", &pr];
        if draft {
            args.push("--undo");
        }
        gh(root, &state, program, &args, cancelled)?;
    }
    state.pr = Some(pr.clone());
    state.published_head = state.head.clone();
    state.checkpoint = state.head.clone();
    state.draft = draft;
    state.publishing = false;
    state.phase = "published".into();
    save(root, &state)?;
    if root.join("transfer").exists() {
        fs::remove_dir_all(root.join("transfer"))?;
    }
    Ok(pr)
}

pub fn cleanup(root: &Path, cancelled: &AtomicBool) -> io::Result<()> {
    let mut state = load(root)?;
    if state.phase == "cleaned" {
        return Ok(());
    }
    if !state.discarding
        && state.pr.is_none()
        && !["ready", "preparing"].contains(&state.phase.as_str())
    {
        return Err(io::Error::other("Publish saved changes before cleanup."));
    }
    let path = checkout(root);
    if !state.discarding
        && !state.published()
        && root.join("work").exists()
        && workspace::review(root)?.count() > 0
    {
        return Err(io::Error::other(
            "The mod has source changes to preserve before cleanup.",
        ));
    }
    if path.exists() {
        validate(root, &state, cancelled)?;
        if !state.discarding && !git(root, &path, &["status", "--porcelain"], cancelled)?.is_empty()
        {
            return Err(io::Error::other(
                "Worktree has unsaved changes; cleanup paused.",
            ));
        }
        if !state.discarding
            && state.pr.is_none()
            && git(root, &path, &["rev-parse", "HEAD"], cancelled)? != state.base
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
            .args(["worktree", "remove"]);
        if state.discarding {
            command.arg("--force");
        }
        command.arg(&path);
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
            &[
                "branch",
                if state.discarding { "-D" } else { "-d" },
                &state.branch,
            ],
            cancelled,
        )?;
    }
    if state.discarding {
        if let Some(head) = state
            .published_head
            .as_ref()
            .or(state.head.as_ref())
            .filter(|_| state.pr.is_some())
        {
            git(
                root,
                &state.repo,
                &["update-ref", &format!("refs/heads/{}", state.branch), head],
                cancelled,
            )?;
        }
        clear_snapshot(root)?;
    }
    state.phase = "cleaned".into();
    save(root, &state)
}

pub fn mark_closing(root: &Path, discard: bool, delete: bool) -> io::Result<()> {
    let mut state = load(root)?;
    if discard && state.pr.is_some() && state.phase == "pushed" {
        // Pushed changes are already in the existing PR.
        state.published_head = state.head.clone();
    }
    state.closing = !delete;
    state.removing = delete;
    state.discarding = discard;
    save(root, &state)
}

pub fn cancel_closing(root: &Path) -> io::Result<()> {
    let mut state = load(root)?;
    state.closing = false;
    save(root, &state)
}

fn open_pr(
    root: &Path,
    state: &GitMod,
    program: &Path,
    cancelled: &AtomicBool,
) -> io::Result<serde_json::Value> {
    let url = state
        .pr
        .as_deref()
        .ok_or_else(|| io::Error::other("This mod has no PR to continue."))?;
    let pr: serde_json::Value = serde_json::from_str(&gh(
        root,
        state,
        program,
        &[
            "pr",
            "view",
            url,
            "--json",
            "url,state,headRefOid,headRefName,isDraft",
        ],
        cancelled,
    )?)?;
    if pr["state"] != "OPEN" {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "This PR is merged or closed. Start a new mod from the updated project.",
        ));
    }
    let expected = state.published_head.as_ref().or(state.head.as_ref());
    if pr["headRefName"] != state.branch
        || (pr["headRefOid"].as_str() != expected.map(String::as_str)
            && pr["headRefOid"].as_str() != state.head.as_deref())
    {
        return Err(io::Error::other(
            "The PR branch changed outside the harness. Work is retained.",
        ));
    }
    Ok(pr)
}

fn clear_snapshot(root: &Path) -> io::Result<()> {
    for name in [
        "before",
        "work",
        "base.git",
        "snapshot-ready",
        "review.patch",
    ] {
        let path = root.join(name);
        if path.is_dir() {
            fs::remove_dir_all(path)?;
        } else if path.exists() {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

pub fn reopen(root: &Path, cancelled: &AtomicBool) -> io::Result<()> {
    let mut state = load(root)?;
    if !checkout(root).exists() {
        let expected = state
            .checkpoint
            .as_ref()
            .or(state.head.as_ref())
            .unwrap_or(&state.base);
        if git(root, &state.repo, &["rev-parse", &state.branch], cancelled)? != *expected {
            return Err(io::Error::other(
                "The saved branch changed. Work is retained.",
            ));
        }
        state.phase = "preparing".into();
        save(root, &state)?;
        prepare(&state.repo, root, cancelled)?;
        state = load(root)?;
    }
    validate(root, &state, cancelled)?;
    state.closing = false;
    state.phase = if state.pr.is_some() && state.checkpoint == state.published_head {
        "published"
    } else {
        "ready"
    }
    .into();
    save(root, &state)
}

pub fn prepare_edits(root: &Path, program: &Path, cancelled: &AtomicBool) -> io::Result<()> {
    let mut state = load(root)?;
    state.continuing = true;
    save(root, &state)?;
    if state.pr.is_some() {
        open_pr(root, &state, program, cancelled)?;
    }
    if !checkout(root).exists() {
        reopen(root, cancelled)?;
        state = load(root)?;
    }
    state.phase = "ready".into();
    state.fingerprint = None;
    state.continuing = true;
    save(root, &state)
}

// Keep an incremental export until its commit is saved, so interrupted copies can resume.
fn transfer(root: &Path, state: &GitMod, cancelled: &AtomicBool) -> io::Result<()> {
    let path = checkout(root);
    let export = root.join("transfer");
    let source = workspace::source_state(&root.join("work"))?;
    if !export.join("base.git").exists() {
        validate(root, state, cancelled)?;
        let expected = state
            .checkpoint
            .as_ref()
            .or(state.head.as_ref())
            .unwrap_or(&state.base);
        let recovering_export = state.phase == "exporting" && expected == &state.base;
        if git(root, &path, &["rev-parse", "HEAD"], cancelled)? != *expected
            || (!recovering_export
                && !git(root, &path, &["status", "--porcelain"], cancelled)?.is_empty())
        {
            return Err(io::Error::other(
                "The worktree changed outside the harness. Work is retained.",
            ));
        }
        let staging = root.join("transfer-next");
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        let baseline_path = if recovering_export {
            root.join("before")
        } else {
            path.clone()
        };
        let baseline = workspace::source_state(&baseline_path)?;
        workspace::create_snapshot(&baseline, &staging)?;
        workspace::replace_source(&staging, &source)?;
        fs::rename(staging, &export)?;
    }
    let review = workspace::review(&export)?;
    if review.fingerprint != workspace::fingerprint(&source)? {
        return Err(io::Error::other(
            "Source changed during saving. Work is retained.",
        ));
    }
    validate(root, state, cancelled)?;
    let head = git(root, &path, &["rev-parse", "HEAD"], cancelled)?;
    let expected = state
        .checkpoint
        .as_ref()
        .or(state.head.as_ref())
        .unwrap_or(&state.base);
    if head != *expected
        && (git(root, &path, &["rev-parse", "HEAD^"], cancelled)? != *expected
            || workspace::fingerprint(&workspace::source_state(&path)?)? != review.fingerprint)
    {
        return Err(io::Error::other(
            "The worktree changed during saving. Work is retained.",
        ));
    }
    review.export(&path, &export)?;
    stage(root, &path, &review, cancelled)?;
    let staged = git(
        root,
        &path,
        &["diff", "--cached", "--name-only", "-z"],
        cancelled,
    )?;
    if staged
        .split('\0')
        .filter(|p| !p.is_empty())
        .any(|p| !review.paths().any(|allowed| allowed == Path::new(p)))
    {
        return Err(io::Error::other(
            "Unrelated staged files found. Work is retained.",
        ));
    }
    Ok(())
}

pub fn checkpoint(root: &Path, cancelled: &AtomicBool) -> io::Result<()> {
    if root.join("checkpoint-error").exists() {
        drop(crate::sandbox::Sandbox::prepare(root, cancelled, |_| {})?);
    }
    let mut state = load(root)?;
    if !checkout(root).exists() {
        reopen(root, cancelled)?;
        state = load(root)?;
    }
    validate(root, &state, cancelled)?;
    if !root.join("work").exists()
        && !git(root, &checkout(root), &["status", "--porcelain"], cancelled)?.is_empty()
    {
        return Err(io::Error::other(
            "The worktree has outside edits. Work is retained.",
        ));
    }
    if root.join("work").exists() {
        transfer(root, &state, cancelled)?;
        if !git(
            root,
            &checkout(root),
            &["diff", "--cached", "--name-only"],
            cancelled,
        )?
        .is_empty()
        {
            git(
                root,
                &checkout(root),
                &["commit", "-m", "Save codemod checkpoint"],
                cancelled,
            )?;
        }
        if workspace::fingerprint(&workspace::source_state(&checkout(root))?)?
            != workspace::fingerprint(&workspace::source_state(&root.join("work"))?)?
        {
            return Err(io::Error::other(
                "Checkpoint differs from source. The VM is retained.",
            ));
        }
    }
    state.checkpoint = Some(git(
        root,
        &checkout(root),
        &["rev-parse", "HEAD"],
        cancelled,
    )?);
    save(root, &state)?;
    let export = root.join("transfer");
    if export.exists() {
        fs::remove_dir_all(export)?;
    }
    crate::sandbox::delete(root)?;
    state.publishing = false;
    state.continuing = false;
    state.fingerprint = None;
    state.phase = "closed".into();
    save(root, &state)
}

pub fn prune(root: &Path, cancelled: &AtomicBool) -> io::Result<()> {
    let mut state = load(root)?;
    if !["closed", "pruned"].contains(&state.phase.as_str()) || root.join("vm.json").exists() {
        return Err(io::Error::other(
            "Only closed codemod worktrees can be pruned.",
        ));
    }
    let path = checkout(root);
    if path.exists() {
        validate(root, &state, cancelled)?;
        if state.checkpoint.as_deref()
            != Some(&git(root, &path, &["rev-parse", "HEAD"], cancelled)?)
            || !git(
                root,
                &path,
                &[
                    "status",
                    "--porcelain",
                    "--untracked-files=all",
                    "--ignored",
                ],
                cancelled,
            )?
            .is_empty()
        {
            return Err(io::Error::other(
                "The closed worktree changed; pruning skipped.",
            ));
        }
        git(
            root,
            &state.repo,
            &["worktree", "remove", path.to_str().unwrap()],
            cancelled,
        )?;
    }
    state.phase = "pruned".into();
    save(root, &state)
}

pub fn finish_continuation(root: &Path) -> io::Result<()> {
    let mut state = load(root)?;
    state.continuing = false;
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
pub(crate) mod tests {
    use super::*;
    use crate::store::test_support::TestData;

    pub(crate) fn fixture() -> (TestData, PathBuf, PathBuf, PathBuf) {
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
        git(
            &root,
            &repo,
            &[
                "config",
                &format!("url.{}.insteadOf", bare.display()),
                "https://github.com/fixture/project.git",
            ],
            &flag,
        )
        .unwrap();
        let program = data.0.join("fake-gh");
        let pr = data.0.join("pr-created");
        let failed = data.0.join("fail-once");
        fs::write(&program, format!(r#"#!/bin/sh
branch=$(git -C '{repo}' for-each-ref --format='%(refname:short)' refs/heads/sprowt/)
head=$(git -C '{bare}' rev-parse "$branch" 2>/dev/null)
state=OPEN
if [ -f '{state}' ]; then state=$(cat '{state}'); fi
draft=false
if [ -f '{draft}' ]; then draft=true; fi
case "$1 $2" in
  "repo create")
    printf '%s\n' "$@" >> '{log}'
    if [ -f '{repo_created}' ]; then exit 1; fi
    touch '{repo_created}' ;;
  "repo view")
    if [ -f '{repo_view_fail}' ]; then rm '{repo_view_fail}'; exit 1; fi
    empty=true
    if git -C '{bare}' show-ref --quiet; then empty=false; fi
    printf '{{"nameWithOwner":"fixture/project","isEmpty":%s,"isPrivate":true,"defaultBranchRef":{{"name":"main"}}}}\n' "$empty" ;;
  "pr list") if [ -f '{pr}' ]; then printf '[{{"url":"https://github.com/fixture/project/pull/1","headRefOid":"%s","state":"%s","isDraft":%s}}]\n' "$head" "$state" "$draft"; else printf '%s\n' '[]'; fi ;;
  "pr view") printf '{{"url":"https://github.com/fixture/project/pull/1","headRefOid":"%s","headRefName":"%s","state":"%s","isDraft":%s}}\n' "$head" "$branch" "$state" "$draft" ;;
  "pr ready")
    printf '%s\n' "$@" >> '{log}'
    if [ -f '{ready_fail}' ]; then rm '{ready_fail}'; exit 1; fi
    if [ "$4" = '--undo' ]; then touch '{draft}'; else rm -f '{draft}'; fi ;;
  "pr create")
    printf '%s\n' "$@" >> '{log}'
    git -C '{checkout}' rev-parse HEAD > '{pr}'
    for arg in "$@"; do if [ "$arg" = '--draft' ]; then touch '{draft}'; fi; done
    if [ -f '{failed}' ]; then rm '{failed}'; printf '%s\n' 'connection interrupted after PR creation' >&2; exit 1; fi
    printf '%s\n' 'https://github.com/fixture/project/pull/1' ;;
  *) exit 1 ;;
esac
"#, pr=pr.display(), failed=failed.display(), log=data.0.join("gh-args").display(), checkout=checkout(&root).display(), repo=repo.display(), bare=bare.display(), state=data.0.join("pr-state").display(), draft=data.0.join("pr-draft").display(), ready_fail=data.0.join("ready-fail-once").display(), repo_created=data.0.join("repo-created").display(), repo_view_fail=data.0.join("repo-view-fail").display())).unwrap();
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
    fn refreshing_recovers_a_partial_checkout_and_refuses_started_or_changed_work() {
        for case in ["interrupted", "outside edit", "started"] {
            let (_data, repo, root, target) = crate::git_sync::tests::remote_change();
            let flag = AtomicBool::new(false);
            prepare(&repo, &root, &flag).unwrap();
            let before = load(&root).unwrap().base;
            assert!(
                git(&root, &checkout(&root), &["status", "--porcelain"], &flag)
                    .unwrap()
                    .is_empty()
            );
            match case {
                "outside edit" => fs::write(checkout(&root).join("a.txt"), "outside\n").unwrap(),
                "started" => workspace::create(&checkout(&root), &root).unwrap(),
                _ => {
                    fs::write(root.join("refresh-target"), &target).unwrap();
                    git(
                        &root,
                        &checkout(&root),
                        &["read-tree", "-m", "-u", &before, &target],
                        &flag,
                    )
                    .unwrap();
                }
            }
            let result = refresh(&repo, &root, &flag);
            if case == "interrupted" {
                result.unwrap();
                assert_eq!(load(&root).unwrap().base, target);
                assert_eq!(
                    git(&root, &checkout(&root), &["rev-parse", "HEAD"], &flag).unwrap(),
                    target
                );
                assert_eq!(
                    fs::read_to_string(checkout(&root).join("new.txt")).unwrap(),
                    "incoming\n"
                );
                assert!(!root.join("refresh-target").exists());
            } else {
                assert!(result.is_err());
                assert_eq!(load(&root).unwrap().base, before);
                assert_eq!(
                    fs::read_to_string(checkout(&root).join("a.txt")).unwrap(),
                    if case == "outside edit" {
                        "outside\n"
                    } else {
                        "original\n"
                    }
                );
            }
        }
    }

    #[test]
    fn private_repository_setup_recovers_and_publishes_only_the_baseline_to_main() {
        let (data, repo, root, program) = fixture();
        let flag = AtomicBool::new(false);
        git(&root, &repo, &["remote", "remove", "origin"], &flag).unwrap();
        prepare(&repo, &root, &flag).unwrap();
        edited(&root);
        let request = RepositoryRequest {
            slug: "fixture/project".into(),
            create: true,
        };
        fs::write(data.0.join("repo-view-fail"), "once").unwrap();
        assert!(connect_repository(&root, request.clone(), &program, &flag).is_err());
        assert!(repository_pending(&root).is_some() && !has_origin(&repo));
        connect_repository(&root, request, &program, &flag).unwrap();
        assert!(repository_pending(&root).is_none() && has_origin(&repo));
        let log = fs::read_to_string(data.0.join("gh-args")).unwrap();
        assert_eq!(log.lines().filter(|line| *line == "--private").count(), 1);
        let bare = data.0.join("remote.git");
        assert_eq!(
            git(&root, &bare, &["rev-parse", "main"], &flag).unwrap(),
            load(&root).unwrap().base
        );
        let pr = publish(&root, "A small change", false, &program, &flag).unwrap();
        assert!(pr.ends_with("/pull/1"));
        assert_eq!(
            git(&root, &bare, &["show", "main:a.txt"], &flag).unwrap(),
            "original"
        );
        cleanup(&root, &flag).unwrap();
        assert!(!checkout(&root).exists());
    }

    #[test]
    fn first_pr_from_existing_files_preserves_the_initial_source_bytes() {
        let (data, repo, root, program) = fixture();
        let flag = AtomicBool::new(false);
        fs::remove_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join(".gitattributes"), "*.txt text eol=lf\n").unwrap();
        fs::write(repo.join("windows.txt"), "line\r\n").unwrap();
        workspace::create(&repo, &root).unwrap();
        adopt(&repo, &root, &flag).unwrap();
        git(
            &root,
            &repo,
            &[
                "config",
                &format!("url.{}.insteadOf", data.0.join("remote.git").display()),
                "https://github.com/fixture/project.git",
            ],
            &flag,
        )
        .unwrap();
        connect_repository(
            &root,
            RepositoryRequest {
                slug: "fixture/project".into(),
                create: false,
            },
            &program,
            &flag,
        )
        .unwrap();
        fs::write(root.join("work/a.txt"), "changed\n").unwrap();
        publish(&root, "Update existing project", false, &program, &flag).unwrap();
        assert_eq!(
            fs::read(checkout(&root).join("windows.txt")).unwrap(),
            b"line\r\n"
        );
        assert_eq!(
            git(&root, &repo, &["show", "HEAD:a.txt"], &flag).unwrap(),
            "original"
        );
        cleanup(&root, &flag).unwrap();
    }

    #[test]
    fn connecting_unrelated_history_keeps_the_remote_and_saved_work_untouched() {
        let (data, repo, root, program) = fixture();
        let flag = AtomicBool::new(false);
        git(&root, &repo, &["remote", "remove", "origin"], &flag).unwrap();
        prepare(&repo, &root, &flag).unwrap();
        edited(&root);
        let bare = data.0.join("remote.git");
        let tree = git(&root, &bare, &["mktree"], &flag).unwrap();
        let head = git(
            &root,
            &bare,
            &["commit-tree", &tree, "-m", "Unrelated project"],
            &flag,
        )
        .unwrap();
        git(
            &root,
            &bare,
            &["update-ref", "refs/heads/main", &head],
            &flag,
        )
        .unwrap();
        let error = connect_repository(
            &root,
            RepositoryRequest {
                slug: "fixture/project".into(),
                create: false,
            },
            &program,
            &flag,
        )
        .unwrap_err();
        assert!(error.to_string().contains("unrelated history"));
        assert!(!has_origin(&repo));
        assert_eq!(
            git(&root, &bare, &["rev-parse", "main"], &flag).unwrap(),
            head
        );
        assert_eq!(
            fs::read_to_string(root.join("work/a.txt")).unwrap(),
            "changed\n"
        );
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
        mark_closing(&root, false, false).unwrap();
        assert!(cleanup(&root, &flag).is_err());
        publish(&root, "Partial work", true, &program, &flag).unwrap();
        assert!(
            fs::read_to_string(data.0.join("gh-args"))
                .unwrap()
                .contains("--draft")
        );
        cleanup(&root, &flag).unwrap();
        assert!(load(&root).unwrap().closing);
        assert!(load(&root).unwrap().pr.is_some());
    }

    #[test]
    fn continuing_restores_source_and_updates_the_same_pr_after_an_interruption() {
        let (data, repo, root, program) = fixture();
        let flag = AtomicBool::new(false);
        prepare(&repo, &root, &flag).unwrap();
        edited(&root);
        let url = publish(&root, "First change", true, &program, &flag).unwrap();
        let first = load(&root).unwrap().head.unwrap();
        cleanup(&root, &flag).unwrap();
        prepare_edits(&root, &program, &flag).unwrap();
        prepare_edits(&root, &program, &flag).unwrap();
        assert!(load(&root).unwrap().continuing);
        assert_eq!(workspace::review(&root).unwrap().count(), 3);
        assert_eq!(
            fs::read_to_string(root.join("work/a.txt")).unwrap(),
            "changed\n"
        );
        finish_continuation(&root).unwrap();
        fs::write(root.join("work/a.txt"), "follow-up\n").unwrap();
        fs::write(data.0.join("ready-fail-once"), "").unwrap();
        assert!(publish(&root, "Follow-up", false, &program, &flag).is_err());
        let second = load(&root).unwrap().head.unwrap();
        assert_ne!(first, second);
        assert_eq!(
            publish(&root, "Follow-up", false, &program, &flag).unwrap(),
            url
        );
        assert!(!data.0.join("pr-draft").exists());
        assert_eq!(
            fs::read_to_string(data.0.join("gh-args"))
                .unwrap()
                .lines()
                .filter(|s| *s == "create")
                .count(),
            1
        );
        cleanup(&root, &flag).unwrap();
        let remote = data.0.join("remote.git");
        let reference = format!("refs/heads/{}", load(&root).unwrap().branch);
        git(&root, &remote, &["update-ref", &reference, &first], &flag).unwrap();
        assert!(
            prepare_edits(&root, &program, &flag)
                .unwrap_err()
                .to_string()
                .contains("outside the harness")
        );
        assert!(!checkout(&root).exists());
        git(&root, &remote, &["update-ref", &reference, &second], &flag).unwrap();
        fs::write(data.0.join("pr-state"), "MERGED").unwrap();
        assert!(
            prepare_edits(&root, &program, &flag)
                .unwrap_err()
                .to_string()
                .contains("merged or closed")
        );
        assert!(!checkout(&root).exists());
        assert_eq!(
            fs::read_to_string(repo.join("a.txt")).unwrap(),
            "original\n"
        );
    }

    #[test]
    fn interrupted_publication_can_be_saved_as_draft_on_close() {
        let (data, repo, root, program) = fixture();
        let flag = AtomicBool::new(false);
        prepare(&repo, &root, &flag).unwrap();
        edited(&root);
        fs::write(data.0.join("fail-once"), "").unwrap();
        assert!(publish(&root, "Change", false, &program, &flag).is_err());
        mark_closing(&root, false, false).unwrap();
        publish(&root, "Change", true, &program, &flag).unwrap();
        assert!(data.0.join("pr-draft").exists());
        cleanup(&root, &flag).unwrap();
    }

    #[test]
    fn discard_cleans_owned_work_without_creating_a_pr() {
        let (_data, repo, root, _) = fixture();
        let flag = AtomicBool::new(false);
        prepare(&repo, &root, &flag).unwrap();
        edited(&root);
        assert!(cleanup(&root, &flag).is_err());
        mark_closing(&root, true, false).unwrap();
        cleanup(&root, &flag).unwrap();
        assert!(!checkout(&root).exists() && !root.join("work").exists());
        let state = load(&root).unwrap();
        assert!(state.closing && state.pr.is_none());
        assert!(git(&root, &repo, &["rev-parse", &state.branch], &flag).is_err());
        assert_eq!(
            fs::read_to_string(repo.join("a.txt")).unwrap(),
            "original\n"
        );
    }

    #[test]
    fn a_followup_can_revert_all_changes_on_the_same_pr() {
        let (_data, repo, root, program) = fixture();
        let flag = AtomicBool::new(false);
        prepare(&repo, &root, &flag).unwrap();
        edited(&root);
        let pr = publish(&root, "First version", false, &program, &flag).unwrap();
        prepare_edits(&root, &program, &flag).unwrap();
        finish_continuation(&root).unwrap();
        let baseline = workspace::source_state(&root.join("before")).unwrap();
        workspace::replace_source(&root, &baseline).unwrap();
        assert_eq!(workspace::review(&root).unwrap().count(), 0);
        assert_eq!(
            publish(&root, "Revert changes", false, &program, &flag).unwrap(),
            pr
        );
        assert_eq!(workspace::source_state(&checkout(&root)).unwrap(), baseline);
        assert!(checkout(&root).exists());
    }

    #[test]
    fn checkpoints_keep_edits_deletions_and_modes_across_close_prune_and_reopen() {
        let (_data, repo, root, program) = fixture();
        let flag = AtomicBool::new(false);
        prepare(&repo, &root, &flag).unwrap();
        edited(&root);
        let base = load(&root).unwrap().base;
        mark_closing(&root, false, false).unwrap();
        checkpoint(&root, &flag).unwrap();
        let first = load(&root).unwrap().checkpoint.unwrap();
        assert_ne!(base, first);
        assert!(load(&root).unwrap().pr.is_none());
        checkpoint(&root, &flag).unwrap();
        assert_eq!(
            load(&root).unwrap().checkpoint.as_deref(),
            Some(first.as_str())
        );
        prune(&root, &flag).unwrap();
        prune(&root, &flag).unwrap();
        assert!(!checkout(&root).exists());
        reopen(&root, &flag).unwrap();
        assert_eq!(
            fs::read_to_string(checkout(&root).join("a.txt")).unwrap(),
            "changed\n"
        );
        assert!(!checkout(&root).join("delete.txt").exists());
        fs::write(root.join("work/a.txt"), "second edit\n").unwrap();
        fs::remove_file(root.join("work/new.txt")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root.join("work/a.txt"), fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        checkpoint(&root, &flag).unwrap();
        let second = load(&root).unwrap().checkpoint.unwrap();
        assert_ne!(first, second);
        reopen(&root, &flag).unwrap();
        assert_eq!(
            fs::read_to_string(checkout(&root).join("a.txt")).unwrap(),
            "second edit\n"
        );
        assert!(!checkout(&root).join("new.txt").exists());
        let pr = publish(&root, "Saved change", false, &program, &flag).unwrap();
        assert_eq!(load(&root).unwrap().head.as_deref(), Some(second.as_str()));
        assert!(pr.ends_with("/pull/1") && checkout(&root).exists());
    }

    #[test]
    fn close_keeps_the_latest_source_after_an_interrupted_pr_update() {
        let (data, repo, root, program) = fixture();
        let flag = AtomicBool::new(false);
        prepare(&repo, &root, &flag).unwrap();
        edited(&root);
        let pr = publish(&root, "First change", true, &program, &flag).unwrap();
        prepare_edits(&root, &program, &flag).unwrap();
        finish_continuation(&root).unwrap();
        fs::write(root.join("work/a.txt"), "already pushed\n").unwrap();
        fs::write(data.0.join("ready-fail-once"), "").unwrap();
        assert!(publish(&root, "Follow-up", false, &program, &flag).is_err());
        mark_closing(&root, false, false).unwrap();
        checkpoint(&root, &flag).unwrap();
        let head = load(&root).unwrap().checkpoint.unwrap();
        prune(&root, &flag).unwrap();
        reopen(&root, &flag).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("work/a.txt")).unwrap(),
            "already pushed\n"
        );
        assert_eq!(
            git(&root, &checkout(&root), &["rev-parse", "HEAD"], &flag).unwrap(),
            head
        );
        assert_eq!(
            publish(&root, "Follow-up", false, &program, &flag).unwrap(),
            pr
        );
    }

    #[test]
    fn retention_and_checkpoint_refuse_outside_edits() {
        let (_data, repo, root, _program) = fixture();
        let flag = AtomicBool::new(false);
        prepare(&repo, &root, &flag).unwrap();
        edited(&root);
        fs::write(checkout(&root).join("a.txt"), "outside edit").unwrap();
        assert!(checkpoint(&root, &flag).is_err());
        assert_eq!(
            fs::read_to_string(checkout(&root).join("a.txt")).unwrap(),
            "outside edit"
        );
        fs::write(checkout(&root).join("a.txt"), "original\n").unwrap();
        checkpoint(&root, &flag).unwrap();
        fs::write(checkout(&root).join("notes.txt"), "keep me").unwrap();
        assert!(
            prune(&root, &flag)
                .unwrap_err()
                .to_string()
                .contains("pruning skipped")
        );
        assert_eq!(
            fs::read_to_string(checkout(&root).join("notes.txt")).unwrap(),
            "keep me"
        );
        fs::remove_file(checkout(&root).join("notes.txt")).unwrap();
        prune(&root, &flag).unwrap();
        let state = load(&root).unwrap();
        git(
            &root,
            &repo,
            &[
                "update-ref",
                &format!("refs/heads/{}", state.branch),
                &state.base,
            ],
            &flag,
        )
        .unwrap();
        assert!(
            reopen(&root, &flag)
                .unwrap_err()
                .to_string()
                .contains("saved branch changed")
        );
    }

    #[test]
    fn checkpoint_recovers_after_export_and_after_commit_without_duplicate_commits() {
        let (_data, repo, root, _program) = fixture();
        let flag = AtomicBool::new(false);
        prepare(&repo, &root, &flag).unwrap();
        edited(&root);
        let state = load(&root).unwrap();
        transfer(&root, &state, &flag).unwrap();
        git(
            &root,
            &checkout(&root),
            &["commit", "-m", "Saved but unacknowledged"],
            &flag,
        )
        .unwrap();
        let head = git(&root, &checkout(&root), &["rev-parse", "HEAD"], &flag).unwrap();
        checkpoint(&root, &flag).unwrap();
        assert_eq!(
            load(&root).unwrap().checkpoint.as_deref(),
            Some(head.as_str())
        );
        assert!(!root.join("transfer").exists());
        checkpoint(&root, &flag).unwrap();
        assert_eq!(
            load(&root).unwrap().checkpoint.as_deref(),
            Some(head.as_str())
        );
    }

    #[test]
    #[ignore = "runs a temporary Apple Container VM through publication, close and reopen"]
    fn publishing_keeps_the_vm_and_closing_removes_it_without_losing_source() {
        let (_data, repo, root, program) = fixture();
        let flag = AtomicBool::new(false);
        prepare(&repo, &root, &flag).unwrap();
        workspace::create(&checkout(&root), &root).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> io::Result<()> {
                let mut vm =
                    crate::sandbox::Sandbox::prepare(&root, &flag, |label| eprintln!("{label}"))?;
                let check = crate::execution::Check { task: None,
                check: "Source copy and guest edits".into(),
                    command: vec!["/bin/sh".into(), "-c".into(), "test ! -f .git && test ! -e .git/HEAD && test ! -e .env && test \"$(cat a.txt)\" = original && printf 'VM edit\\n' > a.txt && printf 'VM new\\n' > guest.txt && rm delete.txt".into()],
            };
                let (_, checks) = vm.verify(&[check], &flag)?;
                assert_eq!(checks[0].exit_code, Some(0), "{}", checks[0].output);
                vm.export(&flag)?;
                drop(vm);
                assert_eq!(fs::read_to_string(repo.join("a.txt"))?, "original\n");
                assert_eq!(fs::read_to_string(root.join("work/a.txt"))?, "VM edit\n");
                let pr = publish(&root, "Change in VM", false, &program, &flag)?;
                assert!(root.join("vm.json").exists() && checkout(&root).exists());
                mark_closing(&root, false, false)?;
                checkpoint(&root, &flag)?;
                assert!(!root.join("vm.json").exists() && checkout(&root).exists());
                assert_eq!(load(&root)?.phase, "closed");
                assert!(root.join("work/guest.txt").exists());
                prune(&root, &flag)?;
                reopen(&root, &flag)?;
                prepare_edits(&root, &program, &flag)?;
                finish_continuation(&root)?;
                let mut vm =
                    crate::sandbox::Sandbox::prepare(&root, &flag, |label| eprintln!("{label}"))?;
                let check = crate::execution::Check { task: None, check: "Fresh VM contains the published source".into(),
                    command: vec!["/bin/sh".into(), "-c".into(), "test \"$(cat a.txt)\" = 'VM edit' && test -f guest.txt && test ! -f delete.txt && printf 'follow-up\\n' > a.txt".into()] };
                let (_, checks) = vm.verify(&[check], &flag)?;
                assert_eq!(checks[0].exit_code, Some(0), "{}", checks[0].output);
                drop(vm);
                assert_eq!(
                    publish(&root, "Follow-up in VM", false, &program, &flag)?,
                    pr
                );
                assert!(root.join("vm.json").exists() && checkout(&root).exists());
                mark_closing(&root, false, false)?;
                checkpoint(&root, &flag)?;
                assert!(!root.join("vm.json").exists() && checkout(&root).exists());
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
