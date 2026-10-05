use std::{
    collections::BTreeSet,
    fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
};

use serde::{Deserialize, Serialize};

use crate::git_mod::{git, run, safe_git};

#[derive(Deserialize, Serialize)]
struct Sync {
    project: PathBuf,
    workspace: PathBuf,
    pid: u32,
    branch: String,
    before: String,
    after: String,
}

pub fn project(project: &Path, root: &Path, cancelled: &AtomicBool) -> io::Result<()> {
    if root.join("sync-project.json").exists() {
        let state: Sync = serde_json::from_slice(&fs::read(root.join("sync-project.json"))?)?;
        if state.project != project.canonicalize()? || state.workspace != root.canonicalize()? {
            return Err(io::Error::other(
                "Synchronization belongs to a different project.",
            ));
        }
        return finish(root);
    }
    let Ok(branch) = git(
        root,
        project,
        &["symbolic-ref", "--quiet", "HEAD"],
        cancelled,
    ) else {
        return Ok(());
    };
    let name = branch.strip_prefix("refs/heads/").unwrap();
    let configured = git(
        root,
        project,
        &["config", "--get", &format!("branch.{name}.remote")],
        cancelled,
    )
    .ok();
    let remote = configured.as_deref().unwrap_or("origin");
    if remote == "." || (configured.is_none() && !crate::git_mod::has_origin(project)) {
        return Ok(());
    }
    if remote.starts_with('-') {
        return Err(io::Error::other("Invalid upstream remote."));
    }
    let reference = git(
        root,
        project,
        &["config", "--get", &format!("branch.{name}.merge")],
        cancelled,
    )
    .unwrap_or_else(|_| branch.clone());
    if !reference.starts_with("refs/heads/") {
        return Err(io::Error::other("The upstream must be a remote branch."));
    }
    let mut command = network(project);
    command.args(["ls-remote", "--heads", "--", remote, &reference]);
    if run(command, root, cancelled)?.is_empty() {
        return if configured.is_some() {
            Err(io::Error::other(format!(
                "Upstream {remote}/{} is missing. Check its tracking settings.",
                reference.trim_start_matches("refs/heads/")
            )))
        } else {
            Ok(())
        };
    }
    let tracking = format!(
        "refs/remotes/{remote}/{}",
        reference.trim_start_matches("refs/heads/")
    );
    let mut command = network(project);
    command.args([
        "fetch",
        "--no-tags",
        "--no-recurse-submodules",
        "--no-write-fetch-head",
        "--",
        remote,
        &format!("+{reference}:{tracking}"),
    ]);
    run(command, root, cancelled)
        .map_err(|error| io::Error::other(format!("Could not check {remote}: {error}")))?;
    let before = git(root, project, &["rev-parse", "HEAD^{commit}"], cancelled)?;
    let after = git(
        root,
        project,
        &["rev-parse", &format!("{tracking}^{{commit}}")],
        cancelled,
    )?;
    if before == after
        || git(
            root,
            project,
            &["merge-base", "--is-ancestor", &after, &before],
            cancelled,
        )
        .is_ok()
    {
        return Ok(());
    }
    if git(
        root,
        project,
        &["merge-base", "--is-ancestor", &before, &after],
        cancelled,
    )
    .is_err()
    {
        return Err(io::Error::other(format!(
            "Local {name} and {remote}/{} have diverged. Reconcile them before starting a codemod.",
            reference.trim_start_matches("refs/heads/")
        )));
    }
    if cancelled.load(Ordering::Relaxed) {
        return Err(io::Error::other(
            "Project synchronization paused; Ctrl+R retries.",
        ));
    }
    let state = Sync {
        project: project.canonicalize()?,
        workspace: root.canonicalize()?,
        pid: std::process::id(),
        branch,
        before,
        after,
    };
    let index = index_path(&state, root)?;
    let original = read_index(&index)?;
    let pending = root.join("sync-index");
    fs::write(root.join("sync-before-index"), &original)?;
    if !original.is_empty() {
        fs::write(&pending, &original)?;
    } else {
        indexed(&state, root, &["read-tree", &state.before])?;
    }
    adopt_files(&state, root)?;
    let next = root.join("sync-project-next.json");
    fs::write(&next, serde_json::to_vec(&state)?)?;
    for path in [&pending, &root.join("sync-before-index"), &next] {
        fs::File::open(path)?.sync_all()?;
    }
    fs::rename(next, root.join("sync-project.json"))?;
    finish(root)
}

pub(crate) fn network(project: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .stdin(Stdio::null())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-C",
        ])
        .arg(project);
    command
}

fn index_path(state: &Sync, root: &Path) -> io::Result<PathBuf> {
    git(
        root,
        &state.project,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        &AtomicBool::new(false),
    )
    .map(PathBuf::from)
}

