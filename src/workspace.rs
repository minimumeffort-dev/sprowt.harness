use std::{
    collections::BTreeSet,
    fs, io,
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command, Output, Stdio},
};

#[derive(PartialEq)]
struct File {
    bytes: Vec<u8>,
    mode: u32,
}

struct Change {
    path: PathBuf,
    before: Option<File>,
    after: Option<File>,
}

pub struct Review {
    pub patch: String,
    pub summary: String,
    pub fingerprint: String,
    changes: Vec<Change>,
}

pub type Snapshot = Vec<(PathBuf, Vec<u8>, u32)>;

pub fn fingerprint(snapshot: &Snapshot) -> io::Result<String> {
    let mut child = trusted_git()
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    serde_json::to_writer(child.stdin.take().unwrap(), snapshot).map_err(io::Error::other)?;
    let output = child.wait_with_output()?;
    checked(&output)?;
    Ok(String::from_utf8(output.stdout)
        .map_err(io::Error::other)?
        .trim()
        .to_owned())
}

pub fn source_state(root: &Path) -> io::Result<Snapshot> {
    walk(root)?
        .into_iter()
        .map(|path| {
            let file = read_file(root, &path)?.ok_or_else(|| {
                io::Error::other("A source file disappeared during verification.")
            })?;
            Ok((path, file.bytes, file.mode))
        })
        .collect()
}

impl Review {
    pub fn count(&self) -> usize {
        self.changes.len()
    }

    pub fn apply(&self, project: &Path, workspace: &Path) -> io::Result<()> {
        self.apply_inner(project, workspace, false)
    }

    pub fn export(&self, project: &Path, workspace: &Path) -> io::Result<()> {
        self.apply_inner(project, workspace, true)
    }

    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.changes.iter().map(|change| change.path.as_path())
    }

    fn apply_inner(&self, project: &Path, workspace: &Path, resume: bool) -> io::Result<()> {
        if fingerprint(&source_state(&workspace.join("work"))?)? != self.fingerprint {
            return Err(io::Error::other(
                "The working folder changed after review. Reopen the diff.",
            ));
        }
        for change in &self.changes {
            let current = read_file(project, &change.path)?;
            if current != change.before && !(resume && current == change.after) {
                return Err(io::Error::other(format!(
                    "{} changed in the project. Nothing was applied.",
                    change.path.display()
                )));
            }
            if read_file(&workspace.join("work"), &change.path)? != change.after {
                return Err(io::Error::other(
                    "The working folder changed after review. Reopen the diff.",
                ));
            }
        }
        for (index, change) in self.changes.iter().enumerate() {
            if resume && read_file(project, &change.path)? == change.after {
                continue;
            }
            if let Err(error) = write_file(project, &change.path, change.after.as_ref()) {
                for previous in self.changes[..index].iter().rev() {
                    if let Err(rollback) =
                        write_file(project, &previous.path, previous.before.as_ref())
                    {
                        return Err(io::Error::other(format!(
                            "Apply failed: {error}; restore failed: {rollback}. The starting files remain in {}.",
                            workspace.join("before").display()
                        )));
                    }
                }
                return Err(error);
            }
        }
        Ok(())
    }
}

pub fn create(project: &Path, root: &Path) -> io::Result<()> {
    if root.starts_with(project) {
        return Err(io::Error::other(
            "The working folder must be outside the project.",
        ));
    }
    let existed = root.exists();
    if existed {
        if root.join("before").exists() || root.join("work").exists() {
            return Err(io::Error::other("A source snapshot already exists."));
        }
    } else {
        fs::create_dir(root)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    }
    let result = (|| {
        fs::create_dir(root.join("before"))?;
        fs::create_dir(root.join("work"))?;
        let paths = if project.join(".git").exists() {
            let output = trusted_git()
                .arg("-C")
                .arg(project)
                .args([
                    "ls-files",
                    "-z",
                    "--cached",
                    "--others",
                    "--exclude-standard",
                ])
                .output()?;
            checked(&output)?;
            paths(&output.stdout)?
        } else {
            walk(project)?
        };
        for path in paths.into_iter().filter(|path| !excluded(path)) {
            if let Some(file) = read_file(project, &path)? {
                write_file(&root.join("before"), &path, Some(&file))?;
                write_file(&root.join("work"), &path, Some(&file))?;
            }
        }
        checked(
            &trusted_git()
                .args(["init", "--bare"])
                .arg(root.join("base.git"))
                .output()?,
        )?;
        fs::write(
            root.join("base.git/info/exclude"),
            "node_modules/\ntarget/\n.venv/\nvenv/\n__pycache__/\n*.pyc\n.codex/\n.env*\n!.env.example\n!.env.sample\n.npmrc\n.netrc\n.pypirc\n.DS_Store\n",
        )?;
        run_git(root, &["add", "--force", "--all"])?;
        run_git(
            root,
            &["commit", "--allow-empty", "-m", "Starting project snapshot"],
        )?;
        Ok(())
    })();
    if result.is_err() {
        for name in ["before", "work", "base.git"] {
            let _ = fs::remove_dir_all(root.join(name));
        }
        if !existed {
            let _ = fs::remove_dir(root);
        }
    }
    result
}

