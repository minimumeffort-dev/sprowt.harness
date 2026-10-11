use super::*;
use crate::store::test_support::TestData;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
};

struct Fixture {
    data: TestData,
    base: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let data = TestData::new();
        fs::create_dir_all(&data.0).unwrap();
        let base = data.0.canonicalize().unwrap();
        Self { data, base }
    }
    fn file(&self, path: &str, bytes: &[u8], mode: u32) {
        let path = self.base.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
    fn assets(&self) {
        self.file(
            "one/.local/share/mise/installs/node/22/bin/node",
            b"runtime",
            0o755,
        );
        self.file(
            "one/.npm/_cacache/content-v2/sha512/00/blob",
            b"package",
            0o644,
        );
        self.file("one/.cache/pip/wheels/a/demo.whl", b"wheel", 0o644);
        self.file(
            "one/.cache/pip/wheels/a/origin.json",
            b"secret metadata",
            0o600,
        );
        self.file(
            "one/.local/share/mise/installs/node/22/bin/.npmrc",
            b"secret",
            0o600,
        );
        self.file("one/.npmrc", b"secret", 0o600);
        self.file("one/auth.json", b"secret", 0o600);
        self.file("one/.codex/history", b"secret", 0o600);
        self.file("task-one/node_modules/pkg/bin.js", b"dependency", 0o755);
        fs::create_dir_all(self.base.join("task-one/node_modules/.bin")).unwrap();
        symlink(
            "../pkg/bin.js",
            self.base.join("task-one/node_modules/.bin/pkg"),
        )
        .unwrap();
    }
    fn run(&self, action: &str, key: &str, home: &str, workspace: &str, dependencies: bool) -> Use {
        for path in [home, workspace] {
            fs::create_dir_all(self.base.join(path)).unwrap();
        }
        let result = self.base.join("result.json");
        let args = json!({"action":action,"key":key,"dependencies":dependencies,
            "home":self.base.join(home),"workspace":self.base.join(workspace),
            "cache":self.base.join("cache"),"result":result});
        let output = Command::new("python3")
            .args(["-c", POLICY, &args.to_string()])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&fs::read(result).unwrap()).unwrap()
    }
    fn thaw(path: &Path) {
        if path.is_symlink() {
            return;
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        if path.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                Self::thaw(&entry.unwrap().path());
            }
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        Self::thaw(&self.data.0);
    }
}

const KEY: &str = "1111111111111111111111111111111111111111";