fn indexed(state: &Sync, root: &Path, args: &[&str]) -> io::Result<Vec<u8>> {
    let mut command = safe_git(&state.project)?;
    command
        .env("GIT_INDEX_FILE", root.join("sync-index"))
        .arg("-C")
        .arg(&state.project)
        .args(args);
    // Once checkout starts, finish the index transaction before honoring cancellation.
    run(command, root, &AtomicBool::new(false))
}

fn adopt_files(state: &Sync, root: &Path) -> io::Result<()> {
    let tracked = indexed(state, root, &["ls-files", "-z"])?;
    let baseline = indexed(
        state,
        root,
        &["ls-tree", "-r", "--name-only", "-z", &state.before],
    )?;
    let tracked = tracked
        .split(|b| *b == 0)
        .chain(baseline.split(|b| *b == 0))
        .collect::<BTreeSet<_>>();
    let tree = indexed(state, root, &["ls-tree", "-r", "-z", &state.after])?;
    let mut entries = Vec::new();
    for entry in tree.split(|b| *b == 0).filter(|entry| !entry.is_empty()) {
        let (meta, name) = entry.split_at(
            entry
                .iter()
                .position(|b| *b == b'\t')
                .ok_or_else(|| io::Error::other("Invalid Git tree."))?,
        );
        let name = &name[1..];
        let fields = std::str::from_utf8(meta)
            .map_err(io::Error::other)?
            .split_whitespace()
            .collect::<Vec<_>>();
        if fields.len() != 3 || !["100644", "100755"].contains(&fields[0]) {
            return Err(io::Error::other(
                "Project synchronization requires regular files; links and submodules are unsupported.",
            ));
        }
        if tracked.contains(name) {
            continue;
        }
        let name = std::str::from_utf8(name).map_err(io::Error::other)?;
        let mut path = state.project.clone();
        for part in Path::new(name).components() {
            let Component::Normal(part) = part else {
                return Err(io::Error::other("Invalid source path."));
            };
            path.push(part);
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(io::Error::other(format!(
                        "Cannot update {name}: a local path is a symlink."
                    )));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => break,
                Err(error) => return Err(error),
                _ => {}
            }
        }
        if state.project.join(name).is_file() {
            // The incoming blob becomes the baseline; existing bytes stay as local edits.
            entries.extend_from_slice(format!("{} {}\t{name}\0", fields[0], fields[2]).as_bytes());
        }
    }
    if !entries.is_empty() {
        let file = root.join("sync-files");
        fs::write(&file, entries)?;
        let mut command = safe_git(&state.project)?;
        command
            .env("GIT_INDEX_FILE", root.join("sync-index"))
            .arg("-C")
            .arg(&state.project)
            .args(["update-index", "-z", "--index-info"])
            .stdin(fs::File::open(&file)?);
        run(command, root, &AtomicBool::new(false))?;
        fs::remove_file(file)?;
    }
    Ok(())
}

struct IndexLock {
    path: PathBuf,
    record: Vec<u8>,
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        if fs::read(&self.path).is_ok_and(|bytes| bytes == self.record) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn read_index(path: &Path) -> io::Result<Vec<u8>> {
    match fs::read(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        result => result,
    }
}

fn finish(root: &Path) -> io::Result<()> {
    let record = fs::read(root.join("sync-project.json"))?;
    let state: Sync = serde_json::from_slice(&record)?;
    let flag = AtomicBool::new(false);
    if git(
        root,
        &state.project,
        &["symbolic-ref", "--quiet", "HEAD"],
        &flag,
    )? != state.branch
    {
        return Err(io::Error::other(
            "The project branch changed during synchronization. Saved state is retained.",
        ));
    }
    let index = index_path(&state, root)?;
    let mut lock_name = index.as_os_str().to_owned();
    lock_name.push(".lock");
    let lock_path = PathBuf::from(lock_name);
    let pending = fs::read(root.join("sync-index"))?;
    if let Ok(bytes) = fs::read(&lock_path) {
        let owner_alive = unsafe { libc::kill(state.pid as i32, 0) } == 0;
        if bytes != record || owner_alive {
            return Err(io::Error::other(
                "Git is using the project index. Finish that operation, then retry.",
            ));
        }
        fs::remove_file(&lock_path)?;
    }
    let mut lock_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)?;
    let lock = IndexLock {
        path: lock_path,
        record: record.clone(),
    };
    lock_file.write_all(&record)?;
    let original = fs::read(root.join("sync-before-index"))?;
    let current = read_index(&index)?;
    let head = git(root, &state.project, &["rev-parse", "HEAD"], &flag)?;
    if head == state.after && current == pending {
        return clear(root);
    }
    if current != original
        || ![state.before.as_str(), state.after.as_str()].contains(&head.as_str())
    {
        return Err(io::Error::other(
            "Project commits or staging changed during synchronization. Saved state is retained.",
        ));
    }
    if head == state.before {
        let result = indexed(
            &state,
            root,
            &[
                "merge",
                "--ff-only",
                "--no-edit",
                "--no-stat",
                "--no-autostash",
                &state.after,
            ],
        );
        if git(root, &state.project, &["rev-parse", "HEAD"], &flag)? != state.after {
            clear(root)?;
            return Err(io::Error::other(format!(
                "Could not update {}. Local files and staging are retained. {}",
                state.branch.trim_start_matches("refs/heads/"),
                result.err().map_or_else(
                    || "Retry synchronization.".into(),
                    |error| error.to_string()
                )
            )));
        }
    }
    let next = index.with_file_name("index.sprowt-next");
    fs::write(&next, fs::read(root.join("sync-index"))?)?;
    fs::File::open(&next)?.sync_all()?;
    fs::rename(next, index)?;
    drop(lock);
    clear(root)
}

