use std::{
    fs, io,
    path::{Component, Path, PathBuf},
    sync::atomic::AtomicBool,
};

use serde::{Deserialize, Serialize};

use crate::{
    git_mod::{self, git, run},
    plan::{Plan, Task},
    workspace::{self, Snapshot},
};

#[derive(Clone, Debug)]
pub struct Target {
    pub branch: String,
    pub head: String,
    pub pr_state: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct Update {
    pub branch: String,
    pub target: String,
    pub ours: String,
    pub tree: String,
    pub conflicts: Vec<String>,
    pub plan: Plan,
    pub context: String,
    pub prepared: bool,
    pub imported: bool,
    pub installed: bool,
    pub commit: Option<String>,
}

pub fn load(root: &Path) -> io::Result<Option<Update>> {
    match fs::read(root.join("main-update.json")) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn save(root: &Path, update: &Update) -> io::Result<()> {
    let path = root.join("main-update-next.json");
    fs::write(&path, serde_json::to_vec(update)?)?;
    git_mod::private(&path)?;
    fs::File::open(&path)?.sync_all()?;
    fs::rename(path, root.join("main-update.json"))
}

pub fn target(root: &Path, program: &Path, cancelled: &AtomicBool) -> io::Result<Option<Target>> {
    let state = git_mod::load(root)?;
    if !git_mod::has_origin(&state.repo) {
        return Ok(None);
    }
    let pr_state = if let Some(pr) = &state.pr {
        let info: serde_json::Value = serde_json::from_str(&git_mod::gh(
            root,
            &state,
            program,
            &["pr", "view", pr, "--json", "state,headRefOid,headRefName"],
            cancelled,
        )?)?;
        let status = info["state"]
            .as_str()
            .filter(|s| ["OPEN", "MERGED", "CLOSED"].contains(s))
            .ok_or_else(|| io::Error::other("Cannot read the PR status."))?;
        if status == "OPEN" {
            git_mod::open_pr(root, &state, program, cancelled)?;
        }
        Some(status.to_owned())
    } else {
        None
    };
    let branch = if let Some(branch) = &state.base_branch {
        branch.clone()
    } else {
        let info: serde_json::Value = serde_json::from_str(&git_mod::gh(
            root,
            &state,
            program,
            &["repo", "view", "--json", "defaultBranchRef"],
            cancelled,
        )?)?;
        info["defaultBranchRef"]["name"]
            .as_str()
            .ok_or_else(|| io::Error::other("Cannot identify the PR target branch."))?
            .to_owned()
    };
    let reference = format!("refs/heads/{branch}");
    git(
        root,
        &state.repo,
        &["check-ref-format", &reference],
        cancelled,
    )?;
    let tracking = format!("refs/sprowt/upstream/{}", state.branch);
    let mut command = crate::git_sync::network(&state.repo);
    command.args([
        "fetch",
        "--no-tags",
        "--no-recurse-submodules",
        "--no-write-fetch-head",
        "--",
        "origin",
        &format!("+{reference}:{tracking}"),
    ]);
    run(command, root, cancelled)?;
    let mut head = git(
        root,
        &state.repo,
        &["rev-parse", &format!("{tracking}^{{commit}}")],
        cancelled,
    )?;
    if git(
        root,
        &state.repo,
        &["merge-base", "--is-ancestor", &state.base, &head],
        cancelled,
    )
    .is_err()
    {
        if git(
            root,
            &state.repo,
            &["merge-base", "--is-ancestor", &head, &state.base],
            cancelled,
        )
        .is_ok()
        {
            // A locally ahead starting commit already includes this remote target.
            head = state.base.clone();
        } else {
            return Err(io::Error::other(
                "The target and saved starting point diverged. Work is retained; reconcile their history before updating.",
            ));
        }
    }
    Ok(Some(Target {
        branch,
        head,
        pr_state,
    }))
}

pub fn require_current(root: &Path, program: &Path, cancelled: &AtomicBool) -> io::Result<()> {
    if load(root)?.is_some() {
        return Err(io::Error::other(
            "The update from main needs verification before publication. Ctrl+R retries.",
        ));
    }
    if let Some(target) = target(root, program, cancelled)?
        && target.head != git_mod::load(root)?.base
    {
        hold_pr(root, program, cancelled)?;
        return Err(io::Error::other(format!(
            "{} changed. Ctrl+U updates this codemod and rechecks it before publication.",
            target.branch
        )));
    }
    Ok(())
}

pub fn hold_pr(root: &Path, program: &Path, cancelled: &AtomicBool) -> io::Result<()> {
    let mut state = git_mod::load(root)?;
    if state.pr.is_none() {
        return Ok(());
    }
    let pr = git_mod::open_pr(root, &state, program, cancelled)?;
    if pr["isDraft"] != true {
        git_mod::gh(
            root,
            &state,
            program,
            &["pr", "ready", state.pr.as_deref().unwrap(), "--undo"],
            cancelled,
        )?;
    }
    state.draft = true;
    git_mod::save(root, &state)
}

pub fn prepare(
    root: &Path,
    target: &Target,
    plan: &Plan,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    let mut state = git_mod::load(root)?;
    if load(root)?.is_none() {
        if target.pr_state.as_deref().is_some_and(|s| s != "OPEN") {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "This PR is merged or closed. Start a new codemod.",
            ));
        }
        if state.base == target.head {
            return Ok(());
        }
        if !["ready", "published"].contains(&state.phase.as_str())
            || state.publishing
            || state.closing
        {
            return Err(io::Error::other(
                "Finish the current Git operation before updating.",
            ));
        }
        git(
            root,
            &state.repo,
            &["merge-base", "--is-ancestor", &state.base, &target.head],
            cancelled,
        )
        .map_err(|_| io::Error::other("The target branch history changed. Work is retained."))?;
        let changed = git(
            root,
            &state.repo,
            &["diff", "--name-only", "-z", &state.base, &target.head],
            cancelled,
        )?;
        if changed
            .split('\0')
            .filter(|p| !p.is_empty())
            .any(|p| workspace::excluded(Path::new(p)))
        {
            return Err(io::Error::other(
                "Upstream changes protected files. Reconcile those changes outside the harness; work is retained.",
            ));
        }
        git_mod::save_source(root, cancelled)?;
        state = git_mod::load(root)?;
        let ours = state.checkpoint.clone().unwrap();
        let mut command = git_mod::safe_git(&state.repo)?;
        command.arg("-C").arg(&state.repo).args([
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            "-z",
            &ours,
            &target.head,
        ]);
        let output = match run(command, root, cancelled) {
            Ok(bytes) => bytes,
            Err(error) => {
                if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(error);
                }
                // A conflicted merge still writes a complete tree and the conflict paths.
                let bytes = fs::read(root.join("git-output"))?;
                if bytes.is_empty() {
                    return Err(error);
                }
                bytes
            }
        };
        let mut parts = output.split(|b| *b == 0).filter(|p| !p.is_empty());
        let tree = std::str::from_utf8(
            parts
                .next()
                .ok_or_else(|| io::Error::other("Missing merge tree."))?,
        )
        .map_err(io::Error::other)?
        .trim()
        .to_owned();
        let merged = snapshot(root, &state.repo, &tree, cancelled)?;
        let incoming = snapshot(root, &state.repo, &target.head, cancelled)?;
        let conflicts = parts
            .map(|p| std::str::from_utf8(p).map(str::to_owned))
            .collect::<Result<Vec<_>, _>>()
            .map_err(io::Error::other)?;
        for path in &conflicts {
            check_path(Path::new(path))?;
        }
        if !conflicts.is_empty() {
            let ours_source = snapshot(root, &state.repo, &ours, cancelled)?;
            for (path, bytes, _) in ours_source.iter().chain(&incoming) {
                if conflicts.iter().any(|p| Path::new(p) == path)
                    && (bytes.contains(&0) || std::str::from_utf8(bytes).is_err())
                {
                    return Err(io::Error::other(format!(
                        "Binary conflict in {}. Automatic resolution is unsupported; both commits and the VM are retained for manual reconciliation.",
                        path.display()
                    )));
                }
            }
        }
        let mut checks = plan
            .tasks
            .iter()
            .flat_map(|t| t.checks.clone())
            .collect::<Vec<_>>();
        checks.sort();
        checks.dedup();
        checks.push("Run the project's regression checks against the combined codemod and upstream changes.".into());
        checks.push(
            "No unresolved merge conflicts remain; both intended behaviors are preserved.".into(),
        );
        let update_plan = Plan {
            summary: format!("Update from {} and verify together", target.branch),
            tasks: vec![Task {
                id: "upstream".into(),
                title: if conflicts.is_empty() {
                    "Verify combined changes".into()
                } else {
                    "Resolve conflicts and verify combined changes".into()
                },
                outcome: "Keep the codemod and merged upstream behavior working together.".into(),
                files: plan
                    .tasks
                    .iter()
                    .flat_map(|t| t.files.clone())
                    .chain(
                        changed
                            .split('\0')
                            .filter(|p| !p.is_empty())
                            .map(str::to_owned),
                    )
                    .chain(conflicts.clone())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect(),
                depends_on: vec![],
                coordination: vec![],
                worker: "codex".into(),
                checks,
            }],
            ..plan.clone()
        };
        update_plan.validate().map_err(io::Error::other)?;
        let context = format!(
            "Update from {} at {}. Original codemod plan: {}\nUpstream goals:\n{}\nUpstream changes:\n{}\nConflicting versions:\n{}\nPreserve both goals. Resolve technical conflicts and fix integration regressions in the declared scope. If product intent conflicts or a binary resolution is unclear, return blocked with one concise question for the user. Never choose ours/theirs blindly. Clean merges still require regression checks. Do not modify unrelated files; if fixes need a broader scope, report blocked and request an edit. Run app and browser in the same command.",
            target.branch,
            target.head,
            serde_json::to_string(plan)?,
            git(
                root,
                &state.repo,
                &[
                    "log",
                    "--format=%s%n%b",
                    "-20",
                    &format!("{}..{}", state.base, target.head)
                ],
                cancelled
            )?
            .chars()
            .take(16000)
            .collect::<String>(),
            git(
                root,
                &state.repo,
                &[
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--stat",
                    &state.base,
                    &target.head
                ],
                cancelled
            )?,
            conflict_context(
                root,
                &state.repo,
                &state.base,
                &ours,
                &target.head,
                &conflicts,
                cancelled
            )?
        );
        let dir = root.join("main-update");
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        workspace::create_snapshot(&incoming, &dir)?;
        workspace::replace_source(&dir, &merged)?;
        save(
            root,
            &Update {
                branch: target.branch.clone(),
                target: target.head.clone(),
                ours,
                tree,
                conflicts,
                plan: update_plan,
                context,
                prepared: false,
                imported: false,
                installed: false,
                commit: None,
            },
        )?;
    }
    let mut update = load(root)?.unwrap();
    if !update.prepared {
        let dir = root.join("main-update");
        // The immutable staging snapshot makes interrupted baseline replacement repeatable.
        for name in ["before", "base.git"] {
            let path = root.join(name);
            if path.exists() {
                fs::remove_dir_all(&path)?;
            }
            copy_tree(&dir.join(name), &path)?;
        }
        workspace::replace_source(root, &workspace::source_state(&dir.join("work"))?)?;
        state.base = update.target.clone();
        state.phase = "ready".into();
        state.continuing = false;
        state.fingerprint = None;
        git_mod::save(root, &state)?;
        update.prepared = true;
        save(root, &update)?;
    }
    Ok(())
}