pub fn review(root: &Path) -> io::Result<Review> {
    let files = walk(&root.join("work"))?.into_iter().collect::<Vec<_>>();
    run_git(root, &["add", "--update"])?;
    for chunk in files.chunks(128) {
        checked(
            &trusted_git()
                .arg("--literal-pathspecs")
                .arg("--git-dir")
                .arg(root.join("base.git"))
                .arg("--work-tree")
                .arg(root.join("work"))
                .args(["add", "--force", "--"])
                .args(chunk)
                .output()?,
        )?;
    }
    let changed = paths(&run_git(
        root,
        &[
            "diff",
            "--cached",
            "--name-only",
            "-z",
            "--no-renames",
            "HEAD",
        ],
    )?)?;
    let mut changes = Vec::new();
    for path in changed {
        if excluded(&path) {
            return Err(io::Error::other(format!(
                "{} is excluded from project changes.",
                path.display()
            )));
        }
        changes.push(Change {
            before: read_file(&root.join("before"), &path)?,
            after: read_file(&root.join("work"), &path)?,
            path,
        });
    }
    let patch = String::from_utf8(run_git(
        root,
        &[
            "diff",
            "--cached",
            "--binary",
            "--no-ext-diff",
            "--no-renames",
            "HEAD",
        ],
    )?)
    .map_err(io::Error::other)?;
    let summary = String::from_utf8(run_git(
        root,
        &["diff", "--cached", "--stat", "--no-renames", "HEAD"],
    )?)
    .map_err(io::Error::other)?;
    fs::write(root.join("review.patch"), &patch)?;
    Ok(Review {
        patch,
        summary,
        changes,
        fingerprint: fingerprint(&source_state(&root.join("work"))?)?,
    })
}

pub(crate) fn excluded(path: &Path) -> bool {
    path.components().any(|part| {
        let name = part.as_os_str().to_string_lossy();
        matches!(
            name.as_ref(),
            ".git"
                | ".codex"
                | "node_modules"
                | "target"
                | ".venv"
                | "venv"
                | "__pycache__"
                | ".DS_Store"
                | ".npmrc"
                | ".netrc"
                | ".pypirc"
        ) || name.ends_with(".pyc")
            || (name.starts_with(".env")
                && !matches!(name.as_ref(), ".env.example" | ".env.sample"))
    })
}

pub fn replace_source(root: &Path, snapshot: &Snapshot) -> io::Result<()> {
    let work = root.join("work");
    let staging = root.join("work-next");
    let previous = root.join("work-previous");
    if previous.exists() {
        if !work.exists() {
            fs::rename(&previous, &work)?;
        } else {
            fs::remove_dir_all(&previous)?;
        }
    }
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir(&staging)?;
    for (path, bytes, mode) in snapshot {
        write_file(
            &staging,
            path,
            Some(&File {
                bytes: bytes.clone(),
                mode: *mode,
            }),
        )?;
    }
    fs::rename(&work, &previous)?;
    if let Err(error) = fs::rename(&staging, &work) {
        fs::rename(&previous, &work)?;
        return Err(error);
    }
    fs::remove_dir_all(previous)
}

fn paths(bytes: &[u8]) -> io::Result<BTreeSet<PathBuf>> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            let path = PathBuf::from(std::str::from_utf8(path).map_err(io::Error::other)?);
            if path
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            {
                return Err(io::Error::other(
                    "Workspace paths must stay inside the project.",
                ));
            }
            Ok(path)
        })
        .collect()
}

fn walk(root: &Path) -> io::Result<BTreeSet<PathBuf>> {
    fn visit(root: &Path, dir: &Path, files: &mut BTreeSet<PathBuf>) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap();
            if excluded(relative) {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_symlink() || (!kind.is_file() && !kind.is_dir()) {
                return Err(io::Error::other(format!(
                    "{} needs a regular file or directory; symlinks are unsupported in this batch.",
                    relative.display()
                )));
            }
            if kind.is_dir() {
                visit(root, &path, files)?;
            } else {
                files.insert(relative.to_owned());
            }
        }
        Ok(())
    }
    let mut files = BTreeSet::new();
    visit(root, root, &mut files)?;
    Ok(files)
}