fn clear(root: &Path) -> io::Result<()> {
    fs::remove_file(root.join("sync-project.json"))?;
    for name in ["sync-before-index", "sync-index", "sync-files"] {
        let _ = fs::remove_file(root.join(name));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{git_mod, store::test_support::TestData};

    pub(crate) fn remote_change() -> (TestData, PathBuf, PathBuf, String) {
        let (data, repo, root, _) = git_mod::tests::fixture();
        let flag = AtomicBool::new(false);
        git(&root, &repo, &["push", "origin", "main"], &flag).unwrap();
        git(&root, &repo, &["checkout", "-b", "remote-change"], &flag).unwrap();
        fs::write(repo.join("a.txt"), "remote version\n").unwrap();
        fs::write(repo.join("new.txt"), "incoming\n").unwrap();
        git(&root, &repo, &["add", "."], &flag).unwrap();
        git(&root, &repo, &["commit", "-m", "Merged version"], &flag).unwrap();
        git(&root, &repo, &["push", "origin", "HEAD:main"], &flag).unwrap();
        let target = git(&root, &repo, &["rev-parse", "HEAD"], &flag).unwrap();
        git(&root, &repo, &["checkout", "main"], &flag).unwrap();
        (data, repo, root, target)
    }

    #[test]
    fn fast_forward_preserves_untracked_bytes_staging_and_deletions_without_filters() {
        for local in ["incoming\n", "local edit\n"] {
            let (data, repo, root, target) = remote_change();
            let flag = AtomicBool::new(false);
            fs::write(repo.join("new.txt"), local).unwrap();
            fs::write(repo.join("delete.txt"), "staged\n").unwrap();
            git(&root, &repo, &["add", "delete.txt"], &flag).unwrap();
            fs::write(repo.join("delete.txt"), "unstaged\n").unwrap();
            git(&root, &repo, &["rm", "--cached", ".gitattributes"], &flag).unwrap();
            fs::write(repo.join("local.txt"), "untracked\n").unwrap();
            let staged = git(&root, &repo, &["diff", "--cached", "--raw"], &flag).unwrap();
            let marker = data.0.join("filter-ran");
            let filter = format!("touch '{}'; cat", marker.display());
            for kind in ["clean", "smudge"] {
                git(
                    &root,
                    &repo,
                    &["config", &format!("filter.fixture.{kind}"), &filter],
                    &flag,
                )
                .unwrap();
            }
            project(&repo, &root, &flag).unwrap();
            assert_eq!(
                git(&root, &repo, &["rev-parse", "HEAD"], &flag).unwrap(),
                target
            );
            assert_eq!(fs::read_to_string(repo.join("new.txt")).unwrap(), local);
            assert_eq!(
                fs::read_to_string(repo.join("a.txt")).unwrap(),
                "remote version\n"
            );
            assert_eq!(
                fs::read_to_string(repo.join("delete.txt")).unwrap(),
                "unstaged\n"
            );
            assert_eq!(
                fs::read_to_string(repo.join("local.txt")).unwrap(),
                "untracked\n"
            );
            assert_eq!(
                git(&root, &repo, &["diff", "--cached", "--raw"], &flag).unwrap(),
                staged
            );
            assert!(!marker.exists());
            assert!(
                !repo.join(".git/index.lock").exists() && !root.join("sync-project.json").exists()
            );
        }
    }

    #[test]
    fn overlapping_edits_divergence_and_index_locks_stop_before_worktree_creation() {
        for case in ["overlap", "diverged", "index locked"] {
            let (_data, repo, root, _) = remote_change();
            let flag = AtomicBool::new(false);
            fs::write(repo.join("a.txt"), "local change\n").unwrap();
            fs::write(repo.join("new.txt"), "untracked incoming\n").unwrap();
            if case == "diverged" {
                git(&root, &repo, &["add", "a.txt"], &flag).unwrap();
                git(&root, &repo, &["commit", "-m", "Local version"], &flag).unwrap();
            } else if case == "index locked" {
                fs::write(repo.join(".git/index.lock"), "other Git operation").unwrap();
            }
            let head = git(&root, &repo, &["rev-parse", "HEAD"], &flag).unwrap();
            let index = fs::read(repo.join(".git/index")).unwrap();
            let error = project(&repo, &root, &flag).unwrap_err().to_string();
            assert!(error.contains(match case {
                "overlap" => "a.txt",
                "diverged" => "diverged",
                _ => "index",
            }));
            assert_eq!(
                git(&root, &repo, &["rev-parse", "HEAD"], &flag).unwrap(),
                head
            );
            assert_eq!(fs::read(repo.join(".git/index")).unwrap(), index);
            assert_eq!(
                fs::read_to_string(repo.join("a.txt")).unwrap(),
                "local change\n"
            );
            assert_eq!(
                fs::read_to_string(repo.join("new.txt")).unwrap(),
                "untracked incoming\n"
            );
            assert!(!git_mod::checkout(&root).exists());
        }
    }

    #[test]
    fn an_interrupted_fast_forward_finishes_its_index_once() {
        let (_data, repo, root, target) = remote_change();
        let flag = AtomicBool::new(false);
        fs::write(repo.join("new.txt"), "local change\n").unwrap();
        let mut state = Sync {
            project: repo.canonicalize().unwrap(),
            workspace: root.canonicalize().unwrap(),
            pid: std::process::id(),
            branch: "refs/heads/main".into(),
            before: git(&root, &repo, &["rev-parse", "HEAD"], &flag).unwrap(),
            after: target.clone(),
        };
        let index = fs::read(repo.join(".git/index")).unwrap();
        fs::write(root.join("sync-before-index"), &index).unwrap();
        fs::write(root.join("sync-index"), &index).unwrap();
        adopt_files(&state, &root).unwrap();
        let record = serde_json::to_vec(&state).unwrap();
        fs::write(root.join("sync-project.json"), &record).unwrap();
        fs::write(repo.join(".git/index.lock"), &record).unwrap();
        assert!(
            project(&repo, &root, &flag)
                .unwrap_err()
                .to_string()
                .contains("using the project index")
        );
        state.pid = i32::MAX as u32;
        let record = serde_json::to_vec(&state).unwrap();
        fs::write(root.join("sync-project.json"), &record).unwrap();
        fs::write(repo.join(".git/index.lock"), &record).unwrap();
        indexed(
            &state,
            &root,
            &["merge", "--ff-only", "--no-autostash", &target],
        )
        .unwrap();
        assert_eq!(fs::read(repo.join(".git/index")).unwrap(), index);
        project(&repo, &root, &flag).unwrap();
        assert_eq!(
            git(&root, &repo, &["rev-parse", "HEAD"], &flag).unwrap(),
            target
        );
        assert_eq!(
            git(&root, &repo, &["diff", "--cached", "--name-only"], &flag).unwrap(),
            ""
        );
        assert_eq!(
            fs::read_to_string(repo.join("new.txt")).unwrap(),
            "local change\n"
        );
        assert!(!repo.join(".git/index.lock").exists());
        project(&repo, &root, &flag).unwrap();
        assert_eq!(
            fs::read_to_string(repo.join("new.txt")).unwrap(),
            "local change\n"
        );
    }

    #[test]
    fn configured_upstream_and_local_only_branches_use_the_expected_source() {
        let (data, repo, root, target) = remote_change();
        let flag = AtomicBool::new(false);
        git(
            &root,
            &repo,
            &["push", "origin", &format!("{target}:refs/heads/trunk")],
            &flag,
        )
        .unwrap();
        git(
            &root,
            &repo,
            &["config", "branch.main.remote", "origin"],
            &flag,
        )
        .unwrap();
        git(
            &root,
            &repo,
            &["config", "branch.main.merge", "refs/heads/trunk"],
            &flag,
        )
        .unwrap();
        project(&repo, &root, &flag).unwrap();
        assert_eq!(
            git(&root, &repo, &["rev-parse", "HEAD"], &flag).unwrap(),
            target
        );
        git(&root, &repo, &["checkout", "-b", "local-only"], &flag).unwrap();
        project(&repo, &root, &flag).unwrap();
        git(&root, &repo, &["checkout", "--detach"], &flag).unwrap();
        project(&repo, &root, &flag).unwrap();
        assert_eq!(
            git(&root, &repo, &["rev-parse", "HEAD"], &flag).unwrap(),
            target
        );
        fs::remove_dir_all(data.0.join("remote.git")).unwrap();
        git(&root, &repo, &["checkout", "main"], &flag).unwrap();
        assert!(project(&repo, &root, &flag).is_err());
    }
}
