use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use serde_json::{Value, json};

use crate::{
    execution::{Check, CheckResult},
    packages::Request,
    rpc::{self, Rpc},
    workspace::{self, Snapshot},
};

pub const VERSION: &str = "0.159.2";
const IMAGE: &str = "sprowt-sandbox:0.159.2-v1";
const OWNER: &str = "dev.minimumeffort.sprowt.workspace";
const GUEST_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
const EXPORT_LIMIT: usize = 512 * 1024 * 1024;
static BUILD: Mutex<()> = Mutex::new(());
static PROCESS: AtomicU64 = AtomicU64::new(0);

pub struct Sandbox {
    root: PathBuf,
    name: String,
    pub home: PathBuf,
    pub domains: BTreeMap<String, String>,
    child: Option<Child>,
    rpc: Option<Rpc>,
    started: bool,
    pub(crate) tasks: crate::task_worktree::TaskWorktrees,
}

impl Sandbox {
    pub fn refresh_network(&mut self) -> io::Result<()> {
        self.domains = network(&self.root)?;
        Ok(())
    }
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn prepare_muse(
        &mut self,
        helper: &Path,
        binary: &Path,
        cancelled: &AtomicBool,
    ) -> io::Result<()> {
        self.install_packages(
            &Request {
                packages: vec!["python3".into()],
                reason: "Muse's credential-free VM transport".into(),
            },
            cancelled,
        )?;
        self.guest(&["/bin/mkdir", "-p", "/opt/sprowt-muse"], cancelled)?;
        for (path, name) in [
            (binary.to_owned(), "muse"),
            (helper.join("bridge.py"), "bridge.py"),
            (helper.join("muse_transport.py"), "muse_transport.py"),
        ] {
            self.copy_in(&path, &format!("/opt/sprowt-muse/{name}"), cancelled)?;
        }
        self.guest(&["/bin/chmod", "755", "/opt/sprowt-muse/muse"], cancelled)
    }

    pub(crate) fn muse_configuration(&self, worker: i64) -> Value {
        let cwd = self.task_folder();
        let home = crate::task_worktree::worker_home(worker);
        let mut entries =
            vec![json!({"path":{"type":"special","value":{"kind":"minimal"}},"access":"read"})];
        entries.extend([("/proc", "read"), ("/opt/sprowt-muse", "read"), (cwd.as_str(), "write"), (home.as_str(), "write"), ("/tmp", "write")]
            .map(|(path,access)| json!({"path":{"type":"path","path":format!("file://{path}")},"access":access})));
        entries.push(
            json!({"path":{"type":"path","path":format!("file://{cwd}/.git")},"access":"read"}),
        );
        json!({"name":self.name,"executor": ["container", "exec", "--interactive", &self.name, "env", "-i", "HOME=/home/sprowt", "CODEX_HOME=/opt/codex-home", "PATH=/usr/local/bin:/usr/bin:/bin", "/usr/local/bin/codex", "exec-server", "--listen", "stdio"],
            "cwd":cwd,"env":{"HOME":home,"PATH":GUEST_PATH,"LANG":"C.UTF-8","NO_PROXY":"localhost,127.0.0.1,::1","MUSE_NO_AUTO_UPDATE":"1"},
            "sandbox":{"permissions":{"type":"managed","file_system":{"type":"restricted","entries":entries},"network":"enabled"},"cwd":format!("file://{cwd}"),"workspaceRoots":[format!("file://{cwd}")],"windowsSandboxLevel":"disabled","useLegacyLandlock":false},
            "proxy":{"proxy":{"enabled":true,"enableSocks5":false,"enableSocks5Udp":false,"allowUpstreamProxy":false,"dangerouslyAllowAllUnixSockets":true,"mode":"full","domains":self.domains,"unixSockets":{},"allowLocalBinding":true},"auditMetadata":{}}})
    }

    pub(crate) fn assign_muse_task(
        &mut self,
        id: i64,
        worker: i64,
        cancelled: &AtomicBool,
    ) -> io::Result<()> {
        self.assign_task(id, worker, cancelled)?;
        self.guest(
            &[
                "/bin/mkdir",
                "-p",
                &format!("{}/.config/muse", crate::task_worktree::worker_home(worker)),
            ],
            cancelled,
        )
    }