fn read_file(root: &Path, relative: &Path) -> io::Result<Option<File>> {
    safe_path(root, relative)?;
    let path = root.join(relative);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() {
        return Err(io::Error::other(format!(
            "{} is not a regular file.",
            path.display()
        )));
    }
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o777
    };
    #[cfg(not(unix))]
    let mode = 0o644;
    Ok(Some(File {
        bytes: fs::read(path)?,
        mode,
    }))
}

fn safe_path(root: &Path, relative: &Path) -> io::Result<()> {
    if fs::symlink_metadata(root)?.file_type().is_symlink() {
        return Err(io::Error::other("The project folder cannot be a symlink."));
    }
    let mut path = root.to_owned();
    for part in relative.components() {
        if !matches!(part, Component::Normal(_)) {
            return Err(io::Error::other("Invalid project path."));
        }
        path.push(part.as_os_str());
        if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(io::Error::other(format!(
                "{} is a symlink; changes were not applied.",
                path.display()
            )));
        }
    }
    Ok(())
}

fn write_file(root: &Path, path: &Path, file: Option<&File>) -> io::Result<()> {
    safe_path(root, path)?;
    let target = root.join(path);
    if let Some(file) = file {
        let parent = target.parent().unwrap();
        fs::create_dir_all(parent)?;
        let temp = parent.join(format!(".sprowt-apply-{}", std::process::id()));
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        let result = (|| {
            output.write_all(&file.bytes)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                output.set_permissions(fs::Permissions::from_mode(file.mode))?;
            }
            output.sync_all()?;
            fs::rename(&temp, target)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    } else {
        fs::remove_file(target)
    }
}

pub(crate) fn trusted_git() -> Command {
    let mut command = Command::new("git");
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Sprowt")
        .env("GIT_AUTHOR_EMAIL", "sprowt@localhost")
        .env("GIT_COMMITTER_NAME", "Sprowt")
        .env("GIT_COMMITTER_EMAIL", "sprowt@localhost")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgSign=false",
            "-c",
            "core.autocrlf=false",
        ])
        .stdin(Stdio::null());
    command
}

fn run_git(root: &Path, args: &[&str]) -> io::Result<Vec<u8>> {
    let output = trusted_git()
        .arg("--git-dir")
        .arg(root.join("base.git"))
        .arg("--work-tree")
        .arg(root.join("work"))
        .args(args)
        .output()?;
    checked(&output)?;
    Ok(output.stdout)
}

