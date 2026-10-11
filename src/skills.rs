use std::{
    fs,
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::plan::Role;

include!(concat!(env!("OUT_DIR"), "/skills.rs"));

static PREPARE: Mutex<()> = Mutex::new(());
static NEXT: AtomicU64 = AtomicU64::new(0);
const POLICY: &str = include_str!("../skills/POLICY.md");

pub fn guest_root() -> String {
    format!("/opt/sprowt-skills/{VERSION}")
}

pub fn host_root() -> io::Result<PathBuf> {
    Ok(crate::router::directory()?.join("skills").join(VERSION))
}

pub fn instructions(role: Role, root: &Path) -> String {
    let role = match role {
        Role::Planner => include_str!("../skills/roles/planner.md"),
        Role::Executor => include_str!("../skills/roles/executor.md"),
        Role::Reviewer => include_str!("../skills/roles/reviewer.md"),
    };
    format!("{POLICY}\n{role}").replace("{{SKILLS_ROOT}}", &root.to_string_lossy())
}

pub fn publication_policy() -> String {
    format!(
        "{POLICY}\nRole: publication writer. This is a noncoding task. Use only supplied change evidence; do not load coding skills or use tools. Preserve the publication JSON schema."
    )
}

pub fn prepare_host() -> io::Result<PathBuf> {
    let root = host_root()?;
    materialize(&root)?;
    Ok(root)
}

fn directory(path: &Path, mode: u32) -> io::Result<()> {
    fs::create_dir_all(path)?;
    if !fs::symlink_metadata(path)?.file_type().is_dir() {
        return Err(io::Error::other(
            "Skill directory must not be a symbolic link.",
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

fn materialize(root: &Path) -> io::Result<()> {
    let _guard = PREPARE.lock().unwrap();
    let base = root
        .parent()
        .ok_or_else(|| io::Error::other("Invalid skill directory."))?;
    directory(base, 0o700)?;
    if root.try_exists()? {
        return validate(root);
    }
    let next = base.join(format!(
        ".{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&next)?;
    let result = (|| {
        for (name, bytes) in FILES {
            let path = next.join(name);
            fs::create_dir_all(path.parent().unwrap())?;
            let mut file = fs::File::create_new(&path)?;
            file.write_all(bytes)?;
            file.set_permissions(fs::Permissions::from_mode(0o444))?;
        }
        set_directories(&next, 0o555)?;
        match fs::rename(&next, root) {
            Ok(()) => Ok(()),
            Err(_) if root.try_exists()? => validate(root),
            Err(error) => Err(error),
        }
    })();
    if next.exists() {
        set_directories(&next, 0o700)?;
        fs::remove_dir_all(next)?;
    }
    result
}

fn set_directories(root: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(root, fs::Permissions::from_mode(mode))?;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            set_directories(&entry.path(), mode)?;
        }
    }
    Ok(())
}

fn validate(root: &Path) -> io::Result<()> {
    for (name, bytes) in FILES {
        let path = root.join(name);
        for ancestor in path.ancestors().take_while(|path| path.starts_with(root)) {
            let meta = fs::symlink_metadata(ancestor)?;
            if meta.file_type().is_symlink() || meta.permissions().mode() & 0o222 != 0 {
                return Err(io::Error::other("Pinned skill bundle is not read-only."));
            }
        }
        if fs::read(path)? != *bytes {
            return Err(io::Error::other(
                "Pinned skill bundle differs from this harness build.",
            ));
        }
    }
    Ok(())
}

pub fn archive(writer: impl Write) -> io::Result<()> {
    let mut archive = tar::Builder::new(writer);
    for (name, bytes) in FILES {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o444);
        header.set_cksum();
        archive.append_data(&mut header, name, *bytes)?;
    }
    archive.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::test_support::TestData;

    #[test]
    fn staged_bundle_preserves_bytes_and_rejects_replacement() {
        let data = TestData::new();
        let root = data.0.join("skills").join(VERSION);
        materialize(&root).unwrap();
        materialize(&root).unwrap();
        validate(&root).unwrap();
        let path = root.join("POLICY.md");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(&path, "project replacement").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        assert!(
            materialize(&root)
                .unwrap_err()
                .to_string()
                .contains("differs")
        );
        set_directories(&root, 0o700).unwrap();
    }

    #[test]
    fn archive_contains_exact_bundle_with_read_only_regular_files() {
        let mut bytes = Vec::new();
        archive(&mut bytes).unwrap();
        let mut count = 0;
        for entry in tar::Archive::new(bytes.as_slice()).entries().unwrap() {
            use std::io::Read;
            let mut entry = entry.unwrap();
            assert!(entry.header().entry_type().is_file());
            assert_eq!(entry.header().mode().unwrap(), 0o444);
            let path = entry.path().unwrap().into_owned();
            let expected = FILES
                .iter()
                .find(|(name, _)| path == Path::new(name))
                .unwrap()
                .1;
            let mut actual = Vec::new();
            entry.read_to_end(&mut actual).unwrap();
            assert_eq!(actual, expected);
            count += 1;
        }
        assert_eq!(count, FILES.len());
    }

    #[test]
    fn every_coding_role_receives_policy_with_a_resolved_root() {
        for role in [Role::Planner, Role::Executor, Role::Reviewer] {
            for root in [
                Path::new("/private/harness/skills/pinned"),
                Path::new("/opt/sprowt-skills/pinned"),
            ] {
                let text = instructions(role, root);
                assert!(text.starts_with(POLICY.split("{{SKILLS_ROOT}}").next().unwrap()));
                assert!(text.contains(&root.to_string_lossy().to_string()));
                assert!(!text.contains("{{SKILLS_ROOT}}"));
                assert!(text.len() < 18_000, "Load upstream references on demand");
                for reference in text.split('`').skip(1).step_by(2) {
                    if let Ok(relative) = Path::new(reference).strip_prefix(root)
                        && !relative.as_os_str().is_empty()
                    {
                        assert!(
                            FILES.iter().any(|(name, _)| Path::new(name) == relative),
                            "Missing mandatory skill: {reference}"
                        );
                    }
                }
            }
        }
    }
}
