use super::*;

const ASSETS: [(&str, &str); 3] = [
    ("apt.conf", include_str!("../../sandbox/apt.conf")),
    (
        "debian.sources",
        include_str!("../../sandbox/debian.sources"),
    ),
    ("offline.c", include_str!("../../sandbox/offline.c")),
];

pub(super) fn write_assets(directory: &Path) -> io::Result<()> {
    for (name, contents) in ASSETS {
        fs::write(directory.join(name), contents)?;
    }
    Ok(())
}

pub(super) fn valid_image(
    configuration: &Value,
    saved: Option<&Value>,
    connected_digest: &str,
) -> bool {
    let reference = configuration["image"]["reference"].as_str();
    let digest = configuration["image"]["descriptor"]["digest"].as_str();
    match reference {
        Some(IMAGE) => digest.is_some(),
        Some(LEGACY_IMAGE) => digest.is_some_and(|digest| {
            !digest.is_empty()
                && (saved.is_some_and(|saved| saved["digest"] == digest)
                    || connected_digest == digest)
        }),
        _ => false,
    }
}

fn sha256(path: &Path) -> io::Result<String> {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("/usr/bin/shasum");
        command.args(["-a", "256"]);
        command
    } else {
        Command::new("/usr/bin/sha256sum")
    };
    let output = command.arg(path).output()?;
    checked(&output)?;
    let digest = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_owned();
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::other("Helper checksum is invalid."));
    }
    Ok(digest)
}

impl Sandbox {
    pub(super) fn prepare_base(&mut self, cancelled: &AtomicBool) -> io::Result<()> {
        let setup = self.root.join("setup");
        fs::create_dir_all(&setup)?;
        write_assets(&setup)?;
        let expected = ASSETS
            .iter()
            .map(|(name, _)| Ok(format!("{}  {name}", sha256(&setup.join(name))?)))
            .collect::<io::Result<Vec<_>>>()?
            .join("\n");
        let script = "test -d /opt/sprowt-apt && cd /opt/sprowt-apt && test -x offline && test -r .compiled.sha256 && printf '%s\\n' \"$1\" | sha256sum --status -c - && sha256sum --status -c .compiled.sha256";
        if self.guest_status(&["/bin/sh", "-c", script, "base", &expected])? {
            return Ok(());
        }
        self.guest(&["/bin/mkdir", "-p", "/opt/sprowt-apt/conf.d"], cancelled)?;
        for (name, _) in ASSETS {
            self.copy_in(
                &setup.join(name),
                &format!("/opt/sprowt-apt/{name}"),
                cancelled,
            )?;
        }
        self.guest(
            &[
                "/usr/bin/cc",
                "-O2",
                "-Wall",
                "-Wextra",
                "-Werror",
                "/opt/sprowt-apt/offline.c",
                "-o",
                "/opt/sprowt-apt/offline",
            ],
            cancelled,
        )?;
        self.guest(
            &[
                "/bin/sh",
                "-c",
                "cd /opt/sprowt-apt && sha256sum offline > .compiled.sha256",
            ],
            cancelled,
        )
    }

    pub(super) fn ensure_python(&mut self, cancelled: &AtomicBool) -> io::Result<()> {
        if !self.guest_status(&["/usr/bin/test", "-x", "/usr/bin/python3"])? {
            self.install_packages(
                &Request {
                    packages: vec!["python3".into()],
                    reason: "Credential-free sandbox preparation".into(),
                },
                cancelled,
            )?;
        }
        Ok(())
    }