pub fn finish(root: &Path, fingerprint: &str, cancelled: &AtomicBool) -> io::Result<()> {
    let Some(mut update) = load(root)? else {
        return Ok(());
    };
    if !update.installed || !update.imported || !update.prepared {
        return Err(io::Error::other(
            "The combined source has not been checked in the VM.",
        ));
    }
    let source = workspace::source_state(&root.join("work"))?;
    if workspace::fingerprint(&source)? != fingerprint {
        return Err(io::Error::other("Source changed after verification."));
    }
    for (path, bytes, _) in &source {
        if update.conflicts.iter().any(|p| Path::new(p) == path)
            && bytes
                .split(|b| *b == b'\n')
                .any(|line| line.starts_with(b"<<<<<<< ") || line.starts_with(b">>>>>>> "))
        {
            return Err(io::Error::other(format!(
                "{} still has conflict markers. Publication is blocked.",
                path.display()
            )));
        }
    }
    let mut state = git_mod::load(root)?;
    git_mod::validate(root, &state, cancelled)?;
    let path = git_mod::checkout(root);
    let head = git(root, &path, &["rev-parse", "HEAD"], cancelled)?;
    if update.commit.is_none() {
        git_mod::transfer(root, &state, cancelled)?;
        // Preserve upstream entries excluded from VM source, including unchanged credentials.
        let index = root.join("main-update/merge-index");
        git_mod::git_index(
            root,
            &path,
            &["read-tree", &update.tree],
            Some(&index),
            cancelled,
        )?;
        let resolution = root.join("main-update/resolution");
        if !resolution.exists() {
            workspace::create_snapshot(
                &snapshot(root, &state.repo, &update.tree, cancelled)?,
                &resolution,
            )?;
        }
        workspace::replace_source(&resolution, &source)?;
        let review = workspace::review_snapshot(&resolution)?;
        git_mod::stage_index(root, &path, &review, Some(&index), cancelled)?;
        let tree = git_mod::git_index(root, &path, &["write-tree"], Some(&index), cancelled)?;
        let commit = git(
            root,
            &path,
            &[
                "commit-tree",
                &tree,
                "-p",
                &head,
                "-p",
                &update.target,
                "-m",
                &format!("Update codemod from {}", update.branch),
            ],
            cancelled,
        )?;
        update.commit = Some(commit);
        save(root, &update)?;
    }
    let commit = update.commit.as_ref().unwrap();
    if head != *commit {
        if head
            != *state
                .checkpoint
                .as_ref()
                .or(state.head.as_ref())
                .unwrap_or(&update.ours)
        {
            return Err(io::Error::other(
                "The codemod branch changed during integration. Work is retained.",
            ));
        }
        git(
            root,
            &path,
            &["update-ref", "HEAD", commit, &head],
            cancelled,
        )?;
    }
    git(root, &path, &["read-tree", commit], cancelled)?;
    state.head = Some(commit.clone());
    state.checkpoint = Some(commit.clone());
    state.phase = "ready".into();
    state.fingerprint = None;
    git_mod::save(root, &state)?;
    for dir in ["transfer", "main-update"] {
        let path = root.join(dir);
        if path.exists() {
            fs::remove_dir_all(&path)?;
        }
    }
    fs::remove_file(root.join("main-update.json"))
}