    pub fn prepare(
        root: &Path,
        cancelled: &AtomicBool,
        progress: impl Fn(&str),
    ) -> io::Result<Self> {
        if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            return Err(io::Error::other(
                "Linux VMs require Apple silicon and macOS 26+.",
            ));
        }
        let mut vm = Self {
            root: root.to_owned(),
            name: name(root)?,
            home: root.join("host-codex"),
            domains: network(root)?,
            child: None,
            rpc: None,
            started: false,
            tasks: Default::default(),
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
        let marker = root.join("vm.json");
        let saved = match fs::read(&marker) {
            Ok(bytes) => Some(serde_json::from_slice::<Value>(&bytes)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let existing = vm.inspect()?;
        if saved.as_ref().is_some_and(|value| value["ready"] == true) && existing.is_none() {
            return Err(io::Error::other(
                "The saved VM is missing. Working files are retained; create a new mod to rebuild its environment.",
            ));
        }
        if let Some(existing) = &existing {
            vm.validate(existing, saved.as_ref())?;
        }
        if saved.as_ref().is_none_or(|value| value["ready"] != true) {
            if existing.is_some() {
                vm.control(&["delete", "--force", &vm.name], cancelled)?;
            }
            vm.image(cancelled, &progress)?;
            vm.control(
                &[
                    "create",
                    "--name",
                    &vm.name,
                    "--cpus",
                    "2",
                    "--memory",
                    "2G",
                    "--label",
                    &format!("{OWNER}={}", vm.name),
                    IMAGE,
                ],
                cancelled,
            )?;
            let inspect = vm
                .inspect()?
                .ok_or_else(|| io::Error::other("VM creation could not be confirmed."))?;
            vm.validate(&inspect, None)?;
            fs::write(
                &marker,
                serde_json::to_vec(
                    &json!({"ready":false,"digest":inspect["configuration"]["image"]["descriptor"]["digest"]}),
                )?,
            )?;
            vm.control(&["start", &vm.name], cancelled)?;
            vm.started = true;
            vm.import(cancelled)?;
            fs::write(
                &marker,
                serde_json::to_vec(
                    &json!({"ready":true,"digest":inspect["configuration"]["image"]["descriptor"]["digest"]}),
                )?,
            )?;
        } else {
            if existing.as_ref().unwrap()["status"]["state"] == "running" {
                vm.control(&["stop", &vm.name], cancelled)?;
            }
            vm.control(&["start", &vm.name], cancelled)?;
        }
        vm.started = true;
        vm.control(
            &["exec", &vm.name, "/bin/mkdir", "-p", "/opt/sprowt-transfer"],
            cancelled,
        )?;
        vm.control(
            &[
                "exec",
                &vm.name,
                "/bin/mkdir",
                "-p",
                "/opt/sprowt-apt/conf.d",
            ],
            cancelled,
        )?;
        let setup = root.join("setup");
        fs::create_dir_all(&setup)?;
        for (name, contents) in [
            ("apt.conf", include_str!("../sandbox/apt.conf")),
            ("debian.sources", include_str!("../sandbox/debian.sources")),
            ("offline.c", include_str!("../sandbox/offline.c")),
        ] {
            let path = setup.join(name);
            fs::write(&path, contents)?;
            vm.control(
                &[
                    "copy",
                    path.to_str().unwrap(),
                    &format!("{}:/opt/sprowt-apt/{name}", vm.name),
                ],
                cancelled,
            )?;
        }
        vm.control(
            &[
                "exec",
                &vm.name,
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
        let (child, mut rpc) = Rpc::start(&mut vm.executor())?;
        vm.child = Some(child);
        let initialized = rpc.call("initialize", json!({"clientName":"sprowt_verification"}))?;
        let info = &initialized["environmentInfo"];
        let version = container()
            .args(["exec", &vm.name, "/usr/local/bin/codex", "--version"])
            .output()?;
        if !version.status.success()
            || String::from_utf8_lossy(&version.stdout).trim() != format!("codex-cli {VERSION}")
            || (info["executorVersion"] != VERSION && info["executorVersion"] != "0.0.0")
            || info["platformOs"] != "linux"
            || info["capabilities"]["networkProxyLaunch"] != true
        {
            return Err(io::Error::other(
                "The guest executor has an incompatible version or platform.",
            ));
        }
        rpc.write(json!({"method":"initialized"}))?;
        vm.rpc = Some(rpc);
        progress("checking Linux VM isolation");
        vm.boundary(cancelled)?;
        vm.restore_tasks(cancelled)?;
        vm.import_update(cancelled)?;
        let active = vm.tasks.active;
        if !vm.tasks.round.is_empty() {
            vm.tasks.active = None;
            vm.export(cancelled)?;
        }
        for id in vm.tasks.round.clone() {
            vm.checkpoint_task(id)?;
        }
        vm.tasks.active = active;
        vm.export(cancelled)?;
        let error = root.join("checkpoint-error");
        if error.exists() {
            fs::remove_file(error)?;
        }
        Ok(vm)
    }

    pub fn prepare_reviewer(&self, worker: i64, cancelled: &AtomicBool) -> io::Result<()> {
        self.guest(
            &[
                "/bin/mkdir",
                "-p",
                &crate::task_worktree::worker_home(worker),
            ],
            cancelled,
        )
    }

    pub fn worker_home(&self, id: i64) -> io::Result<PathBuf> {
        let home = self.home.join(format!("worker-{id}"));
        fs::create_dir_all(&home)?;
        private(&home, 0o700)?;
        if !home.join("auth.json").exists() {
            std::os::unix::fs::symlink(self.home.join("auth.json"), home.join("auth.json"))?;
        }
        fs::copy(
            self.home.join("environments.toml"),
            home.join("environments.toml"),
        )?;
        Ok(home)
    }

    fn login(&self) -> io::Result<()> {
        let login_home = env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".codex")
            });
        let login = login_home.join("auth.json").canonicalize().map_err(|_| {
            io::Error::other(
                "The VM worker needs a file-backed ChatGPT login. Run `codex -c 'cli_auth_credentials_store=\"file\"' login` on your Mac.",
            )
        })?;
        fs::create_dir_all(&self.home)?;
        private(&self.home, 0o700)?;
        let link = self.home.join("auth.json");
        // Only the host app-server sees this link. It is never copied into the guest.
        if !link.exists() {
            std::os::unix::fs::symlink(login, &link)?;
        }
        let program = env::split_paths(&env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join("container"))
            .find(|path| path.is_file())
            .ok_or_else(|| {
                io::Error::other("Install Apple Container, then run `container system start`.")
            })?;
        fs::write(
            self.home.join("environments.toml"),
            format!(
                "default=\"vm\"\ninclude_local=false\n[[environments]]\nid=\"vm\"\nprogram={}\nargs={}\ninitialize_timeout_sec=30\n",
                json!(program),
                json!(self.executor_args())
            ),
        )?;
        Ok(())
    }

    fn executor_args(&self) -> Vec<String> {
        [
            "exec",
            "--interactive",
            "--workdir",
            "/workspace",
            &self.name,
            "env",
            "-i",
            "HOME=/home/sprowt",
            "CODEX_HOME=/opt/codex-home",
            "PATH=/usr/local/bin:/usr/bin:/bin",
            "/usr/local/bin/codex",
            "exec-server",
            "--listen",
            "stdio",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    fn executor(&self) -> Command {
        let mut command = container();
        command.args(self.executor_args());
        command
    }

    fn inspect(&self) -> io::Result<Option<Value>> {
        let output = container()
            .args(["list", "--all", "--format", "json"])
            .output()?;
        checked(&output)?;
        let list: Vec<Value> = serde_json::from_slice(&output.stdout)?;
        Ok(list.into_iter().find(|value| value["id"] == self.name))
    }

    fn validate(&self, inspect: &Value, saved: Option<&Value>) -> io::Result<()> {
        let config = &inspect["configuration"];
        let isolated = ["mounts", "publishedPorts", "publishedSockets", "capAdd"]
            .iter()
            .all(|field| config[field].as_array().is_some_and(Vec::is_empty));
        if !isolated
            || config["ssh"] != false
            || config["platform"]["os"] != "linux"
            || config["labels"][OWNER] != self.name
            || config["image"]["reference"] != IMAGE
            || saved.is_some_and(|value| value["digest"] != config["image"]["descriptor"]["digest"])
        {
            return Err(io::Error::other(
                "The VM configuration changed; refusing to connect or delete it.",
            ));
        }
        Ok(())
    }

    fn control(&self, args: &[&str], cancelled: &AtomicBool) -> io::Result<()> {
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("sandbox.log"))?;
        private(&self.root.join("sandbox.log"), 0o600)?;
        let mut command = container();
        command.args(args).stdout(log.try_clone()?).stderr(log);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn()?;
        let deadline = Instant::now() + Duration::from_secs(900);
        loop {
            if let Some(status) = child.try_wait()? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(io::Error::other(format!(
                        "Apple Container failed. See {}.",
                        self.root.join("sandbox.log").display()
                    )))
                };
            }
            if cancelled.load(Ordering::Relaxed) || Instant::now() > deadline {
                rpc::terminate(&mut child);
                return Err(io::Error::other(
                    "VM setup stopped. Ctrl+R reconnects without changing your project.",
                ));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn image(&self, cancelled: &AtomicBool, progress: &impl Fn(&str)) -> io::Result<()> {
        let _build = loop {
            if let Ok(guard) = BUILD.try_lock() {
                break guard;
            }
            if cancelled.load(Ordering::Relaxed) {
                return Err(io::Error::other("VM setup stopped."));
            }
            thread::sleep(Duration::from_millis(100));
        };
        if container()
            .args(["image", "inspect", IMAGE])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success()
        {
            return Ok(());
        }
        progress("building Linux sandbox image · first run");
        let context = self.root.join("image");
        fs::create_dir_all(&context)?;
        fs::write(
            context.join("Containerfile"),
            include_str!("../sandbox/Containerfile"),
        )?;
        self.control(
            &[
                "build",
                "--progress",
                "plain",
                "--tag",
                IMAGE,
                context.to_str().unwrap(),
            ],
            cancelled,
        )
    }

    fn import(&self, cancelled: &AtomicBool) -> io::Result<()> {
        self.replace_guest_source(
            "/workspace",
            &workspace::source_state(&self.root.join("work"))?,
            cancelled,
        )
    }

    pub(crate) fn replace_guest_source(
        &self,
        destination: &str,
        source: &Snapshot,
        cancelled: &AtomicBool,
    ) -> io::Result<()> {
        let archive = self.root.join("source.tar");
        let mut builder = tar::Builder::new(fs::File::create(&archive)?);
        for (path, bytes, mode) in source {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(*mode);
            header.set_cksum();
            builder.append_data(&mut header, path, bytes.as_slice())?;
        }
        builder.finish()?;
        drop(builder);
        self.guest(&["/bin/mkdir", "-p", "/opt/sprowt-transfer"], cancelled)?;
        self.copy_in(&archive, "/opt/sprowt-transfer/import.tar", cancelled)?;
        // Destinations are harness-owned folders. Retain only their Git pointer.
        self.guest(
            &[
                "/usr/bin/find",
                destination,
                "-mindepth",
                "1",
                "-maxdepth",
                "1",
                "!",
                "-name",
                ".git",
                "-exec",
                "/bin/rm",
                "-rf",
                "--",
                "{}",
                ";",
            ],
            cancelled,
        )?;
        self.guest(
            &[
                "/usr/bin/tar",
                "--no-same-owner",
                "--same-permissions",
                "-xf",
                "/opt/sprowt-transfer/import.tar",
                "-C",
                destination,
            ],
            cancelled,
        )?;
        self.guest(&["/bin/rm", "/opt/sprowt-transfer/import.tar"], cancelled)?;
        fs::remove_file(archive)
    }

    pub(crate) fn guest(&self, args: &[&str], cancelled: &AtomicBool) -> io::Result<()> {
        let mut argv = vec!["exec", &self.name];
        argv.extend_from_slice(args);
        self.control(&argv, cancelled)
    }

    pub(crate) fn guest_exists(&self, path: &str) -> io::Result<bool> {
        self.guest_status(&["/usr/bin/test", "-e", path])
    }

    pub(crate) fn guest_status(&self, args: &[&str]) -> io::Result<bool> {
        let output = container().args(["exec", &self.name]).args(args).output()?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => {
                checked(&output)?;
                Ok(false)
            }
        }
    }

    pub(crate) fn copy_in(
        &self,
        path: &Path,
        destination: &str,
        cancelled: &AtomicBool,
    ) -> io::Result<()> {
        self.control(
            &[
                "copy",
                path.to_str()
                    .ok_or_else(|| io::Error::other("Invalid transfer path."))?,
                &format!("{}:{destination}", self.name),
            ],
            cancelled,
        )
    }

    pub(crate) fn copy_out(
        &mut self,
        source: &str,
        path: &Path,
        cancelled: &AtomicBool,
    ) -> io::Result<()> {
        let bytes = self.read_guest(source, cancelled)?;
        let next = path.with_extension("next");
        fs::write(&next, bytes)?;
        private(&next, 0o600)?;
        fs::rename(next, path)
    }

    fn profile(&self, name: &str, cwd: &str, home: &str) -> String {
        let domains = self
            .domains
            .iter()
            .map(|(domain, access)| format!("{}={}", json!(domain), json!(access)))
            .collect::<Vec<_>>()
            .join(",");
        // Codex 0.159.2 needs this guest-only grant for Chromium's local IPC.
        format!(
            "permissions.{name}={{filesystem={{\"/\"=\"read\",{}=\"write\",{}=\"read\",{}=\"write\",\"/tmp\"=\"write\"}},network={{enabled=true,domains={{{domains}}},allow_local_binding=true,dangerously_allow_all_unix_sockets=true}}}}",
            json!(cwd),
            json!(format!("{cwd}/.git")),
            json!(home)
        )
    }

    pub fn configuration(&self) -> Vec<String> {
        vec![
            self.profile("sprowt_vm", "/workspace", "/home/sprowt"),
            "default_permissions=\"sprowt_vm\"".into(),
            "features.network_proxy=true".into(),
            format!("shell_environment_policy.set.PATH={}", json!(GUEST_PATH)),
            "shell_environment_policy.set.HOME=\"/home/sprowt\"".into(),
        ]
    }

    pub fn worker_configuration(&self, worker: i64) -> Vec<String> {
        let home = crate::task_worktree::worker_home(worker);
        let mut config: Vec<_> = self
            .tasks
            .round
            .iter()
            .map(|id| {
                self.profile(
                    &format!("sprowt_task_{id}"),
                    &crate::task_worktree::folder(*id),
                    &home,
                )
            })
            .collect();
        config.push(format!("permissions.sprowt_review={{filesystem={{\"/\"=\"read\",{}=\"write\",\"/tmp\"=\"write\"}},network={{enabled=false}}}}", json!(home)));
        config.push(format!("shell_environment_policy.set.HOME={}", json!(home)));
        config
    }

    fn permissions(&self) -> Value {
        self.process_permissions(false)
    }

    fn process_permissions(&self, installing: bool) -> Value {
        let cwd = self.task_folder();
        let git = format!("{cwd}/.git");
        let home = self.runtime_home();
        let entries = [("/","read"),(cwd.as_str(),"write"),(git.as_str(),"read"),(home.as_str(),"write"),("/tmp","write")].map(|(path,access)| json!({"path":{"type":"path","path":format!("file://{path}")},"access":access}));
        let filesystem = if installing {
            json!({"type":"unrestricted"})
        } else {
            json!({"type":"restricted","entries":entries})
        };
        json!({"permissions":{"type":"managed","file_system":filesystem,"network":"enabled"},
            "cwd":format!("file://{cwd}"),"workspaceRoots":[format!("file://{cwd}")],"windowsSandboxLevel":"disabled","useLegacyLandlock":false})
    }

    fn run(
        &mut self,
        argv: &[String],
        cancelled: &AtomicBool,
        timeout: u64,
        cap: usize,
    ) -> io::Result<(Option<i64>, Vec<u8>, Vec<u8>)> {
        self.run_process(argv, cancelled, timeout, cap, false)
    }

    fn run_process(
        &mut self,
        argv: &[String],
        cancelled: &AtomicBool,
        timeout: u64,
        cap: usize,
        installing: bool,
    ) -> io::Result<(Option<i64>, Vec<u8>, Vec<u8>)> {
        let sandbox = self.process_permissions(installing);
        let proxy = json!({"proxy":{"enabled":true,"enableSocks5":false,"enableSocks5Udp":false,"allowUpstreamProxy":false,"dangerouslyAllowAllUnixSockets":!installing,"mode":"full","domains":self.domains,"unixSockets":{},"allowLocalBinding":true},"auditMetadata":{}});
        let task_folder = self.task_folder();
        let home = self.runtime_home();
        let rpc = self.rpc.as_mut().unwrap();
        let process = format!("sprowt-{}", PROCESS.fetch_add(1, Ordering::Relaxed));
        let mut environment = json!({"HOME":home,"PATH":GUEST_PATH,"LANG":"C.UTF-8","NO_PROXY":"localhost,127.0.0.1,::1"});
        if installing {
            environment["PATH"] = json!("/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin");
            environment["DEBIAN_FRONTEND"] = json!("noninteractive");
            environment["TMPDIR"] = json!("/opt/sprowt-apt/tmp");
            environment["APT_CONFIG"] = json!("/opt/sprowt-apt/apt.conf");
            environment["HOME"] = json!("/opt/sprowt-apt/home");
        }
        let cwd = if installing {
            "file:///opt/sprowt-apt".into()
        } else {
            format!("file://{task_folder}")
        };
        let start = rpc.call("process/start", json!({"processId":process,"argv":argv,"cwd":cwd,"env":environment,"envPolicy":{"inherit":"none","ignoreDefaultExcludes":true,"exclude":[],"set":{},"includeOnly":[]},"tty":false,"arg0":null,"sandbox":sandbox,"enforceManagedNetwork":true,"networkProxy":proxy}))?;
        if start["sandboxType"] != "linuxSeccomp" {
            return Err(io::Error::other(
                "Guest sandbox enforcement is unavailable.",
            ));
        }
        let mut cursor = 0;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(timeout);
        loop {
            if cancelled.load(Ordering::Relaxed) || Instant::now() > deadline {
                rpc.call("process/terminate", json!({"processId":process}))?;
                return Err(io::Error::other(
                    "Guest command stopped or exceeded its time limit.",
                ));
            }
            let read = rpc.call(
                "process/read",
                json!({"processId":process,"afterSeq":cursor,"maxBytes":65536,"waitMs":100}),
            )?;
            for chunk in read["chunks"]
                .as_array()
                .ok_or_else(|| io::Error::other("Invalid guest output."))?
            {
                let bytes = BASE64
                    .decode(chunk["chunk"].as_str().unwrap_or_default())
                    .map_err(io::Error::other)?;
                let output = if chunk["stream"] == "stdout" {
                    &mut stdout
                } else {
                    &mut stderr
                };
                if installing {
                    output.extend_from_slice(&bytes);
                    output.drain(..output.len().saturating_sub(cap));
                } else {
                    output.extend_from_slice(
                        &bytes[..bytes.len().min(cap.saturating_sub(output.len()))],
                    );
                }
            }
            cursor = read["nextSeq"]
                .as_u64()
                .map(|next| next.saturating_sub(1))
                .unwrap_or(cursor);
            rpc.buffered.clear();
            if read["closed"] == true && read["chunks"].as_array().is_some_and(Vec::is_empty) {
                if let Some(error) = read["failure"].as_str() {
                    return Err(io::Error::other(error.to_owned()));
                }
                return Ok((read["exitCode"].as_i64(), stdout, stderr));
            }
        }
    }

    pub fn install_packages(
        &mut self,
        request: &Request,
        cancelled: &AtomicBool,
    ) -> io::Result<String> {
        let mut log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("packages.jsonl"))?;
        private(&self.root.join("packages.jsonl"), 0o600)?;
        writeln!(
            log,
            "{}",
            json!({"packages":request.packages,"reason":request.reason,"status":"requested"})
        )?;
        let result = (|| {
            let inspect = self
                .inspect()?
                .ok_or_else(|| io::Error::other("The mod VM is missing."))?;
            let saved = serde_json::from_slice::<Value>(&fs::read(self.root.join("vm.json"))?)?;
            self.validate(&inspect, Some(&saved))?;
            if !self.domains.contains_key("deb.debian.org") {
                return Err(io::Error::other(
                    "Allow deb.debian.org in the Mac's network.json, then reopen the harness and retry.",
                ));
            }
            self.control(
                &[
                    "exec",
                    &self.name,
                    "/bin/mkdir",
                    "-p",
                    "/opt/sprowt-apt/tmp",
                    "/opt/sprowt-apt/cache/archives/partial",
                    "/opt/sprowt-apt/lists/partial",
                    "/opt/sprowt-apt/conf.d",
                    "/opt/sprowt-apt/home",
                ],
                cancelled,
            )?;
            self.offline_install(&["/usr/bin/dpkg", "--configure", "--pending"], cancelled)?;
            for action in ["update", "install"] {
                let mut argv = vec!["/usr/bin/apt-get".into(), action.into()];
                if action == "install" {
                    argv.extend([
                        "--download-only".into(),
                        "--yes".into(),
                        "--no-install-recommends".into(),
                        "--no-remove".into(),
                    ]);
                    argv.extend(request.packages.clone());
                }
                let (exit, stdout, stderr) = self.run_process(&argv, cancelled, 900, 8192, true)?;
                if exit != Some(0) {
                    return Err(io::Error::other(format!(
                        "System package {action} failed: {}{}",
                        String::from_utf8_lossy(&stdout),
                        String::from_utf8_lossy(&stderr)
                    )));
                }
            }
            let mut install = vec![
                "/usr/bin/apt-get",
                "install",
                "--yes",
                "--no-download",
                "--no-install-recommends",
                "--no-remove",
            ];
            install.extend(request.packages.iter().map(String::as_str));
            self.offline_install(&install, cancelled)?;
            Ok(format!(
                "Installed system packages in the mod VM: {}. Continue the task and rerun its checks.",
                request.packages.join(", ")
            ))
        })();
        writeln!(
            log,
            "{}",
            json!({"packages":request.packages,"status":if result.is_ok(){"installed"}else{"failed"},"result":result.as_ref().map_or_else(|error|error.to_string(),Clone::clone)})
        )?;
        result
    }

    fn offline_install(&self, command: &[&str], cancelled: &AtomicBool) -> io::Result<()> {
        let mut argv = vec![
            "exec",
            "--workdir",
            "/opt/sprowt-apt",
            &self.name,
            "/usr/bin/env",
            "-i",
            "PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            "HOME=/opt/sprowt-apt/home",
            "LANG=C.UTF-8",
            "DEBIAN_FRONTEND=noninteractive",
            "TMPDIR=/opt/sprowt-apt/tmp",
            "APT_CONFIG=/opt/sprowt-apt/apt.conf",
            "/opt/sprowt-apt/offline",
        ];
        argv.extend_from_slice(command);
        self.control(&argv, cancelled).map_err(|error| {
            let log = fs::read(self.root.join("sandbox.log")).unwrap_or_default();
            io::Error::other(format!(
                "{error} {}",
                String::from_utf8_lossy(&log[log.len().saturating_sub(8192)..])
            ))
        })
    }

    fn boundary(&mut self, cancelled: &AtomicBool) -> io::Result<()> {
        let code = "test \"$(uname -s)\" = Linux && (if test -e /root/.ssh || test -L /root/.ssh; then test -d /root/.ssh && ! test -L /root/.ssh && entries=$(find /root/.ssh -mindepth 1 -print -quit) && test -z \"$entries\"; fi) && test ! -e /home/sprowt/.codex/auth.json && test ! -e /opt/codex-home/auth.json && test -z \"${OPENAI_API_KEY}${CODEX_API_KEY}${CODEX_ACCESS_TOKEN}\" && ! touch /usr/local/bin/sprowt-canary && (curl -sI --max-time 5 https://sprowt-policy-check.invalid | grep -q '403 Forbidden') && ! curl --noproxy '*' --resolve github.com:443:140.82.112.3 -fsI --max-time 5 https://github.com >/dev/null && ! curl -fsI --max-time 5 http://192.168.64.1:80 >/dev/null";
        let (exit, _, error) = self.run(
            &["/bin/sh".into(), "-c".into(), code.into()],
            cancelled,
            40,
            8192,
        )?;
        if exit != Some(0) {
            return Err(io::Error::other(format!(
                "VM isolation check failed: {}",
                String::from_utf8_lossy(&error)
            )));
        }
        Ok(())
    }

    pub(crate) fn snapshot(&mut self, path: &str, cancelled: &AtomicBool) -> io::Result<Snapshot> {
        let mut argv = vec![
            "exec",
            &self.name,
            "/usr/bin/env",
            "-i",
            "PATH=/usr/bin:/bin",
            "/usr/bin/tar",
            "--format=gnu",
            "-C",
            path,
            "-cf",
            "/opt/sprowt-transfer/source.tar",
        ];
        let exclusions = [
            ".git",
            ".codex",
            "node_modules",
            "target",
            ".venv",
            "venv",
            "__pycache__",
            "*.pyc",
        ]
        .map(|part| format!("--exclude={part}"));
        argv.extend(exclusions.iter().map(String::as_str));
        argv.push(".");
        // Use a protected transfer file: process/read retains only 1 MiB of output.
        self.control(&argv, cancelled)?;
        let bytes = self.read_guest("/opt/sprowt-transfer/source.tar", cancelled)?;
        self.guest(
            &["/bin/rm", "-f", "/opt/sprowt-transfer/source.tar"],
            &AtomicBool::new(false),
        )?;
        workspace::source_snapshot(&self.root, decode_source(&bytes)?)
    }

    pub fn export(&mut self, cancelled: &AtomicBool) -> io::Result<Snapshot> {
        let snapshot = self.snapshot(&self.task_folder(), cancelled)?;
        if !self.tasks.round.is_empty() {
            self.commit_source(self.tasks.active, &snapshot, cancelled)?;
            self.save_draft(&snapshot)?;
            self.checkpoint_tasks(cancelled)?;
            let review = self.review_source(cancelled)?;
            workspace::replace_source(&self.root, &review)?;
        } else {
            workspace::replace_source(&self.root, &snapshot)?;
        }
        Ok(snapshot)
    }

    fn read_guest(&mut self, path: &str, cancelled: &AtomicBool) -> io::Result<Vec<u8>> {
        let permissions = self.permissions();
        let rpc = self.rpc.as_mut().unwrap();
        let handle = format!("export-{}", PROCESS.fetch_add(1, Ordering::Relaxed));
        rpc.call(
            "fs/open",
            json!({"handleId":handle,"path":format!("file://{path}"),"sandbox":permissions}),
        )?;
        let result = (|| {
            let mut bytes = Vec::new();
            loop {
                if cancelled.load(Ordering::Relaxed) {
                    return Err(io::Error::other("Source export stopped."));
                }
                let block = rpc.call(
                    "fs/readBlock",
                    json!({"handleId":handle,"offset":bytes.len(),"len":262144}),
                )?;
                let chunk = BASE64
                    .decode(block["chunk"].as_str().unwrap_or_default())
                    .map_err(io::Error::other)?;
                if chunk.is_empty() && block["eof"] != true {
                    return Err(io::Error::other(
                        "The guest source stream stopped unexpectedly.",
                    ));
                }
                if bytes.len() + chunk.len() > EXPORT_LIMIT {
                    return Err(io::Error::other(
                        "Source export exceeds 512 MiB. The previous export is retained.",
                    ));
                }
                bytes.extend(chunk);
                if block["eof"] == true {
                    return Ok(bytes);
                }
            }
        })();
        rpc.call("fs/close", json!({"handleId":handle}))?;
        result
    }

    pub fn verify(
        &mut self,
        checks: &[Check],
        cancelled: &AtomicBool,
    ) -> io::Result<(Snapshot, Vec<CheckResult>)> {
        let before = self.snapshot(&self.task_folder(), cancelled)?;
        let mut results = Vec::new();
        for check in checks {
            if cancelled.load(Ordering::Relaxed) {
                break;
            }
            let executable = check
                .command
                .first()
                .ok_or_else(|| io::Error::other("Verification requires an executable."))?;
            let probe = vec!["/usr/bin/test".into(), "-x".into(), executable.clone()];
            let available = match self.run(&probe, cancelled, 30, 8192) {
                Ok((Some(0), _, _)) => true,
                Ok((Some(1), _, _)) => false,
                result => {
                    results.push(CheckResult {
                        task: check.task,
                        check: check.check.clone(),
                        command: check.command.clone(),
                        exit_code: None,
                        output: match result {
                            Err(error) => error.to_string(),
                            Ok(_) => "Executable check was interrupted.".into(),
                        },
                        missing_runtime: None,
                    });
                    break;
                }
            };
            let missing_runtime = (!available).then(|| executable.clone());
            if let Some(path) = &missing_runtime {
                results.push(CheckResult {
                    task: check.task,
                    check: check.check.clone(),
                    command: check.command.clone(),
                    exit_code: None,
                    output: format!("Check executable is unavailable: {path}. Its task environment needs rebuilding."),
                    missing_runtime,
                });
                break;
            }
            let (exit_code, output) = match self.run(&check.command, cancelled, 30, 8192) {
                Ok((exit, stdout, stderr)) => (
                    exit,
                    format!(
                        "{}{}",
                        String::from_utf8_lossy(&stdout),
                        String::from_utf8_lossy(&stderr)
                    ),
                ),
                Err(error) => (None, error.to_string()),
            };
            results.push(CheckResult {
                task: check.task,
                check: check.check.clone(),
                command: check.command.clone(),
                exit_code,
                output,
                missing_runtime: None,
            });
            if exit_code != Some(0) {
                break;
            }
        }
        // Cancellation stops checks, but still exports partial edits for review.
        self.export(&AtomicBool::new(false))?;
        Ok((before, results))
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            rpc::terminate(child);
        }
        if self.started
            && self.inspect().ok().flatten().is_some_and(|value| {
                self.validate(&value, None).is_ok() && value["status"]["state"] == "running"
            })
        {
            let _ = container()
                .args(["stop", &self.name])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

pub fn delete(root: &Path) -> io::Result<()> {
    if !root.join("vm.json").exists() && !root.join("host-codex").exists() {
        return Ok(());
    }
    let vm = Sandbox {
        root: root.into(),
        name: name(root)?,
        home: root.join("host-codex"),
        domains: BTreeMap::new(),
        child: None,
        rpc: None,
        started: false,
        tasks: Default::default(),
    };
    if let Some(inspect) = vm.inspect()? {
        let saved = fs::read(root.join("vm.json"))
            .ok()
            .map(|bytes| serde_json::from_slice::<Value>(&bytes))
            .transpose()?;
        vm.validate(&inspect, saved.as_ref())?;
        vm.control(&["delete", "--force", &vm.name], &AtomicBool::new(false))?;
    }
    match fs::remove_file(root.join("vm.json")) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn name(root: &Path) -> io::Result<String> {
    let suffix = root
        .file_name()
        .and_then(|part| part.to_str())
        .unwrap_or("");
    if suffix.is_empty()
        || !suffix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(io::Error::other("Invalid VM workspace identity."));
    }
    Ok(format!("sprowt-{suffix}"))
}

pub(crate) fn network(root: &Path) -> io::Result<BTreeMap<String, String>> {
    let file = root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("Invalid workspace location."))?
        .join("network.json");
    if !file.exists() {
        fs::write(&file, include_str!("../sandbox/network.json"))?;
        private(&file, 0o600)?;
    }
    let domains: Vec<String> = serde_json::from_slice(&fs::read(&file)?)?;
    if domains.is_empty()
        || domains.iter().any(|domain| {
            domain.is_empty()
                || domain == "*"
                || domain.contains(['/', ':', '\\']) && domain != "::1"
        })
    {
        return Err(io::Error::other(format!(
            "Invalid domain allowlist in {}.",
            file.display()
        )));
    }
    let mut policy: BTreeMap<_, _> = domains
        .into_iter()
        .map(|domain| (domain, "allow".into()))
        .collect();
    policy.extend(crate::network::grants(root)?);
    Ok(policy)
}

fn container() -> Command {
    let mut command = Command::new("container");
    command.env_clear().envs(
        ["PATH", "HOME", "TMPDIR"]
            .iter()
            .filter_map(|name| env::var_os(name).map(|value| (*name, value))),
    );
    command
}

fn checked(output: &std::process::Output) -> io::Result<()> {
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}

fn private(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

fn decode_source(bytes: &[u8]) -> io::Result<Snapshot> {
    let mut files = BTreeMap::new();
    for entry in tar::Archive::new(bytes).entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let path = path.strip_prefix(".").unwrap_or(&path).to_owned();
        if path.as_os_str().is_empty() {
            continue;
        }
        if path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(io::Error::other("Invalid path in VM source export."));
        }
        if workspace::excluded(&path) {
            continue;
        }
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            continue;
        }
        if !kind.is_file() || entry.size() > 64 * 1024 * 1024 {
            return Err(io::Error::other(
                "VM source export requires regular files under 64 MiB; links are unsupported.",
            ));
        }
        let mode = entry.header().mode()? & 0o777;
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        if files.insert(path, (bytes, mode)).is_some() {
            return Err(io::Error::other("Duplicate path in VM source export."));
        }
    }
    Ok(files
        .into_iter()
        .map(|(path, (bytes, mode))| (path, bytes, mode))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::test_support::TestData;

    #[test]
    fn exports_only_regular_source_and_preserves_modes() {
        let mut archive = tar::Builder::new(Vec::new());
        for (path, content) in [
            ("./src/run.sh", "hello"),
            ("./.env", "secret"),
            ("./.env.example", "template"),
            ("./node_modules/a.js", "cache"),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            archive
                .append_data(&mut header, path, content.as_bytes())
                .unwrap();
        }
        let snapshot = decode_source(&archive.into_inner().unwrap()).unwrap();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(
            snapshot[1],
            (PathBuf::from("src/run.sh"), b"hello".to_vec(), 0o755)
        );
        let mut archive = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_link_name("/etc/passwd").unwrap();
        header.set_cksum();
        archive.append_data(&mut header, "source", &[][..]).unwrap();
        assert!(decode_source(&archive.into_inner().unwrap()).is_err());
    }

    #[test]
    #[ignore = "repairs generated files in legacy VM checkpoints without deleting runtime data"]
    fn legacy_checkpoints_drop_generated_files_and_keep_vm_data() {
        let data = TestData::new();
        let project = data.0.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("first.txt"), "before").unwrap();
        let root = data.0.join(format!("artifacts-{}", std::process::id()));
        workspace::create(&project, &root).unwrap();
        let flag = AtomicBool::new(false);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> io::Result<()> {
                let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
                vm.prepare_tasks(&[1], &flag)?;
                vm.guest(&["/bin/sh", "-c", "set -eu; git=/opt/sprowt-git/repo; export GIT_DIR=$git GIT_WORK_TREE=/workspace; /usr/bin/git update-ref refs/heads/start integration; mkdir -p /workspace/.pytest_cache/v/cache /workspace/data; printf cache > /workspace/.pytest_cache/v/cache/nodeids; printf database > /workspace/data/todos.duckdb; printf wal > /workspace/data/todos.duckdb.wal; printf requested > /workspace/first.txt; /usr/bin/git add --force --all; /usr/bin/git -c user.name=Sprowt -c user.email=sprowt@localhost commit -m legacy; /usr/bin/git worktree add -b task/1 /tasks/1 integration"], &flag)?;
                vm.tasks.active = Some(1);
                fs::write(root.join("tasks.json"), serde_json::to_vec(&vm.tasks)?)?;
                vm.checkpoint_tasks(&flag)?;
                drop(vm);
                let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
                vm.guest(&["/bin/sh", "-c", "set -eu; export GIT_DIR=/opt/sprowt-git/repo GIT_WORK_TREE=/workspace; test \"$(git diff --name-only start integration)\" = first.txt; test \"$(git diff --name-only start task/1)\" = first.txt; test \"$(cat /workspace/data/todos.duckdb)\" = database; test \"$(cat /tasks/1/data/todos.duckdb)\" = database; test -f /workspace/.pytest_cache/v/cache/nodeids"], &flag)?;
                let source = vm.export(&flag)?;
                assert_eq!(
                    source,
                    vec![(PathBuf::from("first.txt"), b"requested".to_vec(), 0o644)]
                );
                assert_eq!(
                    workspace::review(&root)?.paths().collect::<Vec<_>>(),
                    [Path::new("first.txt")]
                );
                Ok(())
            },
        ));
        delete(&root).unwrap();
        result.unwrap().unwrap();
    }

    #[test]
    #[ignore = "recovers an old metadata merge conflict in a disposable Apple Container VM"]
    fn tracked_package_metadata_does_not_conflict_with_parallel_source_edits() {
        let data = TestData::new();
        let project = data.0.join("project");
        fs::create_dir_all(project.join("local.egg-info")).unwrap();
        fs::write(project.join("first.txt"), "before").unwrap();
        fs::write(project.join("local.egg-info/SOURCES.txt"), "baseline\n").unwrap();
        for args in [
            vec!["init"],
            vec!["add", "."],
            vec!["commit", "-m", "Baseline"],
        ] {
            checked(
                &workspace::trusted_git()
                    .arg("-C")
                    .arg(&project)
                    .args(args)
                    .output()
                    .unwrap(),
            )
            .unwrap();
        }
        let root = data.0.join(format!("metadata-{}", std::process::id()));
        workspace::create(&project, &root).unwrap();
        let flag = AtomicBool::new(false);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> io::Result<()> {
                let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
                vm.prepare_tasks(&[1, 2], &flag)?;
                vm.activate_task(1, &flag)?;
                vm.activate_task(2, &flag)?;
                // Reproduce checkpoints made before metadata was frozen to the baseline.
                vm.guest(&["/bin/sh", "-c", r#"set -eu
                saved_git() { /usr/bin/git -c user.name=Sprowt -c user.email=sprowt@localhost "$@"; }
                cd /tasks/1
                saved_git update-index --no-skip-worktree -- local.egg-info/SOURCES.txt
                printf runtime > runtime.mjs
                printf 'runtime.mjs\n' > local.egg-info/SOURCES.txt
                saved_git add .; saved_git commit -m runtime
                cd /tasks/2
                saved_git update-index --no-skip-worktree -- local.egg-info/SOURCES.txt
                printf panel > panel.mjs
                printf 'panel.mjs\n' > local.egg-info/SOURCES.txt
                saved_git add .; saved_git commit -m panel
                cd /workspace
                saved_git update-index --no-skip-worktree -- local.egg-info/SOURCES.txt
                saved_git reset --hard integration
                saved_git merge --no-ff --no-edit task/2
                if saved_git merge --no-ff --no-edit task/1; then exit 1; fi
                saved_git merge --abort
                cd /tasks/1
                if saved_git merge --no-ff --no-edit integration; then exit 1; fi
                saved_git add .; saved_git commit -m conflict
                grep -q '<<<<<<<' local.egg-info/SOURCES.txt
            "#], &flag)?;
                vm.tasks.integrated.insert(2);
                vm.tasks.active = Some(1);
                fs::write(root.join("tasks.json"), serde_json::to_vec(&vm.tasks)?)?;
                vm.checkpoint_tasks(&flag)?;
                drop(vm);
                let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
                let check = Check { task: Some(1), check: "Runtime works".into(), command: vec!["/bin/sh".into(), "-c".into(), "test \"$(cat runtime.mjs)\" = runtime && test \"$(cat panel.mjs)\" = panel".into()] };
                assert_eq!(
                    vm.verify_execution(
                        "00000004-0002-4000-8000-000000000001",
                        std::slice::from_ref(&check),
                        &flag
                    )?
                    .1[0]
                        .exit_code,
                    Some(0)
                );
                vm.guest(&["/bin/sh", "-c", "set -eu; grep -q '<<<<<<<' /tasks/1/local.egg-info/SOURCES.txt; cd /tasks/1; ! git diff --name-only | grep -q egg-info; test \"$(git show HEAD:local.egg-info/SOURCES.txt)\" = baseline"], &flag)?;
                let source = vm.snapshot("/workspace", &flag)?;
                assert_eq!(source, workspace::source_state(&root.join("work"))?);
                let review = workspace::review(&root)?;
                assert_eq!(
                    review.paths().collect::<Vec<_>>(),
                    [Path::new("panel.mjs"), Path::new("runtime.mjs")]
                );
                assert!(!review.patch.contains("egg-info"));
                let (_, checks) = vm.verify_execution("final:1", &[check], &flag)?;
                assert_eq!(checks[0].exit_code, Some(0));
                Ok(())
            },
        ));
        if matches!(&result, Ok(Err(_))) {
            eprintln!(
                "{}",
                fs::read_to_string(root.join("sandbox.log")).unwrap_or_default()
            );
        }
        delete(&root).unwrap();
        result.unwrap().unwrap();
    }

    #[test]
    #[ignore = "checks repair handoff, retained drafts, restart and verification in a disposable VM"]
    fn repair_handoff_preserves_drafts_and_rechecks_in_vm() {
        let data = TestData::new();
        let project = data.0.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("runtime.txt"), "original").unwrap();
        let root = data.0.join(format!("repair-{}", std::process::id()));
        workspace::create(&project, &root).unwrap();
        let flag = AtomicBool::new(false);
        let command = |script: &str| vec!["/bin/sh".into(), "-c".into(), script.into()];
        let check = |id: i64, script: &str| Check {
            task: Some(id),
            check: "Regression check".into(),
            command: command(script),
        };
        let source = |id: i64, attempt: i64| crate::store::task_source(id, attempt);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> io::Result<()> {
                let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
                vm.prepare_tasks(&[1, 2, 3], &flag)?;
                vm.assign_task(1, 101, &flag)?;
                assert_eq!(vm.run(&command("printf buggy > runtime.txt; mkdir -p .venv; printf installed > .venv/proof"),&flag,30,8192)?.0,Some(0));
                vm.verify_execution(&source(1, 1), &[check(1, "test -s runtime.txt")], &flag)?;
                vm.assign_task(2, 102, &flag)?;
                assert_eq!(
                    vm.run(&command("printf ui > ui.txt"), &flag, 30, 8192)?.0,
                    Some(0)
                );
                vm.verify_execution(&source(2, 1), &[check(2, "test -s ui.txt")], &flag)?;
                vm.assign_task(3, 101, &flag)?;
                assert_eq!(
                    vm.run(
                        &command(
                            "printf diagnostics > integration.txt; printf documentation > README.md"
                        ),
                        &flag,
                        30,
                        8192
                    )?
                    .0,
                    Some(0)
                );
                let (_, failed) = vm.verify_execution(
                    &source(3, 1),
                    &[check(3, "test \"$(cat runtime.txt)\" = fixed")],
                    &flag,
                )?;
                assert_ne!(failed[0].exit_code, Some(0));
                vm.assign_task(1, 101, &flag)?;
                vm.refresh_for_repair(1, &source(1, 2), &flag)?;
                assert_eq!(vm.run(&command("test \"$(cat ui.txt)\" = ui && test \"$(cat .venv/proof)\" = installed && printf fixed > runtime.txt && ! touch /tasks/2/denied"),&flag,30,8192)?.0,Some(0));
                vm.export(&flag)?;
                drop(vm);
                let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
                vm.assign_task(1, 101, &flag)?;
                vm.refresh_for_repair(1, &source(1, 2), &flag)?;
                // The same delivery and an explicit retry both preserve the saved fix.
                vm.refresh_for_repair(1, &source(1, 3), &flag)?;
                let fixed = check(
                    1,
                    "test \"$(cat runtime.txt)\" = fixed && test \"$(cat .venv/proof)\" = installed",
                );
                assert_eq!(
                    vm.verify_execution(&source(1, 3), std::slice::from_ref(&fixed), &flag)?
                        .1[0]
                        .exit_code,
                    Some(0)
                );
                vm.assign_task(3, 101, &flag)?;
                vm.refresh_for_repair(3, &source(3, 2), &flag)?;
                let integration = check(
                    3,
                    "test \"$(cat runtime.txt)\" = fixed && test \"$(cat integration.txt)\" = diagnostics && test \"$(cat README.md)\" = documentation && test \"$(cat ui.txt)\" = ui",
                );
                assert_eq!(
                    vm.verify_execution(&source(3, 2), std::slice::from_ref(&integration), &flag)?
                        .1[0]
                        .exit_code,
                    Some(0)
                );
                let (_, results) = vm.verify_execution(
                    "final:repair",
                    &[
                        fixed,
                        check(2, "test \"$(cat runtime.txt)\" = fixed && test -s ui.txt"),
                        integration,
                    ],
                    &flag,
                )?;
                assert_eq!(results.len(), 3);
                assert!(results.iter().all(|result| result.exit_code == Some(0)));
                for id in [1, 2, 3] {
                    assert!(vm.guest_exists(&format!("/tasks/{id}"))?);
                }
                assert_eq!(fs::read(root.join("work/runtime.txt"))?, b"fixed");
                assert_eq!(fs::read(root.join("work/integration.txt"))?, b"diagnostics");
                Ok(())
            },
        ));
        delete(&root).unwrap();
        result.unwrap().unwrap();
    }

    #[test]
    #[ignore = "checks runtime retention and missing executable recovery in a disposable VM"]
    fn review_rechecks_retain_task_runtimes() {
        let data = TestData::new();
        let project = data.0.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("source.txt"), "original").unwrap();
        let root = data.0.join(format!("runtime-{}", std::process::id()));
        workspace::create(&project, &root).unwrap();
        let flag = AtomicBool::new(false);
        let source = |id| crate::store::task_source(id, 1);
        let command = |script: &str| vec!["/bin/sh".into(), "-c".into(), script.into()];
        let check = |id| Check {
            task: Some(id),
            check: "Runtime sees combined source".into(),
            command: vec![format!("/tasks/{id}/.venv/bin/check")],
        };
        let setup = command(
            "mkdir -p .venv/bin; printf '#!/bin/sh\ntest -s source.txt\n' > .venv/bin/check; chmod +x .venv/bin/check",
        );
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> io::Result<()> {
                let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
                vm.prepare_tasks(&[1, 2], &flag)?;
                for id in [1, 2] {
                    vm.assign_task(id, 100 + id, &flag)?;
                    assert_eq!(vm.run(&setup, &flag, 30, 8192)?.0, Some(0));
                    assert_eq!(
                        vm.verify_execution(&source(id), &[check(id)], &flag)?.1[0].exit_code,
                        Some(0)
                    );
                }
                let checks = [check(1), check(2)];
                assert!(
                    vm.verify_execution("final:test", &checks, &flag)?
                        .1
                        .iter()
                        .all(|c| c.exit_code == Some(0))
                );
                drop(vm);
                let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
                // A review reopens only task 2; task 1 must still be runnable afterward.
                vm.assign_task(2, 102, &flag)?;
                let retry = crate::store::task_source(2, 2);
                vm.refresh_for_repair(2, &retry, &flag)?;
                assert_eq!(
                    vm.run(&command("printf reviewed > source.txt"), &flag, 30, 8192)?
                        .0,
                    Some(0)
                );
                assert_eq!(
                    vm.verify_execution(&retry, &[check(2)], &flag)?.1[0].exit_code,
                    Some(0)
                );
                let (before, results) = vm.verify_execution("final:test", &checks, &flag)?;
                assert_eq!(results.len(), 2);
                assert!(results.iter().all(|c| c.exit_code == Some(0)));
                // Simulate an environment removed by an older harness.
                vm.guest(&["/bin/rm", "/tasks/1/.venv/bin/check"], &flag)?;
                let (_, results) = vm.verify_execution("final:test", &checks, &flag)?;
                assert_eq!(results.len(), 1);
                assert_eq!(
                    results[0].missing_runtime.as_deref(),
                    Some("/tasks/1/.venv/bin/check")
                );
                assert!(results[0].failed() && !results[0].output.contains("panicked"));
                vm.assign_task(1, 101, &flag)?;
                let retry = crate::store::task_source(1, 2);
                vm.refresh_for_repair(1, &retry, &flag)?;
                assert_eq!(vm.run(&setup, &flag, 30, 8192)?.0, Some(0));
                vm.verify_execution(&retry, &[check(1)], &flag)?;
                let (after, results) = vm.verify_execution("final:test", &checks, &flag)?;
                assert_eq!(before, after);
                assert_eq!(results.len(), 2);
                assert!(results.iter().all(|c| c.exit_code == Some(0)));
                vm.prepare_tasks(&[3], &flag)?;
                assert!(!vm.guest_exists("/tasks/1")? && !vm.guest_exists("/tasks/2")?);
                Ok(())
            }));
        delete(&root).unwrap();
        result.unwrap().unwrap();
    }

    #[test]
    #[ignore = "checks task worktrees, recovery and cleanup in a temporary Apple Container VM"]
    fn task_worktrees_isolate_combine_restore_and_clean() {
        let data = TestData::new();
        let project = data.0.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("first.txt"), "before").unwrap();
        let parent = data.0.join("workspaces");
        fs::create_dir(&parent).unwrap();
        let root = parent.join(format!("tasks-{}", std::process::id()));
        workspace::create(&project, &root).unwrap();
        let flag = AtomicBool::new(false);
        let command = |script: &str| vec!["/bin/sh".into(), "-c".into(), script.into()];
        let check = |task: i64, script: &str| Check {
            task: Some(task),
            check: "Source is correct".into(),
            command: command(script),
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> io::Result<()> {
                let mut vm = Sandbox::prepare(&root, &flag, |label| eprintln!("{label}"))?;
                vm.prepare_tasks(&[1, 2, 3], &flag)?;
                vm.activate_task(1, &flag)?;
                let (exit, _, stderr) = vm.run(&command("test \"$PWD\" = /tasks/1 && printf one > first.txt && ! touch /workspace/denied && ! touch /opt/sprowt-git/repo/config && ! sh -c 'echo bad > .git' && mkdir -p .venv && printf cached > .venv/proof"), &flag, 30, 8192)?;
                assert_eq!(exit, Some(0), "{}", String::from_utf8_lossy(&stderr));
                assert_eq!(vm.snapshot("/workspace", &flag)?[0].1, b"before");
                let first = check(
                    1,
                    "test \"$(cat first.txt)\" = one && test \"$(cat .venv/proof)\" = cached",
                );
                let source = |id| format!("00000004-0001-4000-8000-{id:012x}");
                assert_eq!(
                    vm.verify_execution(&source(1), std::slice::from_ref(&first), &flag)?
                        .1[0]
                        .exit_code,
                    Some(0)
                );
                assert_eq!(vm.snapshot("/workspace", &flag)?[0].1, b"one");
                vm.activate_task(2, &flag)?;
                assert_eq!(vm.run(&command("test \"$(cat first.txt)\" = one && printf draft > second.txt && ! touch /tasks/1/denied"), &flag, 30, 8192)?.0, Some(0));
                vm.export(&flag)?;
                drop(vm);
                // Closing deletes the VM, retaining only sanitized source and Git objects.
                delete(&root)?;
                let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
                assert_eq!(vm.task_folder(), "/tasks/2");
                assert_eq!(
                    vm.run(
                        &command("test \"$(cat second.txt)\" = draft && printf two > second.txt"),
                        &flag,
                        30,
                        8192
                    )?
                    .0,
                    Some(0)
                );
                let second = check(
                    2,
                    "test \"$(cat first.txt)\" = one && test \"$(cat second.txt)\" = two",
                );
                assert_eq!(
                    vm.verify_execution(&source(2), std::slice::from_ref(&second), &flag)?
                        .1[0]
                        .exit_code,
                    Some(0)
                );
                // The next task starts from both integrated changes.
                vm.activate_task(3, &flag)?;
                assert_eq!(vm.run(&command("test \"$(cat second.txt)\" = two && printf three > third.txt && printf secret > .env"), &flag, 30, 8192)?.0, Some(0));
                let third = check(3, "test \"$(cat third.txt)\" = three");
                vm.verify_execution(&source(3), std::slice::from_ref(&third), &flag)?;
                let checks = [check(1, "test \"$(cat third.txt)\" = three"), second, third];
                let (before, results) = vm.verify_execution("final:1", &checks, &flag)?;
                assert_eq!(results.len(), 3);
                assert!(
                    results.iter().all(|r| r.exit_code == Some(0)),
                    "Check failed"
                );
                assert_eq!(before, workspace::source_state(&root.join("work"))?);
                assert_eq!(before.len(), 3);
                assert!(
                    vm.guest_exists("/tasks/1")?
                        && vm.guest_exists("/tasks/2")?
                        && vm.guest_exists("/tasks/3")?
                );
                drop(vm);
                let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
                assert!(vm.guest_exists("/tasks/1")?);
                // A new edit round retains combined source and removes old task branches.
                vm.prepare_tasks(&[4], &flag)?;
                assert!(
                    !vm.guest_exists("/tasks/1")?
                        && !vm.guest_exists("/tasks/2")?
                        && !vm.guest_exists("/tasks/3")?
                );
                vm.activate_task(4, &flag)?;
                assert_eq!(
                    vm.run(
                        &command("test \"$(cat third.txt)\" = three"),
                        &flag,
                        30,
                        8192
                    )?
                    .0,
                    Some(0)
                );
                assert!(!vm.guest_exists("/opt/sprowt-git/repo/refs/heads/task/1")?);
                // A changed check cannot complete a task.
                let bad = check(4, "printf changed > third.txt");
                let (before, _) = vm.verify_execution(&source(4), &[bad], &flag)?;
                assert_ne!(before, workspace::source_state(&root.join("work"))?);
                assert!(!vm.tasks.integrated.contains(&4));
                // Keep both sides of a merge conflict, including across VM deletion.
                vm.prepare_tasks(&[5, 6], &flag)?;
                vm.activate_task(5, &flag)?;
                vm.activate_task(6, &flag)?;
                vm.run(
                    &command("printf six > first.txt; printf shared > combined.txt"),
                    &flag,
                    30,
                    8192,
                )?;
                let sixth = check(6, "test \"$(cat first.txt)\" = six");
                vm.verify_execution(&source(6), &[sixth], &flag)?;
                vm.activate_task(5, &flag)?;
                vm.run(&command("printf five > first.txt"), &flag, 30, 8192)?;
                let fifth = check(5, "test \"$(cat first.txt)\" = five");
                assert!(
                    vm.verify_execution(&source(5), &[fifth], &flag)
                        .err()
                        .unwrap()
                        .to_string()
                        .contains("conflict")
                );
                assert_eq!(
                    vm.snapshot("/workspace", &flag)?
                        .iter()
                        .find(|f| f.0 == Path::new("first.txt"))
                        .unwrap()
                        .1,
                    b"six"
                );
                vm.export(&flag)?;
                drop(vm);
                delete(&root)?;
                let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
                assert_eq!(
                    vm.snapshot("/workspace", &flag)?
                        .iter()
                        .find(|f| f.0 == Path::new("first.txt"))
                        .unwrap()
                        .1,
                    b"six"
                );
                let draft = vm.snapshot("/tasks/5", &flag)?;
                let conflict = String::from_utf8_lossy(
                    &draft
                        .iter()
                        .find(|f| f.0 == Path::new("first.txt"))
                        .unwrap()
                        .1,
                );
                assert!(
                    conflict.contains("<<<<<<<")
                        && conflict.contains("five")
                        && conflict.contains("six")
                );
                assert!(
                    draft
                        .iter()
                        .any(|f| f.0 == Path::new("combined.txt") && f.1 == b"shared")
                );
                vm.prepare_tasks(&[7, 8], &flag)?;
                vm.assign_task(7, 101, &flag)?;
                assert_eq!(
                    vm.run(
                        &command(
                            "printf seven > seven.txt; printf private > \"$HOME/runtime-proof\""
                        ),
                        &flag,
                        30,
                        8192
                    )?
                    .0,
                    Some(0)
                );
                vm.checkpoint_task(7)?;
                vm.assign_task(8, 102, &flag)?;
                assert_eq!(
                    vm.run(
                        &command(
                            "printf eight > eight.txt; ! touch /home/sprowt/workers/101/denied"
                        ),
                        &flag,
                        30,
                        8192
                    )?
                    .0,
                    Some(0)
                );
                vm.checkpoint_task(8)?;
                assert_eq!(fs::read_to_string(root.join("work/seven.txt"))?, "seven");
                assert_eq!(fs::read_to_string(root.join("work/eight.txt"))?, "eight");
                drop(vm);
                delete(&root)?;
                let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
                vm.assign_task(7, 101, &flag)?;
                assert_eq!(vm.run(&command("test \"$(cat seven.txt)\" = seven && test ! -e eight.txt && test ! -e \"$HOME/runtime-proof\""), &flag, 30, 8192)?.0, Some(0));
                vm.assign_task(8, 102, &flag)?;
                assert_eq!(
                    vm.run(
                        &command("test \"$(cat eight.txt)\" = eight && test ! -e seven.txt"),
                        &flag,
                        30,
                        8192
                    )?
                    .0,
                    Some(0)
                );
                assert_eq!(fs::read_to_string(root.join("work/seven.txt"))?, "seven");
                assert_eq!(fs::read_to_string(root.join("work/eight.txt"))?, "eight");
                drop(vm);
                Ok(())
            },
        ));
        delete(&root).unwrap();
        result.unwrap().unwrap();
    }

    #[test]
    #[ignore = "installs real Debian packages in a temporary Apple Container VM"]
    fn system_packages_persist_without_widening_worker_permissions() {
        let data = TestData::new();
        let project = data.0.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("keep.txt"), "source stays").unwrap();
        let parent = data.0.join("workspaces");
        fs::create_dir(&parent).unwrap();
        let root = parent.join(format!("packages-{}", std::process::id()));
        workspace::create(&project, &root).unwrap();
        let cancelled = AtomicBool::new(false);
        let request =
            Request::parse(json!({"packages":["jq","fonts-liberation","libfontconfig1","libglib2.0-0","libnss3"],"reason":"Read fixture JSON and prepare browser libraries"})).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> io::Result<()> {
                let mut vm = Sandbox::prepare(&root, &cancelled, |label| eprintln!("{label}"))?;
                vm.domains.remove("deb.debian.org");
                assert!(
                    vm.install_packages(&request, &cancelled)
                        .unwrap_err()
                        .to_string()
                        .contains("network.json")
                );
                vm.domains.insert("deb.debian.org".into(), "allow".into());
                let probe = root.join("network-test.c");
                fs::write(
                    &probe,
                    "#include <errno.h>\n#include <sys/socket.h>\n#include <linux/netlink.h>\nint main(void) { if (socket(AF_INET, SOCK_STREAM, 0) != -1 || errno != EPERM) return 1; return socket(AF_NETLINK, SOCK_RAW, NETLINK_AUDIT) == -1 && errno == EAFNOSUPPORT ? 0 : 1; }\n",
                )?;
                vm.control(
                    &[
                        "copy",
                        probe.to_str().unwrap(),
                        &format!("{}:/opt/sprowt-apt/network-test.c", vm.name),
                    ],
                    &cancelled,
                )?;
                vm.control(
                    &[
                        "exec",
                        &vm.name,
                        "/usr/bin/cc",
                        "/opt/sprowt-apt/network-test.c",
                        "-o",
                        "/opt/sprowt-apt/network-test",
                    ],
                    &cancelled,
                )?;
                vm.offline_install(&["/opt/sprowt-apt/network-test"], &cancelled)?;
                vm.install_packages(&request, &cancelled)?;
                vm.control(
                    &["exec", &vm.name, "/bin/mkdir", "-p", "/root/.ssh"],
                    &cancelled,
                )?;
                vm.boundary(&cancelled)?;
                vm.control(
                    &[
                        "exec",
                        &vm.name,
                        "/usr/bin/touch",
                        "/root/.ssh/credential-canary",
                    ],
                    &cancelled,
                )?;
                assert!(vm.boundary(&cancelled).is_err());
                vm.control(
                    &["exec", &vm.name, "/bin/rm", "/root/.ssh/credential-canary"],
                    &cancelled,
                )?;
                let checks = vec![Check { task: None, check:"Package installed and source retained".into(),command:vec!["/bin/sh".into(),"-c".into(),"/usr/bin/jq --version && test \"$(cat keep.txt)\" = 'source stays' && ! touch /opt/sprowt-apt/worker-write".into()]}];
                assert_eq!(vm.verify(&checks, &cancelled)?.1[0].exit_code, Some(0));
                drop(vm);
                let mut vm = Sandbox::prepare(&root, &cancelled, |_| {})?;
                assert_eq!(vm.verify(&checks, &cancelled)?.1[0].exit_code, Some(0));
                drop(vm);
                let log = fs::read_to_string(root.join("packages.jsonl"))?;
                assert!(
                    log.contains("\"status\":\"failed\"")
                        && log.contains("\"status\":\"installed\"")
                );
                assert_eq!(
                    fs::read_to_string(project.join("keep.txt"))?,
                    "source stays"
                );
                Ok(())
            },
        ));
        delete(&root).unwrap();
        result.unwrap().unwrap();
    }

    #[test]
    #[ignore = "starts an Apple Container VM and checks real network enforcement"]
    fn persistent_vm_checks_network_exports_source_and_reconnects() {
        let data = TestData::new();
        let project = data.0.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("delete.txt"), "original").unwrap();
        let large = vec![42; 3 * 1024 * 1024];
        fs::write(project.join("large.bin"), &large).unwrap();
        let parent = data.0.join("workspaces");
        fs::create_dir(&parent).unwrap();
        let root = parent.join(format!("1-{}", std::process::id()));
        workspace::create(&project, &root).unwrap();
        let cancelled = AtomicBool::new(false);
        let result = (|| -> io::Result<()> {
            let mut vm = Sandbox::prepare(&root, &cancelled, |label| eprintln!("{label}"))?;
            let commands = [
                "printf edited > result.txt; rm delete.txt; chmod 755 result.txt; printf secret > .env; mkdir -p .venv; printf cache > .venv/cache",
                "curl -fsI --max-time 15 https://github.com >/dev/null",
                "! curl --noproxy '*' --resolve github.com:443:140.82.112.3 -fsI --max-time 5 https://github.com >/dev/null",
                "! curl -fsI --max-time 5 https://example.com >/dev/null",
                "! curl -fsI --max-time 5 http://192.168.64.1:80 >/dev/null",
                "printf persistent > /home/sprowt/runtime-proof",
            ];
            for command in commands {
                let (exit, _, error) = vm.run(
                    &["/bin/sh".into(), "-c".into(), command.into()],
                    &cancelled,
                    30,
                    8192,
                )?;
                assert_eq!(exit, Some(0), "{}", String::from_utf8_lossy(&error));
            }
            let snapshot = vm.export(&cancelled)?;
            assert_eq!(snapshot.len(), 2);
            assert_eq!(snapshot[0].0, PathBuf::from("large.bin"));
            assert_eq!(snapshot[0].1, large);
            assert_eq!(
                snapshot[1],
                (PathBuf::from("result.txt"), b"edited".to_vec(), 0o755)
            );
            assert!(!root.join("work/.env").exists() && !root.join("work/.venv").exists());
            assert_eq!(fs::read_to_string(project.join("delete.txt"))?, "original");
            drop(vm);
            let mut vm = Sandbox::prepare(&root, &cancelled, |label| eprintln!("{label}"))?;
            let checks = vec![Check { task: None, check: "runtime persisted".into(), command: vec!["/bin/sh".into(), "-c".into(), "test \"$(cat /home/sprowt/runtime-proof)\" = persistent && test \"$(cat result.txt)\" = edited".into()] }];
            let (before, results) = vm.verify(&checks, &cancelled)?;
            assert_eq!(results[0].exit_code, Some(0));
            assert_eq!(before, workspace::source_state(&root.join("work"))?);
            drop(vm);
            Ok(())
        })();
        delete(&root).unwrap();
        result.unwrap();
    }
}