    pub(super) fn install_muse(
        &mut self,
        helper: &Path,
        binary: &Path,
        cancelled: &AtomicBool,
    ) -> io::Result<()> {
        self.ensure_python(cancelled)?;
        self.guest(
            &["/bin/mkdir", "-p", "/opt/sprowt-muse", "/opt/sprowt-checks"],
            cancelled,
        )?;
        let mut identity = serde_json::Map::new();
        identity.insert("version".into(), json!(crate::muse::VERSION));
        for (path, name) in [
            (binary.to_owned(), "muse"),
            (helper.join("bridge.py"), "bridge.py"),
            (helper.join("muse_transport.py"), "muse_transport.py"),
        ] {
            let expected = sha256(&path)?;
            let destination = format!("/opt/sprowt-muse/{name}");
            let valid = self.guest_status(&[
                "/bin/sh",
                "-c",
                "test -f \"$2\" && printf '%s  %s\\n' \"$1\" \"$2\" | sha256sum --status -c -",
                "muse",
                &expected,
                &destination,
            ])?;
            if !valid {
                self.copy_in(&path, &destination, cancelled)?;
            }
            identity.insert(name.into(), json!(expected));
        }
        self.guest(&["/bin/chmod", "755", "/opt/sprowt-muse/muse"], cancelled)?;
        self.guest(
            &[
                "/bin/sh",
                "-c",
                "printf '%s\\n' \"$1\" > /opt/sprowt-muse/identity.json",
                "muse",
                &Value::Object(identity).to_string(),
            ],
            cancelled,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "migrates a disposable retained v1 VM when its local legacy image is available"]
    fn retained_v1_vm_migrates_stable_helpers_and_keeps_source_and_runtime() {
        let legacy = container()
            .args(["image", "inspect", LEGACY_IMAGE])
            .output()
            .unwrap();
        assert!(
            legacy.status.success(),
            "Legacy image is required for this migration gate."
        );
        let data = crate::store::test_support::TestData::new();
        let project = data.0.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("source.txt"), "retained source").unwrap();
        let root = data.0.join(format!("legacy-base-{}", std::process::id()));
        workspace::create(&project, &root).unwrap();
        fs::create_dir_all(root.join("host-codex")).unwrap();
        let flag = AtomicBool::new(false);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> io::Result<()> {
                let mut old = Sandbox {
                    root: root.clone(),
                    name: name(&root)?,
                    home: root.join("host-codex"),
                    domains: network(&root)?,
                    child: None,
                    rpc: None,
                    started: false,
                    tasks: Default::default(),
                    identity: Identity {
                        digest: String::new(),
                        architecture: String::new(),
                    },
                };
                old.control(
                    &[
                        "create",
                        "--name",
                        &old.name,
                        "--cpus",
                        "2",
                        "--memory",
                        "2G",
                        "--label",
                        &format!("{OWNER}={}", old.name),
                        LEGACY_IMAGE,
                    ],
                    &flag,
                )?;
                let inspect = old
                    .inspect()?
                    .ok_or_else(|| io::Error::other("Legacy fixture VM is missing."))?;
                let digest = inspect["configuration"]["image"]["descriptor"]["digest"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("Legacy fixture digest is missing."))?
                    .to_owned();
                fs::write(
                    root.join("vm.json"),
                    serde_json::to_vec(&json!({"ready":true,"digest":digest}))?,
                )?;
                old.identity.digest = digest.clone();
                old.control(&["start", &old.name], &flag)?;
                old.started = true;
                old.import(&flag)?;
                old.guest(&["/bin/sh", "-c", "set -eu; test ! -x /usr/bin/python3; test ! -e /opt/sprowt-apt/offline; printf retained-runtime > /home/sprowt/runtime-sentinel"], &flag)?;
                drop(old);

                let vm = Sandbox::prepare(&root, &flag, |_| {})?;
                assert_eq!(vm.identity().digest, digest);
                vm.guest(&["/bin/sh", "-c", "set -eu; test \"$(cat /workspace/source.txt)\" = 'retained source'; test \"$(cat /home/sprowt/runtime-sentinel)\" = retained-runtime; test -x /usr/bin/python3; cd /opt/sprowt-apt; sha256sum --status -c .compiled.sha256"], &flag)?;
                drop(vm);
                let vm = Sandbox::prepare(&root, &flag, |_| {})?;
                assert_eq!(vm.identity().digest, digest);
                vm.guest(&["/bin/sh", "-c", "set -eu; test \"$(cat /workspace/source.txt)\" = 'retained source'; test \"$(cat /home/sprowt/runtime-sentinel)\" = retained-runtime; test -x /usr/bin/python3"], &flag)
            },
        ));
        delete(&root).unwrap();
        result.unwrap().unwrap();
    }

    #[test]
    #[ignore = "checks copy reuse and corruption recovery in a disposable VM without model inference"]
    fn muse_helpers_reuse_verified_content_and_recopy_corruption() {
        let data = crate::store::test_support::TestData::new();
        let project = data.0.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("source.txt"), "original").unwrap();
        let root = data.0.join(format!("muse-copy-{}", std::process::id()));
        workspace::create(&project, &root).unwrap();
        let helpers = data.0.join("helpers");
        fs::create_dir_all(&helpers).unwrap();
        fs::write(helpers.join("bridge.py"), "print('bridge')\n").unwrap();
        fs::write(helpers.join("muse_transport.py"), "print('transport')\n").unwrap();
        let binary = helpers.join("muse");
        fs::write(&binary, "#!/bin/sh\nprintf muse\n").unwrap();
        let flag = AtomicBool::new(false);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> io::Result<()> {
                let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
                vm.prepare_muse(&helpers, &binary, &flag)?;
                vm.guest(&["/bin/sh", "-c", "set -eu; touch -t 200001010000 /opt/sprowt-muse/muse /opt/sprowt-muse/bridge.py; printf runtime > /home/sprowt/retained"], &flag)?;
                vm.prepare_muse(&helpers, &binary, &flag)?;
                vm.guest(&["/bin/sh", "-c", "set -eu; test \"$(stat -c %Y /opt/sprowt-muse/muse)\" = 946684800; test \"$(stat -c %Y /opt/sprowt-muse/bridge.py)\" = 946684800; printf broken > /opt/sprowt-muse/bridge.py"], &flag)?;
                vm.prepare_muse(&helpers, &binary, &flag)?;
                assert_eq!(
                    vm.read_guest("/opt/sprowt-muse/bridge.py", &flag)?,
                    b"print('bridge')\n"
                );
                vm.guest(&["/bin/sh", "-c", "set -eu; test \"$(stat -c %Y /opt/sprowt-muse/muse)\" = 946684800; printf broken > /opt/sprowt-apt/offline"], &flag)?;
                drop(vm);
                let vm = Sandbox::prepare(&root, &flag, |_| {})?;
                vm.guest(&["/bin/sh", "-c", "cd /opt/sprowt-apt && sha256sum --status -c .compiled.sha256 && test \"$(cat /home/sprowt/retained)\" = runtime && test -x /usr/bin/python3"], &flag)
            },
        ));
        delete(&root).unwrap();
        result.unwrap().unwrap();
    }

    #[test]
    fn old_vm_requires_its_saved_digest_and_unknown_images_are_rejected() {
        let config =
            json!({"image":{"reference":LEGACY_IMAGE,"descriptor":{"digest":"sha256:old"}}});
        assert!(valid_image(
            &config,
            Some(&json!({"digest":"sha256:old"})),
            ""
        ));
        assert!(!valid_image(
            &config,
            Some(&json!({"digest":"sha256:changed"})),
            ""
        ));
        assert!(!valid_image(&config, None, ""));
        assert!(valid_image(&config, None, "sha256:old"));
        assert!(!valid_image(
            &json!({"image":{"reference":"sprowt-sandbox:untrusted","descriptor":{"digest":"sha256:old"}}}),
            Some(&json!({"digest":"sha256:old"})),
            ""
        ));
    }

    #[test]
    fn content_checksum_detects_corrupted_helper_with_same_name() {
        let data = crate::store::test_support::TestData::new();
        fs::create_dir_all(&data.0).unwrap();
        let path = data.0.join("helper.py");
        fs::write(&path, "print('ready')\n").unwrap();
        assert_eq!(
            sha256(&path).unwrap(),
            "e13cdff7de4a58aa927d0812d424a163af37d0619bebb12c0e68f042cd02a511"
        );
        fs::write(&path, "broken").unwrap();
        assert_ne!(
            sha256(&path).unwrap(),
            "e13cdff7de4a58aa927d0812d424a163af37d0619bebb12c0e68f042cd02a511"
        );
    }
}