fn check_path(path: &Path) -> io::Result<()> {
    if path
        .components()
        .any(|p| !matches!(p, Component::Normal(_)))
        || workspace::excluded(path)
    {
        return Err(io::Error::other(
            "Upstream conflict touches a protected path. Resolve it outside this codemod.",
        ));
    }
    Ok(())
}

pub fn snapshot(
    root: &Path,
    repo: &Path,
    reference: &str,
    cancelled: &AtomicBool,
) -> io::Result<Snapshot> {
    let mut command = workspace::trusted_git();
    command
        .arg("-C")
        .arg(repo)
        .args(["ls-tree", "-r", "-z", reference]);
    let output = run(command, root, cancelled)?;
    let mut files = Vec::new();
    for entry in output.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let (meta, name) = entry.split_at(
            entry
                .iter()
                .position(|b| *b == b'\t')
                .ok_or_else(|| io::Error::other("Invalid tree entry."))?,
        );
        let meta = std::str::from_utf8(meta)
            .map_err(io::Error::other)?
            .split_whitespace()
            .collect::<Vec<_>>();
        if meta.len() != 3 || !["100644", "100755"].contains(&meta[0]) {
            return Err(io::Error::other(
                "Upstream contains links or submodules. Work is retained.",
            ));
        }
        let path = PathBuf::from(std::str::from_utf8(&name[1..]).map_err(io::Error::other)?);
        if workspace::excluded(&path) {
            continue;
        }
        check_path(&path)?;
        files.push((
            path,
            meta[2].to_owned(),
            if meta[0] == "100755" { 0o755 } else { 0o644 },
        ));
    }
    let input = root.join("sync-objects");
    fs::write(
        &input,
        files
            .iter()
            .map(|(_, hash, _)| format!("{hash}\n"))
            .collect::<String>(),
    )?;
    git_mod::private(&input)?;
    let mut command = workspace::trusted_git();
    command
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "--batch"])
        .stdin(fs::File::open(&input)?);
    let output = run(command, root, cancelled)?;
    let mut remaining = output.as_slice();
    let mut source = Vec::new();
    for (path, _, mode) in files {
        let newline = remaining
            .iter()
            .position(|b| *b == b'\n')
            .ok_or_else(|| io::Error::other("Invalid blob response."))?;
        let size: usize = std::str::from_utf8(&remaining[..newline])
            .map_err(io::Error::other)?
            .split_whitespace()
            .nth(2)
            .ok_or_else(|| io::Error::other("Missing blob size."))?
            .parse()
            .map_err(io::Error::other)?;
        remaining = &remaining[newline + 1..];
        if remaining.len() <= size {
            return Err(io::Error::other("Truncated blob response."));
        }
        source.push((path, remaining[..size].to_vec(), mode));
        remaining = &remaining[size + 1..];
    }
    Ok(source)
}