#[test]
fn prepared_assets_are_verified_private_copies_without_credentials() {
    let f = Fixture::new();
    f.assets();
    assert_eq!(
        f.run("publish", KEY, "one", "task-one", true),
        Use::Published
    );
    assert_eq!(
        f.run("restore", KEY, "two", "task-two", true),
        Use::Restored {
            bytes: 29,
            dependencies: true
        }
    );
    let runtime = "two/.local/share/mise/installs/node/22/bin/node";
    assert_eq!(fs::read(f.base.join(runtime)).unwrap(), b"runtime");
    assert_eq!(
        fs::metadata(f.base.join(runtime))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert_eq!(
        fs::read_link(f.base.join("task-two/node_modules/.bin/pkg")).unwrap(),
        Path::new("../pkg/bin.js")
    );
    assert_eq!(
        fs::read(f.base.join("task-two/node_modules/.bin/pkg")).unwrap(),
        b"dependency"
    );
    for path in [
        "two/auth.json",
        "two/.npmrc",
        "two/.codex",
        "two/.cache/pip/wheels/a/origin.json",
        "two/.local/share/mise/installs/node/22/bin/.npmrc",
    ] {
        assert!(!f.base.join(path).exists(), "{path}");
    }
    f.file(runtime, b"private edit", 0o755);
    f.file(
        "task-two/node_modules/pkg/bin.js",
        b"private dependency",
        0o755,
    );
    assert_eq!(
        f.run("restore", KEY, "three", "task-three", true),
        Use::Restored {
            bytes: 29,
            dependencies: true
        }
    );
    assert_eq!(
        fs::read(
            f.base
                .join("three/.local/share/mise/installs/node/22/bin/node")
        )
        .unwrap(),
        b"runtime"
    );
    assert_eq!(
        fs::read(f.base.join("task-three/node_modules/pkg/bin.js")).unwrap(),
        b"dependency"
    );
}

#[test]
fn restore_preserves_existing_roots_and_requires_explicit_dependencies() {
    let f = Fixture::new();
    f.assets();
    f.run("publish", KEY, "one", "task-one", true);
    f.file("two/.local/share/mise/installs/keep", b"current", 0o600);
    f.file("task-two/node_modules/keep", b"current", 0o600);
    assert_eq!(
        f.run("restore", KEY, "two", "task-two", true),
        Use::Restored {
            bytes: 12,
            dependencies: false
        }
    );
    assert_eq!(
        fs::read(f.base.join("two/.local/share/mise/installs/keep")).unwrap(),
        b"current"
    );
    assert_eq!(
        fs::read(f.base.join("task-two/node_modules/keep")).unwrap(),
        b"current"
    );
    assert_eq!(
        f.run("restore", KEY, "three", "task-three", false),
        Use::Restored {
            bytes: 19,
            dependencies: false
        }
    );
    assert!(!f.base.join("task-three/node_modules").exists());
}

#[test]
fn corruption_and_changed_modes_reject_before_restore_and_can_be_rebuilt() {
    let f = Fixture::new();
    f.assets();
    f.run("publish", KEY, "one", "task-one", false);
    let cached = f.base.join(format!("cache/{KEY}/0/node/22/bin/node"));
    fs::set_permissions(&cached, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(&cached, b"corrupt").unwrap();
    fs::set_permissions(&cached, fs::Permissions::from_mode(0o555)).unwrap();
    assert!(matches!(
        f.run("restore", KEY, "two", "task-two", false),
        Use::Rejected { .. }
    ));
    assert!(!f.base.join("two/.local").exists());
    assert_eq!(
        f.run("publish", KEY, "one", "task-one", false),
        Use::Published
    );
    fs::set_permissions(&cached, fs::Permissions::from_mode(0o444)).unwrap();
    assert!(matches!(
        f.run("restore", KEY, "three", "task-three", false),
        Use::Rejected { .. }
    ));
    assert_eq!(
        f.run("publish", KEY, "one", "task-one", false),
        Use::Published
    );
    assert_eq!(
        f.run("restore", KEY, "four", "task-four", false),
        Use::Restored {
            bytes: 19,
            dependencies: false
        }
    );
}

#[test]
fn interrupted_restore_stages_are_cleaned_without_touching_worker_source() {
    let f = Fixture::new();
    f.assets();
    f.run("publish", KEY, "one", "task-one", false);
    f.file("cache/.restore-interrupted/.lease", b"", 0o600);
    f.file(
        "cache/.restore-interrupted/assets/partial",
        b"interrupted",
        0o600,
    );
    f.file(
        "task-two/.sprowt-seed-user-note/keep",
        b"user source",
        0o600,
    );
    f.file("cache/.restore-active/.lease", b"", 0o600);
    f.file(
        "cache/.restore-active/assets/partial",
        b"copy in progress",
        0o600,
    );
    let lease = fs::File::open(f.base.join("cache/.restore-active/.lease")).unwrap();
    use std::os::fd::AsRawFd;
    assert_eq!(
        unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    assert_eq!(
        f.run("restore", KEY, "two", "task-two", false),
        Use::Restored {
            bytes: 19,
            dependencies: false
        }
    );
    assert!(!f.base.join("cache/.restore-interrupted").exists());
    assert!(f.base.join("cache/.restore-active/assets/partial").exists());
    assert_eq!(
        fs::read(f.base.join("task-two/.sprowt-seed-user-note/keep")).unwrap(),
        b"user source"
    );
    drop(lease);
    assert_eq!(
        f.run("restore", KEY, "three", "task-three", false),
        Use::Restored {
            bytes: 19,
            dependencies: false
        }
    );
    assert!(!f.base.join("cache/.restore-active").exists());
}

#[test]
fn dependency_snapshots_cannot_silently_omit_private_configuration() {
    let f = Fixture::new();
    f.assets();
    f.file(
        "task-one/node_modules/pkg/.env",
        b"private dependency config",
        0o600,
    );
    assert!(
        matches!(f.run("publish", KEY, "one", "task-one", true), Use::Rejected { reason } if reason.contains("private configuration"))
    );
    assert!(!f.base.join(format!("cache/{KEY}")).exists());
}

#[test]
fn relative_links_cannot_escape_through_other_links() {
    let f = Fixture::new();
    f.assets();
    let root = f.base.join("task-one/node_modules");
    fs::create_dir_all(root.join("d")).unwrap();
    fs::create_dir_all(root.join("x")).unwrap();
    symlink("../x", root.join("d/a")).unwrap();
    symlink("d/a/../../outside", root.join("b")).unwrap();
    let result = f.run("publish", KEY, "one", "task-one", true);
    assert!(matches!(result, Use::Rejected { reason } if reason.contains("leaves its asset root")));
    assert!(!f.base.join(format!("cache/{KEY}")).exists());
}

#[test]
fn symlinked_home_ancestors_cause_no_writes_outside_the_private_home() {
    for ancestor in [".local", ".npm"] {
        let f = Fixture::new();
        f.assets();
        f.run("publish", KEY, "one", "task-one", false);
        let outside = f.base.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(f.base.join("two")).unwrap();
        symlink(&outside, f.base.join("two").join(ancestor)).unwrap();
        assert!(matches!(
            f.run("restore", KEY, "two", "task-two", false),
            Use::Rejected { .. }
        ));
        assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
    }
}

#[test]
fn invalid_keys_absolute_links_and_oversized_assets_are_cache_misses() {
    let f = Fixture::new();
    f.assets();
    assert!(matches!(
        f.run("publish", "../outside", "one", "task-one", false),
        Use::Rejected { .. }
    ));
    let path = f
        .base
        .join("one/.local/share/mise/installs/node/22/bin/escape");
    symlink("/etc/passwd", &path).unwrap();
    assert!(matches!(
        f.run("publish", KEY, "one", "task-one", false),
        Use::Rejected { .. }
    ));
    fs::remove_file(path).unwrap();
    let large = fs::File::create(f.base.join("one/.local/share/mise/installs/large")).unwrap();
    large.set_len(512 * 1024 * 1024 + 1).unwrap();
    assert!(matches!(
        f.run("publish", KEY, "one", "task-one", false),
        Use::Rejected { .. }
    ));
    assert!(!f.base.join(format!("cache/{KEY}")).exists());
}

#[test]
fn cache_retention_keeps_at_most_three_complete_environments() {
    let f = Fixture::new();
    f.assets();
    for number in 1..=4 {
        let key = format!("{number:040x}");
        assert_eq!(
            f.run("publish", &key, "one", "task-one", false),
            Use::Published
        );
    }
    let entries = fs::read_dir(f.base.join("cache"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().len() == 40)
        .count();
    assert_eq!(entries, 3);
    assert_eq!(
        f.run("restore", &format!("{:040x}", 1), "two", "task-two", false),
        Use::Miss
    );
    assert_eq!(
        f.run(
            "restore",
            &format!("{:040x}", 4),
            "three",
            "task-three",
            false
        ),
        Use::Restored {
            bytes: 19,
            dependencies: false
        }
    );
}