fn checked(output: &Output) -> io::Result<()> {
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::test_support::TestData;

    fn project(data: &TestData) -> PathBuf {
        let project = data.0.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("a.txt"), "original").unwrap();
        project
    }

    #[test]
    fn replacing_exported_source_removes_deleted_files_and_keeps_baseline() {
        let data = TestData::new();
        let project = project(&data);
        let root = data.0.join("workspace");
        create(&project, &root).unwrap();
        let snapshot = vec![(PathBuf::from("new.txt"), b"exported".to_vec(), 0o755)];
        replace_source(&root, &snapshot).unwrap();
        assert_eq!(source_state(&root.join("work")).unwrap(), snapshot);
        assert_eq!(
            fs::read_to_string(root.join("before/a.txt")).unwrap(),
            "original"
        );
        assert_eq!(review(&root).unwrap().count(), 2);
    }

    #[test]
    fn snapshot_keeps_dirty_and_untracked_code_without_sharing_git_or_credentials() {
        let data = TestData::new();
        let project = project(&data);
        checked(
            &trusted_git()
                .arg("-C")
                .arg(&project)
                .args(["init"])
                .output()
                .unwrap(),
        )
        .unwrap();
        fs::write(project.join(".gitignore"), "ignored.txt\n").unwrap();
        checked(
            &trusted_git()
                .arg("-C")
                .arg(&project)
                .args(["add", "."])
                .output()
                .unwrap(),
        )
        .unwrap();
        checked(
            &trusted_git()
                .arg("-C")
                .arg(&project)
                .args(["commit", "-m", "initial"])
                .output()
                .unwrap(),
        )
        .unwrap();
        fs::write(project.join("a.txt"), "dirty").unwrap();
        fs::write(project.join("new.txt"), "untracked").unwrap();
        fs::write(project.join("ignored.txt"), "ignored").unwrap();
        fs::write(project.join(".env"), "secret").unwrap();
        fs::write(project.join(".env.example"), "template").unwrap();
        let first = data.0.join("first");
        let second = data.0.join("second");
        create(&project, &first).unwrap();
        create(&project, &second).unwrap();
        assert_eq!(
            fs::read_to_string(first.join("work/a.txt")).unwrap(),
            "dirty"
        );
        assert!(first.join("work/new.txt").exists() && first.join("work/.env.example").exists());
        assert!(
            !first.join("work/.git").exists()
                && !first.join("work/.env").exists()
                && !first.join("work/ignored.txt").exists()
        );
        fs::write(first.join("work/a.txt"), "isolated").unwrap();
        assert_eq!(
            fs::read_to_string(second.join("work/a.txt")).unwrap(),
            "dirty"
        );
        assert_eq!(fs::read_to_string(project.join("a.txt")).unwrap(), "dirty");
        fs::write(first.join("work/.gitignore"), "created.txt\n").unwrap();
        fs::write(first.join("work/created.txt"), "requested source").unwrap();
        fs::write(first.join("work/cache.pyc"), "cache").unwrap();
        let review = review(&first).unwrap();
        assert!(review.patch.contains("created.txt") && !review.patch.contains("cache.pyc"));
    }

    #[test]
    fn review_applies_exact_additions_edits_deletions_and_preserves_modes() {
        use std::os::unix::fs::PermissionsExt;
        let data = TestData::new();
        let project = project(&data);
        fs::set_permissions(project.join("a.txt"), fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(project.join("delete.txt"), "delete").unwrap();
        let root = data.0.join("workspace");
        create(&project, &root).unwrap();
        fs::write(root.join("work/a.txt"), "edited").unwrap();
        fs::remove_file(root.join("work/delete.txt")).unwrap();
        fs::write(root.join("work/binary"), [0, 255, 1]).unwrap();
        fs::write(root.join("work/run"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(root.join("work/run"), fs::Permissions::from_mode(0o755)).unwrap();
        let review = review(&root).unwrap();
        assert_eq!(review.count(), 4);
        assert!(review.patch.contains("GIT binary patch"));
        review.apply(&project, &root).unwrap();
        assert_eq!(fs::read_to_string(project.join("a.txt")).unwrap(), "edited");
        assert!(!project.join("delete.txt").exists());
        assert_eq!(fs::read(project.join("binary")).unwrap(), [0, 255, 1]);
        assert_eq!(
            fs::metadata(project.join("a.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(project.join("run"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }

    #[test]
    fn conflicts_and_changed_review_abort_before_applying_any_file() {
        let data = TestData::new();
        let project = project(&data);
        fs::write(project.join("z.txt"), "before").unwrap();
        let root = data.0.join("workspace");
        create(&project, &root).unwrap();
        fs::write(root.join("work/a.txt"), "worker").unwrap();
        fs::write(root.join("work/z.txt"), "worker").unwrap();
        let review = review(&root).unwrap();
        fs::write(project.join("z.txt"), "user").unwrap();
        assert!(review.apply(&project, &root).is_err());
        assert_eq!(
            fs::read_to_string(project.join("a.txt")).unwrap(),
            "original"
        );
        fs::write(project.join("z.txt"), "before").unwrap();
        fs::write(root.join("work/z.txt"), "changed after review").unwrap();
        assert!(review.apply(&project, &root).is_err());
        assert_eq!(
            fs::read_to_string(project.join("a.txt")).unwrap(),
            "original"
        );
    }

    #[test]
    fn symlinks_cannot_redirect_snapshot_review_or_apply() {
        use std::os::unix::fs::symlink;
        let data = TestData::new();
        let project = project(&data);
        fs::write(data.0.join("outside"), "safe").unwrap();
        symlink(data.0.join("outside"), project.join("link")).unwrap();
        let root = data.0.join("workspace");
        assert!(create(&project, &root).is_err() && !root.exists());
        fs::remove_file(project.join("link")).unwrap();
        create(&project, &root).unwrap();
        fs::write(root.join("work/a.txt"), "worker").unwrap();
        let review = review(&root).unwrap();
        fs::remove_file(project.join("a.txt")).unwrap();
        symlink(data.0.join("outside"), project.join("a.txt")).unwrap();
        assert!(review.apply(&project, &root).is_err());
        symlink(data.0.join("outside"), root.join("work/link")).unwrap();
        assert!(super::review(&root).is_err());
        assert_eq!(fs::read_to_string(data.0.join("outside")).unwrap(), "safe");
    }
}