fn conflict_context(
    root: &Path,
    repo: &Path,
    base: &str,
    ours: &str,
    target: &str,
    paths: &[String],
    cancelled: &AtomicBool,
) -> io::Result<String> {
    if paths.is_empty() {
        return Ok(String::new());
    }
    let mut context = String::new();
    let versions = [
        ("base", snapshot(root, repo, base, cancelled)?),
        ("codemod", snapshot(root, repo, ours, cancelled)?),
        ("upstream", snapshot(root, repo, target, cancelled)?),
    ];
    for path in paths {
        for (label, snapshot) in &versions {
            let bytes = snapshot
                .iter()
                .find(|(p, _, _)| p == Path::new(path))
                .map(|(_, bytes, _)| bytes);
            let text: String = match bytes {
                Some(bytes) if !bytes.contains(&0) => {
                    String::from_utf8_lossy(bytes).chars().take(16000).collect()
                }
                Some(_) => {
                    "[binary file; ask the user if the correct resolution is unclear]".into()
                }
                None => "[file absent]".into(),
            };
            context.push_str(&format!("\n{path} · {label}:\n{text}\n"));
        }
    }
    Ok(context)
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::store::test_support::TestData;

    pub(crate) fn fixture() -> (TestData, PathBuf, PathBuf, Target, Plan) {
        let (data, repo, root, head) = crate::git_sync::tests::remote_change();
        let flag = AtomicBool::new(false);
        git_mod::prepare(&repo, &root, &flag).unwrap();
        workspace::create(&git_mod::checkout(&root), &root).unwrap();
        let target = Target {
            branch: "main".into(),
            head,
            pr_state: None,
        };
        let plan = Plan {
            summary: "Improve deletion".into(),
            tasks: vec![Task {
                id: "delete".into(),
                title: "Improve deletion".into(),
                outcome: "Deletion works".into(),
                files: vec!["delete.txt".into()],
                depends_on: vec![],
                coordination: vec![],
                worker: "codex".into(),
                checks: vec!["Deletion check passes".into()],
            }],
            ..Plan::default()
        };
        (data, repo, root, target, plan)
    }

    fn verified(root: &Path) -> String {
        let mut update = load(root).unwrap().unwrap();
        update.installed = true;
        update.imported = true;
        save(root, &update).unwrap();
        workspace::review(root).unwrap().fingerprint
    }

    #[test]
    fn clean_updates_keep_both_changes_and_publish_only_the_codemod_diff() {
        let (_data, repo, root, target, plan) = fixture();
        let flag = AtomicBool::new(false);
        let main = git(&root, &repo, &["rev-parse", "main"], &flag).unwrap();
        fs::write(root.join("work/delete.txt"), "codemod version\n").unwrap();
        let fetched = super::target(&root, Path::new("gh"), &flag)
            .unwrap()
            .unwrap();
        assert_eq!(fetched.head, target.head);
        assert!(require_current(&root, Path::new("gh"), &flag).is_err());
        prepare(&root, &target, &plan, &flag).unwrap();
        let update = load(&root).unwrap().unwrap();
        assert!(update.conflicts.is_empty());
        assert!(update.plan.tasks[0].files.contains(&"delete.txt".into()));
        assert!(
            update.plan.tasks[0]
                .checks
                .iter()
                .any(|c| c == "Deletion check passes")
        );
        assert_eq!(
            fs::read_to_string(root.join("work/a.txt")).unwrap(),
            "remote version\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("work/delete.txt")).unwrap(),
            "codemod version\n"
        );
        assert_eq!(
            git(&root, &repo, &["rev-parse", "main"], &flag).unwrap(),
            main
        );
        assert_eq!(
            fs::read_to_string(repo.join("a.txt")).unwrap(),
            "original\n"
        );
        assert!(!root.join("work/.env").exists());
        assert!(require_current(&root, Path::new("gh"), &flag).is_err());
        let fingerprint = verified(&root);
        finish(&root, &fingerprint, &flag).unwrap();
        assert!(load(&root).unwrap().is_none());
        let state = git_mod::load(&root).unwrap();
        assert_eq!(state.base, target.head);
        assert_eq!(
            git(
                &root,
                &git_mod::checkout(&root),
                &["rev-parse", "HEAD^2"],
                &flag
            )
            .unwrap(),
            target.head
        );
        let review = workspace::review(&root).unwrap();
        assert_eq!(
            review.paths().collect::<Vec<_>>(),
            [Path::new("delete.txt")]
        );
        assert_eq!(review.fingerprint, fingerprint);
        assert!(
            git(
                &root,
                &git_mod::checkout(&root),
                &["status", "--porcelain"],
                &flag
            )
            .unwrap()
            .is_empty()
        );
        require_current(&root, Path::new("gh"), &flag).unwrap();
        let tracking = format!("refs/sprowt/upstream/{}", state.branch);
        git_mod::mark_closing(&root, true, true).unwrap();
        git_mod::cleanup(&root, &flag).unwrap();
        assert!(git(&root, &repo, &["show-ref", "--verify", &tracking], &flag).is_err());
    }

    #[test]
    fn a_locally_ahead_start_already_contains_the_remote_target() {
        let (_data, repo, root, target, _plan) = fixture();
        let flag = AtomicBool::new(false);
        git(
            &root,
            &repo,
            &["checkout", "-b", "local-start", &target.head],
            &flag,
        )
        .unwrap();
        fs::write(repo.join("local.txt"), "local starting commit\n").unwrap();
        git(&root, &repo, &["add", "local.txt"], &flag).unwrap();
        git(
            &root,
            &repo,
            &["commit", "-m", "Local starting source"],
            &flag,
        )
        .unwrap();
        let mut state = git_mod::load(&root).unwrap();
        state.base = git(&root, &repo, &["rev-parse", "HEAD"], &flag).unwrap();
        git_mod::save(&root, &state).unwrap();
        assert_eq!(
            super::target(&root, Path::new("gh"), &flag)
                .unwrap()
                .unwrap()
                .head,
            state.base
        );
        require_current(&root, Path::new("gh"), &flag).unwrap();
    }

    #[test]
    fn conflicts_keep_versions_and_block_markers_until_resolution_is_checked() {
        for resolution in ["both behaviors\n", "remote version\n"] {
            let (_data, _repo, root, target, plan) = fixture();
            let flag = AtomicBool::new(false);
            fs::write(root.join("work/a.txt"), "codemod version\n").unwrap();
            prepare(&root, &target, &plan, &flag).unwrap();
            let update = load(&root).unwrap().unwrap();
            assert_eq!(update.conflicts, ["a.txt"]);
            for version in ["original", "codemod version", "remote version"] {
                assert!(update.context.contains(version));
            }
            let merged = fs::read_to_string(root.join("work/a.txt")).unwrap();
            assert!(
                merged.contains("<<<<<<<")
                    && merged.contains("codemod version")
                    && merged.contains("remote version")
            );
            let fingerprint = verified(&root);
            assert!(
                finish(&root, &fingerprint, &flag)
                    .unwrap_err()
                    .to_string()
                    .contains("conflict markers")
            );
            fs::write(root.join("work/a.txt"), resolution).unwrap();
            assert!(finish(&root, &fingerprint, &flag).is_err());
            let fingerprint = verified(&root);
            finish(&root, &fingerprint, &flag).unwrap();
            assert_eq!(
                fs::read_to_string(git_mod::checkout(&root).join("a.txt")).unwrap(),
                resolution
            );
            assert_eq!(
                git(
                    &root,
                    &git_mod::checkout(&root),
                    &["show", "HEAD:a.txt"],
                    &flag
                )
                .unwrap(),
                resolution.trim()
            );
        }
    }

    #[test]
    fn interrupted_updates_resume_without_overwriting_a_resolution_or_losing_ancestry() {
        let (_data, _repo, root, target, plan) = fixture();
        let flag = AtomicBool::new(false);
        fs::write(root.join("work/a.txt"), "codemod version\n").unwrap();
        prepare(&root, &target, &plan, &flag).unwrap();
        let mut update = load(&root).unwrap().unwrap();
        update.prepared = false;
        save(&root, &update).unwrap();
        fs::remove_dir_all(root.join("before")).unwrap();
        prepare(&root, &target, &plan, &flag).unwrap();
        fs::write(root.join("work/a.txt"), "resolved\n").unwrap();
        prepare(&root, &target, &plan, &flag).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("work/a.txt")).unwrap(),
            "resolved\n"
        );
        let fingerprint = verified(&root);
        let mut update = load(&root).unwrap().unwrap();
        git_mod::transfer(&root, &git_mod::load(&root).unwrap(), &flag).unwrap();
        let path = git_mod::checkout(&root);
        let tree = git(&root, &path, &["write-tree"], &flag).unwrap();
        let commit = git(
            &root,
            &path,
            &[
                "commit-tree",
                &tree,
                "-p",
                &update.ours,
                "-p",
                &update.target,
                "-m",
                "Recovered merge",
            ],
            &flag,
        )
        .unwrap();
        update.commit = Some(commit.clone());
        save(&root, &update).unwrap();
        git(
            &root,
            &path,
            &["update-ref", "HEAD", &commit, &update.ours],
            &flag,
        )
        .unwrap();
        finish(&root, &fingerprint, &flag).unwrap();
        assert_eq!(
            git(&root, &path, &["rev-parse", "HEAD"], &flag).unwrap(),
            commit
        );
        assert!(load(&root).unwrap().is_none());
    }

    #[test]
    fn upstream_tracked_artifacts_survive_the_save_copy_and_merge() {
        let (_data, repo, root, mut target, plan) = fixture();
        let flag = AtomicBool::new(false);
        git(&root, &repo, &["checkout", "remote-change"], &flag).unwrap();
        let files = [
            ".pytest_cache/v/cache/nodeids",
            "fixture.sqlite",
            "app.egg-info/PKG-INFO",
        ];
        for name in files {
            fs::create_dir_all(repo.join(name).parent().unwrap()).unwrap();
            fs::write(repo.join(name), "committed source\n").unwrap();
            git(&root, &repo, &["add", "--force", "--", name], &flag).unwrap();
        }
        git(
            &root,
            &repo,
            &["commit", "-m", "Add tracked fixtures"],
            &flag,
        )
        .unwrap();
        target.head = git(&root, &repo, &["rev-parse", "HEAD"], &flag).unwrap();
        fs::write(root.join("work/delete.txt"), "codemod version\n").unwrap();
        prepare(&root, &target, &plan, &flag).unwrap();
        let fingerprint = verified(&root);
        let transfer = root.join("transfer");
        let path = git_mod::checkout(&root);
        workspace::create_snapshot(&workspace::source_state(&path).unwrap(), &transfer).unwrap();
        workspace::replace_source(
            &transfer,
            &workspace::source_state(&root.join("work")).unwrap(),
        )
        .unwrap();
        // Reuse a save copy whose older baseline incorrectly filtered upstream files.
        assert_ne!(
            workspace::review(&transfer).unwrap().fingerprint,
            fingerprint
        );
        finish(&root, &fingerprint, &flag).unwrap();
        for name in files {
            assert_eq!(
                fs::read_to_string(path.join(name)).unwrap(),
                "committed source\n"
            );
            assert_eq!(
                git(&root, &path, &["show", &format!("HEAD:{name}")], &flag).unwrap(),
                "committed source"
            );
        }
        assert_eq!(
            git(
                &root,
                &path,
                &["diff", "--name-only", &target.head, "HEAD"],
                &flag
            )
            .unwrap(),
            "delete.txt"
        );
        assert_eq!(
            git(&root, &path, &["rev-parse", "HEAD^2"], &flag).unwrap(),
            target.head
        );
        assert!(load(&root).unwrap().is_none());
        assert!(!transfer.exists());
    }

    #[test]
    fn protected_upstream_files_and_outside_edits_stop_before_replacing_source() {
        let (_data, repo, root, mut target, plan) = fixture();
        let flag = AtomicBool::new(false);
        fs::write(git_mod::checkout(&root).join("a.txt"), "outside edit\n").unwrap();
        assert!(prepare(&root, &target, &plan, &flag).is_err());
        assert!(!root.join("main-update.json").exists());
        fs::write(git_mod::checkout(&root).join("a.txt"), "original\n").unwrap();
        git(&root, &repo, &["checkout", "remote-change"], &flag).unwrap();
        fs::write(repo.join(".env"), "new-fixture-secret\n").unwrap();
        git(&root, &repo, &["add", ".env"], &flag).unwrap();
        git(&root, &repo, &["commit", "-m", "Protected update"], &flag).unwrap();
        target.head = git(&root, &repo, &["rev-parse", "HEAD"], &flag).unwrap();
        assert!(
            prepare(&root, &target, &plan, &flag)
                .unwrap_err()
                .to_string()
                .contains("protected files")
        );
        assert!(!root.join("main-update.json").exists());
    }

    #[test]
    fn interrupted_merge_index_rebuilds_and_modify_delete_conflicts_keep_both_versions() {
        let (_data, repo, root, mut target, plan) = fixture();
        let flag = AtomicBool::new(false);
        git(&root, &repo, &["checkout", "remote-change"], &flag).unwrap();
        git(&root, &repo, &["rm", "delete.txt"], &flag).unwrap();
        git(
            &root,
            &repo,
            &["commit", "-m", "Remove obsolete file"],
            &flag,
        )
        .unwrap();
        target.head = git(&root, &repo, &["rev-parse", "HEAD"], &flag).unwrap();
        fs::write(root.join("work/delete.txt"), "codemod edit\n").unwrap();
        prepare(&root, &target, &plan, &flag).unwrap();
        let update = load(&root).unwrap().unwrap();
        assert!(update.conflicts.contains(&"delete.txt".into()));
        assert!(update.context.contains("[file absent]"));
        fs::remove_file(root.join("work/delete.txt")).unwrap();
        let fingerprint = verified(&root);
        git_mod::transfer(&root, &git_mod::load(&root).unwrap(), &flag).unwrap();
        let path = git_mod::checkout(&root);
        let index = root.join("main-update/merge-index");
        git_mod::git_index(
            &root,
            &path,
            &["read-tree", &update.tree],
            Some(&index),
            &flag,
        )
        .unwrap();
        // Simulate interruption with only part of the private merge index staged.
        git_mod::git_index(
            &root,
            &path,
            &["update-index", "--force-remove", "--", "new.txt"],
            Some(&index),
            &flag,
        )
        .unwrap();
        finish(&root, &fingerprint, &flag).unwrap();
        assert!(!path.join("delete.txt").exists());
        assert_eq!(
            git(&root, &path, &["show", "HEAD:new.txt"], &flag).unwrap(),
            "incoming"
        );
        assert_eq!(
            git(&root, &path, &["rev-parse", "HEAD^2"], &flag).unwrap(),
            target.head
        );
    }

    #[test]
    fn binary_conflicts_pause_without_guessing_or_losing_either_commit() {
        let (_data, repo, root, mut target, plan) = fixture();
        let flag = AtomicBool::new(false);
        git(&root, &repo, &["checkout", "remote-change"], &flag).unwrap();
        fs::write(repo.join("a.txt"), b"remote\0bytes").unwrap();
        git(&root, &repo, &["add", "a.txt"], &flag).unwrap();
        git(&root, &repo, &["commit", "-m", "Binary change"], &flag).unwrap();
        target.head = git(&root, &repo, &["rev-parse", "HEAD"], &flag).unwrap();
        fs::write(root.join("work/a.txt"), b"codemod\0bytes").unwrap();
        assert!(
            prepare(&root, &target, &plan, &flag)
                .unwrap_err()
                .to_string()
                .contains("Binary conflict")
        );
        assert!(!root.join("main-update.json").exists());
        assert_eq!(
            fs::read(root.join("work/a.txt")).unwrap(),
            b"codemod\0bytes"
        );
        assert_eq!(
            fs::read(git_mod::checkout(&root).join("a.txt")).unwrap(),
            b"codemod\0bytes"
        );
        assert!(git_mod::load(&root).unwrap().checkpoint.is_some());
    }

    #[test]
    fn a_published_pr_becomes_draft_and_republishes_with_merge_ancestry() {
        let (data, repo, root, program) = crate::git_mod::tests::fixture();
        let flag = AtomicBool::new(false);
        git(&root, &repo, &["push", "origin", "main"], &flag).unwrap();
        git_mod::prepare(&repo, &root, &flag).unwrap();
        workspace::create(&git_mod::checkout(&root), &root).unwrap();
        fs::write(root.join("work/delete.txt"), "codemod version\n").unwrap();
        let pr = git_mod::publish(&root, "Improve deletion", false, &program, &flag).unwrap();
        git(&root, &repo, &["checkout", "-b", "other-codemod"], &flag).unwrap();
        fs::write(repo.join("a.txt"), "merged first codemod\n").unwrap();
        git(&root, &repo, &["add", "a.txt"], &flag).unwrap();
        git(&root, &repo, &["commit", "-m", "First codemod"], &flag).unwrap();
        git(&root, &repo, &["push", "origin", "HEAD:main"], &flag).unwrap();
        git(&root, &repo, &["checkout", "main"], &flag).unwrap();
        assert!(require_current(&root, &program, &flag).is_err());
        assert!(data.0.join("pr-draft").exists());
        let target = target(&root, &program, &flag).unwrap().unwrap();
        let plan = Plan {
            summary: "Improve deletion".into(),
            tasks: vec![Task {
                id: "delete".into(),
                title: "Improve deletion".into(),
                outcome: "Deletion works".into(),
                files: vec!["delete.txt".into()],
                depends_on: vec![],
                coordination: vec![],
                worker: "codex".into(),
                checks: vec!["Deletion works".into()],
            }],
            ..Plan::default()
        };
        prepare(&root, &target, &plan, &flag).unwrap();
        let fingerprint = verified(&root);
        finish(&root, &fingerprint, &flag).unwrap();
        require_current(&root, &program, &flag).unwrap();
        assert_eq!(
            git_mod::publish(&root, "Improve deletion", false, &program, &flag).unwrap(),
            pr
        );
        assert!(!data.0.join("pr-draft").exists());
        fs::write(data.0.join("pr-state"), "MERGED").unwrap();
        assert_eq!(
            super::target(&root, &program, &flag)
                .unwrap()
                .unwrap()
                .pr_state
                .as_deref(),
            Some("MERGED")
        );
    }

    #[test]
    fn updating_keeps_queues_drafts_history_and_recovery_task_ids() {
        let (data, repo, root, target, plan) = fixture();
        let flag = AtomicBool::new(false);
        let mut store = data.store();
        let project = store.load_project(&repo).unwrap();
        let code_mod = store.create_mod(project.id, "Improve deletion").unwrap();
        let other = store.create_mod(project.id, "Other goal").unwrap();
        store.save_git_root(code_mod.id, &root).unwrap();
        store.enqueue(code_mod.id, "later edit").unwrap();
        store.save_draft(code_mod.id, "keep draft").unwrap();
        store
            .save_plan(
                code_mod.id,
                &code_mod.planning.as_ref().unwrap().source,
                &plan,
            )
            .unwrap();
        store.create_execution(code_mod.id, &root, &plan).unwrap();
        let original = store.execution(code_mod.id).unwrap().unwrap().tasks[0].id;
        fs::write(root.join("work/a.txt"), "codemod version\n").unwrap();
        prepare(&root, &target, &plan, &flag).unwrap();
        let update = load(&root).unwrap().unwrap();
        store.install_update(code_mod.id, &root, &update).unwrap();
        let execution = store.execution(code_mod.id).unwrap().unwrap();
        let worker = store
            .worker_for(code_mod.id, crate::plan::Role::Executor)
            .unwrap();
        let input = store
            .task_input(code_mod.id, worker.id, &update.plan)
            .unwrap()
            .unwrap();
        assert!(
            input.texts[0].contains("codemod version") && input.texts[0].contains("return blocked")
        );
        store.install_update(code_mod.id, &root, &update).unwrap();
        let restored = data.store().load_project(&repo).unwrap();
        let code_mod = restored.mods.iter().find(|m| m.id == code_mod.id).unwrap();
        assert_eq!(code_mod.draft, "keep draft");
        assert_eq!(code_mod.queue[0].body, "later edit");
        assert_ne!(execution.tasks[0].id, original);
        assert_eq!(
            code_mod.execution.as_ref().unwrap().tasks[0].id,
            execution.tasks[0].id
        );
        assert_eq!(
            code_mod.execution.as_ref().unwrap().tasks[0].status,
            "sending"
        );
        assert_eq!(code_mod.description, "Improve deletion");
        assert!(
            restored
                .mods
                .iter()
                .find(|m| m.id == other.id)
                .unwrap()
                .execution
                .is_none()
        );
    }

    #[test]
    #[ignore = "resolves an upstream conflict with a real Codex worker in a retained VM"]
    fn upstream_conflict_resolves_in_the_existing_vm_and_keeps_runtime() {
        use crate::{
            execution::Check,
            plan::Role,
            sandbox::Sandbox,
            tools::Context,
            worker::{Status, Worker},
        };
        let (data, repo, root, target, mut plan) = fixture();
        let flag = AtomicBool::new(false);
        plan.summary = "Keep the codemod greeting".into();
        plan.tasks[0].files = vec!["a.txt".into()];
        plan.tasks[0].checks = vec!["a.txt contains the codemod greeting".into()];
        fs::write(root.join("work/a.txt"), "codemod version\n").unwrap();
        let mut store = data.store();
        let project = store.load_project(&repo).unwrap();
        let mut code_mod = store.create_mod(project.id, "Keep both greeting lines in a.txt: codemod version first, then remote version. Preserve new.txt and delete.txt. No packages are needed; verify with the shell.").unwrap();
        store.save_git_root(code_mod.id, &root).unwrap();
        code_mod.git_root = Some(root.clone());
        store
            .save_plan(
                code_mod.id,
                &code_mod.planning.as_ref().unwrap().source,
                &plan,
            )
            .unwrap();
        store.create_execution(code_mod.id, &root, &plan).unwrap();
        let record = store.worker_for(code_mod.id, Role::Executor).unwrap();
        let original = store.execution(code_mod.id).unwrap().unwrap().tasks[0].clone();
        let runtime = format!("/home/sprowt/workers/{}/cache-proof", record.id);
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> io::Result<()> {
                let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
                vm.prepare_tasks(&[original.id], &flag)?;
                vm.assign_task(original.id, record.id, &flag)?;
                vm.guest(
                    &["/bin/sh", "-c", &format!("printf retained > {runtime}")],
                    &flag,
                )?;
                let check = Check {
                    task: Some(original.id),
                    check: plan.tasks[0].checks[0].clone(),
                    command: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        "grep -qx 'codemod version' a.txt".into(),
                    ],
                };
                assert_eq!(
                    vm.verify_execution(&original.source, std::slice::from_ref(&check), &flag)?
                        .1[0]
                        .exit_code,
                    Some(0)
                );
                assert_eq!(
                    vm.verify_execution("final:fixture", &[check], &flag)?.1[0].exit_code,
                    Some(0)
                );
                drop(vm);
                let before = fs::read(root.join("vm.json"))?;
                prepare(&root, &target, &plan, &flag)?;
                let mut update = load(&root)?.unwrap();
                store
                    .install_update(code_mod.id, &root, &update)
                    .map_err(io::Error::other)?;
                update.installed = true;
                save(&root, &update)?;
                code_mod = store
                    .load_project(&repo)
                    .map_err(io::Error::other)?
                    .mods
                    .remove(0);
                assert_eq!(fs::read(root.join("vm.json"))?, before);
                assert!(
                    Context::worker(&code_mod, record.id, Role::Executor)
                        .workspace()
                        .is_some()
                );
                let mut worker = Worker::start(&repo, &code_mod, record, Role::Executor, None)?;
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(240);
                while std::time::Instant::now() < deadline
                    && worker.status != Status::Failed
                    && code_mod.execution.as_ref().unwrap().status != "review"
                {
                    worker
                        .poll(&mut store, &mut code_mod, true, &git_mod::checkout(&root))
                        .map_err(io::Error::other)?;
                    if code_mod.execution.as_ref().unwrap().status == "blocked" {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                assert_eq!(
                    code_mod.execution.as_ref().unwrap().status,
                    "review",
                    "{:?}",
                    worker.error
                );
                drop(worker);
                let fingerprint = code_mod
                    .execution
                    .as_ref()
                    .unwrap()
                    .fingerprint
                    .as_deref()
                    .unwrap();
                finish(&root, fingerprint, &flag)?;
                assert_eq!(
                    fs::read_to_string(root.join("work/a.txt"))?,
                    "codemod version\nremote version\n"
                );
                assert_eq!(fs::read_to_string(root.join("work/new.txt"))?, "incoming\n");
                let vm = Sandbox::prepare(&root, &flag, |_| {})?;
                assert!(vm.guest_status(&[
                    "/bin/sh",
                    "-c",
                    &format!("test \"$(cat {runtime})\" = retained")
                ])?);
                drop(vm);
                assert_eq!(
                    git(
                        &root,
                        &git_mod::checkout(&root),
                        &["rev-parse", "HEAD^2"],
                        &flag
                    )?,
                    target.head
                );
                Ok(())
            }));
        crate::sandbox::delete(&root).unwrap();
        result.unwrap().unwrap();
    }
}
