use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(crate) enum State {
    Preparing,
    EmptyReady,
    BoundReady,
}

pub(crate) fn saved_state(root: &Path) -> io::Result<State> {
    Ok(read_marker(root)?.as_ref().map_or(State::Preparing, state))
}

fn read_marker(root: &Path) -> io::Result<Option<Value>> {
    match fs::read(root.join("vm.json")) {
        Ok(bytes) => {
            let value: Value = serde_json::from_slice(&bytes)?;
            if let Some(state) = value.get("state") {
                serde_json::from_value::<State>(state.clone())?;
            }
            Ok(Some(value))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn state(marker: &Value) -> State {
    match marker["state"].as_str() {
        Some("BoundReady") => State::BoundReady,
        Some("EmptyReady") => State::EmptyReady,
        Some("Preparing") => State::Preparing,
        _ if marker["ready"] == true => State::BoundReady,
        _ => State::Preparing,
    }
}

impl Sandbox {
    fn save_state(&self) -> io::Result<()> {
        let pending = self.root.join("vm.json.pending");
        fs::write(
            &pending,
            serde_json::to_vec(&json!({"state":self.state,"digest":self.identity.digest}))?,
        )?;
        private(&pending, 0o600)?;
        fs::rename(pending, self.root.join("vm.json"))
    }

    pub fn has_bound_source(root: &Path) -> io::Result<bool> {
        Ok(saved_state(root)? == State::BoundReady)
    }

    pub(crate) fn require_bound(&self) -> io::Result<()> {
        if self.state != State::BoundReady {
            return Err(io::Error::other(
                "The empty VM has no accepted source to export or checkpoint.",
            ));
        }
        Ok(())
    }

    pub fn prepare(
        root: &Path,
        cancelled: &AtomicBool,
        progress: impl Fn(&str),
    ) -> io::Result<Self> {
        Self::prepare_with_timing(root, cancelled, progress, Timing::quiet())
    }

    pub(crate) fn prepare_with_timing(
        root: &Path,
        cancelled: &AtomicBool,
        progress: impl Fn(&str),
        timing: Timing<'_>,
    ) -> io::Result<Self> {
        let mut vm = Self::prepare_empty_with_timing(root, None, cancelled, &progress, timing)?;
        vm.bind(&[], None, cancelled, &progress, timing)?;
        Ok(vm)
    }

    pub(crate) fn prepare_empty_with_timing(
        root: &Path,
        memory: Option<u64>,
        cancelled: &AtomicBool,
        progress: &impl Fn(&str),
        timing: Timing<'_>,
    ) -> io::Result<Self> {
        if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            return Err(io::Error::other(
                "Linux VMs require Apple silicon and macOS 26+.",
            ));
        }
        fs::create_dir_all(root)?;
        private(root, 0o700)?;
        let saved = read_marker(root)?;
        let mut vm = Self {
            root: root.to_owned(),
            name: name(root)?,
            home: root.join("host-codex"),
            domains: network(root)?,
            child: None,
            rpc: None,
            started: false,
            tasks: Default::default(),
            state: saved.as_ref().map_or(State::Preparing, state),
            allocation: resources::Allocation::SMALL,
            environment: std::sync::Arc::new(super::environment_gate::Gate::default()),
            transfer_basis: None,
            environment_read: false,
            identity: Identity {
                digest: String::new(),
                architecture: String::new(),
            },
        };
        progress("preparing Linux VM");
        vm.login()?;
        let version = Command::new("codex").arg("--version").output()?;
        if !version.status.success()
            || String::from_utf8_lossy(&version.stdout).trim() != format!("codex-cli {VERSION}")
        {
            return Err(io::Error::other(format!(
                "This sandbox needs Codex CLI {VERSION}; host and guest versions must match."
            )));
        }
        let mut existing = vm.inspect()?;
        if vm.state == State::BoundReady && existing.is_none() {
            return Err(io::Error::other(
                "The saved VM is missing. Working files are retained; create a new mod to rebuild its environment.",
            ));
        }
        if let Some(inspect) = &existing {
            vm.validate(inspect, saved.as_ref())?;
            vm.allocation = resources::Allocation::from_configuration(&inspect["configuration"])?;
            if vm.state == State::BoundReady
                && memory.is_some_and(|memory| memory > vm.allocation.memory_gib)
            {
                return Err(io::Error::other(
                    "The saved VM has less memory than this recipe requires. Its source and runtime are retained; create a new codemod with the requested memory.",
                ));
            }
        }
        let recreate = vm.state == State::Preparing
            || vm.state == State::EmptyReady
                && memory.is_some_and(|memory| memory > vm.allocation.memory_gib);
        if recreate && existing.is_some() {
            vm.control(&["delete", "--force", &vm.name], cancelled)?;
            existing = None;
            vm.state = State::Preparing;
        }
        if existing.is_none() {
            vm.state = State::Preparing;
            vm.image(cancelled, progress, timing)?;
        }
        if existing
            .as_ref()
            .is_some_and(|inspect| inspect["status"]["state"] == "running")
        {
            vm.control(&["stop", &vm.name], cancelled)?;
        }
        progress("waiting for VM capacity");
        let (allocation, admission) = resources::admit(
            existing.as_ref().map(|_| vm.allocation),
            memory,
            cancelled,
            timing,
        )?;
        vm.allocation = allocation;
        if existing.is_none() {
            timing.measure(Phase::Boot, || {
                vm.control(
                    &[
                        "create",
                        "--name",
                        &vm.name,
                        "--cpus",
                        &allocation.cpus.to_string(),
                        "--memory",
                        &format!("{}G", allocation.memory_gib),
                        "--label",
                        &format!("{OWNER}={}", vm.name),
                        IMAGE,
                    ],
                    cancelled,
                )
            })?;
        }
        vm.started = true;
        timing.measure(Phase::Boot, || vm.control(&["start", &vm.name], cancelled))?;
        let inspect = vm
            .inspect()?
            .ok_or_else(|| io::Error::other("Prepared VM is missing."))?;
        vm.validate(
            &inspect,
            if existing.is_some() {
                saved.as_ref()
            } else {
                None
            },
        )?;
        if inspect["status"]["state"] != "running"
            || resources::Allocation::from_configuration(&inspect["configuration"])? != allocation
        {
            return Err(io::Error::other(
                "The running VM allocation could not be confirmed.",
            ));
        }
        drop(admission);
        vm.identity = Identity {
            digest: inspect["configuration"]["image"]["descriptor"]["digest"]
                .as_str()
                .ok_or_else(|| io::Error::other("VM image digest is missing."))?
                .into(),
            architecture: inspect["configuration"]["platform"]["architecture"]
                .as_str()
                .ok_or_else(|| io::Error::other("VM architecture is missing."))?
                .into(),
        };
        vm.save_state()?;
        vm.control(
            &["exec", &vm.name, "/bin/mkdir", "-p", "/opt/sprowt-transfer"],
            cancelled,
        )?;
        let digest = vm.identity.digest.clone();
        let timing = timing.with_environment(&digest);
        timing.measure(Phase::BaseSetup, || vm.prepare_base(cancelled))?;
        let (child, mut rpc) = Rpc::start(&mut vm.executor())?;
        vm.child = Some(child);
        super::session::initialize(&vm.name, &mut rpc)?;
        vm.rpc = Some(rpc);
        progress("checking Linux VM isolation");
        timing.measure(Phase::BaseSetup, || {
            vm.boundary(cancelled)?;
            vm.ensure_python(cancelled)
        })?;
        if vm.state == State::BoundReady {
            vm.reconcile_bound(cancelled, timing)?;
        } else {
            vm.state = State::EmptyReady;
            vm.save_state()?;
        }
        Ok(vm)
    }

    pub(crate) fn bind(
        &mut self,
        tasks: &[i64],
        memory: Option<u64>,
        cancelled: &AtomicBool,
        progress: &impl Fn(&str),
        timing: Timing<'_>,
    ) -> io::Result<()> {
        if self.state == State::Preparing
            || memory.is_some_and(|memory| memory > self.allocation.memory_gib)
        {
            if self.state == State::BoundReady {
                return Err(io::Error::other(
                    "The saved VM has less memory than this recipe requires. Its source and runtime are retained; create a new codemod with the requested memory.",
                ));
            }
            if let Some(mut child) = self.child.take() {
                rpc::terminate(&mut child);
            }
            self.rpc.take();
            self.control(&["stop", &self.name], cancelled)?;
            self.started = false;
            *self =
                Self::prepare_empty_with_timing(&self.root, memory, cancelled, progress, timing)?;
        }
        self.refresh_network()?;
        let digest = self.identity.digest.clone();
        let binding = self.state == State::EmptyReady;
        let result = timing.with_environment(&digest).measure(Phase::Source, || {
            if binding {
                self.state = State::Preparing;
                self.save_state()?;
                // No worker could dispatch before the first bound marker.
                for name in ["tasks.json", "tasks.bundle"] {
                    let path = self.root.join(name);
                    if path.exists() {
                        fs::remove_file(path)?;
                    }
                }
                let drafts = self.root.join("task-drafts");
                if drafts.exists() {
                    fs::remove_dir_all(drafts)?;
                }
                self.import(cancelled)?;
                self.state = State::BoundReady;
                self.reconcile_bound(cancelled, timing)?;
            }
            if !tasks.is_empty() {
                self.prepare_tasks(tasks, cancelled)?;
            }
            let error = self.root.join("checkpoint-error");
            if error.exists() {
                fs::remove_file(error)?;
            }
            self.save_state()?;
            Ok(())
        });
        if binding && result.is_err() {
            self.state = State::Preparing;
        }
        result
    }
    fn reconcile_bound(&mut self, cancelled: &AtomicBool, timing: Timing<'_>) -> io::Result<()> {
        timing.measure(Phase::Source, || {
            self.restore_tasks(cancelled)?;
            self.import_update(cancelled)?;
            let active = self.tasks.active;
            if !self.tasks.round.is_empty() {
                self.tasks.active = None;
                self.export(cancelled)?;
            }
            for id in self.tasks.round.clone() {
                self.checkpoint_task(id)?;
            }
            self.tasks.active = active;
            self.export(cancelled)?;
            Ok(())
        })
    }
}

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
            (helper.join("muse_release.py"), "muse_release.py"),
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
    fn legacy_ready_is_bound_and_malformed_states_are_rejected() {
        let data = crate::store::test_support::TestData::new();
        fs::create_dir_all(&data.0).unwrap();
        fs::write(
            data.0.join("vm.json"),
            br#"{"ready":true,"digest":"sha256:old"}"#,
        )
        .unwrap();
        assert!(Sandbox::has_bound_source(&data.0).unwrap());
        assert!(
            crate::codex::shared_vm::prewarm(&data.0, &AtomicBool::new(false), Timing::quiet())
                .unwrap()
                .is_none()
        );
        fs::write(
            data.0.join("vm.json"),
            br#"{"state":"EmptyReady","digest":"sha256:new"}"#,
        )
        .unwrap();
        assert!(!Sandbox::has_bound_source(&data.0).unwrap());
        fs::write(
            data.0.join("vm.json"),
            br#"{"state":"Preparing","ready":true}"#,
        )
        .unwrap();
        assert_eq!(saved_state(&data.0).unwrap(), State::Preparing);
        fs::write(
            data.0.join("vm.json"),
            br#"{"state":"unknown","ready":true}"#,
        )
        .unwrap();
        assert!(Sandbox::has_bound_source(&data.0).is_err());
    }

    #[test]
    #[ignore = "prepares and promotes a disposable empty VM; no model inference or project downloads"]
    fn early_vm_is_empty_then_binds_accepted_source_and_preserves_bound_allocation() {
        let data = crate::store::test_support::TestData::new();
        let project = data.0.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("source.txt"), "unaccepted").unwrap();
        let root = data.0.join(format!("early-vm-{}", std::process::id()));
        let flag = AtomicBool::new(false);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> io::Result<()> {
                let owner =
                    crate::codex::shared_vm::prewarm(&root, &flag, Timing::quiet())?.unwrap();
                assert_eq!(saved_state(&root)?, State::EmptyReady);
                assert!(!root.join("before").exists() && !root.join("work").exists());
                {
                    let mut vm = owner.lock().unwrap();
                    vm.guest(
                        &[
                            "/bin/sh",
                            "-c",
                            "test ! -e /workspace/source.txt && test ! -e /tasks/1",
                        ],
                        &flag,
                    )?;
                    assert!(
                        vm.export(&flag)
                            .unwrap_err()
                            .to_string()
                            .contains("empty VM")
                    );
                    assert!(vm.checkpoint_task(1).is_err());
                    assert_eq!(
                        vm.allocation,
                        resources::Allocation {
                            cpus: 4,
                            memory_gib: 4
                        }
                    );
                }
                assert!(
                    crate::codex::shared_vm::prepare(
                        &root,
                        &[1, 2],
                        None,
                        &flag,
                        |_| {},
                        Timing::quiet(),
                    )
                    .is_err()
                );
                assert_eq!(saved_state(&root)?, State::Preparing);
                fs::write(project.join("source.txt"), "accepted")?;
                workspace::create(&project, &root)?;
                fs::create_dir(root.join("tasks.next"))?;
                assert!(
                    crate::codex::shared_vm::prepare(
                        &root,
                        &[1, 2],
                        Some(8),
                        &flag,
                        |_| {},
                        Timing::quiet(),
                    )
                    .is_err()
                );
                assert_eq!(saved_state(&root)?, State::Preparing);
                assert!(root.join("tasks.json").exists());
                assert!(!root.join("tasks.bundle").exists());
                fs::remove_dir(root.join("tasks.next"))?;
                let bound = crate::codex::shared_vm::prepare(
                    &root,
                    &[1, 2],
                    Some(8),
                    &flag,
                    |_| {},
                    Timing::quiet(),
                )?;
                assert!(std::sync::Arc::ptr_eq(&owner, &bound));
                assert_eq!(saved_state(&root)?, State::BoundReady);
                {
                    let vm = bound.lock().unwrap();
                    assert_eq!(
                        vm.allocation,
                        resources::Allocation {
                            cpus: 4,
                            memory_gib: 8
                        }
                    );
                    vm.guest(&["/bin/sh", "-c", "test \"$(cat /workspace/source.txt)\" = accepted && test \"$(cat /tasks/1/source.txt)\" = accepted && test \"$(cat /tasks/2/source.txt)\" = accepted"], &flag)?;
                }
                assert!(crate::codex::shared_vm::prewarm(&root, &flag, Timing::quiet())?.is_none());
                let resumed = crate::codex::shared_vm::prepare(
                    &root,
                    &[1, 2],
                    None,
                    &flag,
                    |_| {},
                    Timing::quiet(),
                )?;
                assert!(std::sync::Arc::ptr_eq(&bound, &resumed));
                assert_eq!(resumed.lock().unwrap().allocation.memory_gib, 8);
                drop(resumed);
                drop(bound);
                drop(owner);
                let resumed = Sandbox::prepare(&root, &flag, |_| {})?;
                assert_eq!(resumed.allocation.memory_gib, 8);
                resumed.guest(
                    &[
                        "/bin/sh",
                        "-c",
                        "test \"$(cat /tasks/1/source.txt)\" = accepted",
                    ],
                    &flag,
                )?;
                Ok(())
            },
        ));
        delete(&root).unwrap();
        result.unwrap().unwrap();
    }

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
                    state: State::BoundReady,
                    allocation: resources::Allocation::SMALL,
                    environment: std::sync::Arc::new(super::environment_gate::Gate::default()),
                    transfer_basis: None,
                    environment_read: false,
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
        fs::write(helpers.join("muse_release.py"), "print('release')\n").unwrap();
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
