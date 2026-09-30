use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, Read},
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
}

impl Sandbox {
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
        vm.export(cancelled)?;
        Ok(vm)
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
        command.args(args).stdout(Stdio::null()).stderr(log);
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
        let archive = self.root.join("source.tar");
        let mut builder = tar::Builder::new(fs::File::create(&archive)?);
        for (path, bytes, mode) in workspace::source_state(&self.root.join("work"))? {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(mode);
            header.set_cksum();
            builder.append_data(&mut header, path, bytes.as_slice())?;
        }
        builder.finish()?;
        drop(builder);
        self.control(
            &[
                "copy",
                archive.to_str().unwrap(),
                &format!("{}:/tmp/sprowt-source.tar", self.name),
            ],
            cancelled,
        )?;
        self.control(&["exec", &self.name, "/bin/sh", "-c", "tar --no-same-owner --same-permissions -xf /tmp/sprowt-source.tar -C /workspace && rm /tmp/sprowt-source.tar"], cancelled)?;
        fs::remove_file(archive)
    }

    pub fn configuration(&self) -> Vec<String> {
        let domains = self
            .domains
            .iter()
            .map(|(domain, access)| format!("{}={}", json!(domain), json!(access)))
            .collect::<Vec<_>>()
            .join(",");
        vec![
            format!(
                "permissions.sprowt_vm={{filesystem={{\"/\"=\"read\",\"/workspace\"=\"write\",\"/home/sprowt\"=\"write\",\"/tmp\"=\"write\"}},network={{enabled=true,domains={{{domains}}},allow_local_binding=true}}}}"
            ),
            "default_permissions=\"sprowt_vm\"".into(),
            "features.network_proxy=true".into(),
            format!("shell_environment_policy.set.PATH={}", json!(GUEST_PATH)),
            "shell_environment_policy.set.HOME=\"/home/sprowt\"".into(),
        ]
    }

    fn permissions(&self) -> Value {
        let entries = [("/","read"),("/workspace","write"),("/home/sprowt","write"),("/tmp","write")].map(|(path,access)| json!({"path":{"type":"path","path":format!("file://{path}")},"access":access}));
        json!({"permissions":{"type":"managed","file_system":{"type":"restricted","entries":entries},"network":"enabled"},
            "cwd":"file:///workspace","workspaceRoots":["file:///workspace"],"windowsSandboxLevel":"disabled","useLegacyLandlock":false})
    }

    fn run(
        &mut self,
        argv: &[String],
        cancelled: &AtomicBool,
        timeout: u64,
        cap: usize,
    ) -> io::Result<(Option<i64>, Vec<u8>, Vec<u8>)> {
        let sandbox = self.permissions();
        let proxy = json!({"proxy":{"enabled":true,"enableSocks5":false,"enableSocks5Udp":false,"allowUpstreamProxy":false,"dangerouslyAllowAllUnixSockets":false,"mode":"full","domains":self.domains,"unixSockets":{},"allowLocalBinding":true},"auditMetadata":{}});
        let rpc = self.rpc.as_mut().unwrap();
        let process = format!("sprowt-{}", PROCESS.fetch_add(1, Ordering::Relaxed));
        let start = rpc.call("process/start", json!({"processId":process,"argv":argv,"cwd":"file:///workspace","env":{"HOME":"/home/sprowt","PATH":GUEST_PATH,"LANG":"C.UTF-8","NO_PROXY":"localhost,127.0.0.1,::1"},"envPolicy":{"inherit":"none","ignoreDefaultExcludes":true,"exclude":[],"set":{},"includeOnly":[]},"tty":false,"arg0":null,"sandbox":sandbox,"enforceManagedNetwork":true,"networkProxy":proxy}))?;
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
                output
                    .extend_from_slice(&bytes[..bytes.len().min(cap.saturating_sub(output.len()))]);
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

    fn boundary(&mut self, cancelled: &AtomicBool) -> io::Result<()> {
        let code = "test \"$(uname -s)\" = Linux && test ! -e /root/.ssh && test ! -e /home/sprowt/.codex/auth.json && test ! -e /opt/codex-home/auth.json && test -z \"${OPENAI_API_KEY}${CODEX_API_KEY}${CODEX_ACCESS_TOKEN}\" && ! touch /usr/local/bin/sprowt-canary && (curl -sI --max-time 5 https://sprowt-policy-check.invalid | grep -q '403 Forbidden') && ! curl --noproxy '*' --resolve github.com:443:140.82.112.3 -fsI --max-time 5 https://github.com >/dev/null && ! curl -fsI --max-time 5 http://192.168.64.1:80 >/dev/null";
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

    pub fn export(&mut self, cancelled: &AtomicBool) -> io::Result<Snapshot> {
        let mut argv = vec![
            "exec",
            &self.name,
            "/usr/bin/env",
            "-i",
            "PATH=/usr/bin:/bin",
            "/usr/bin/tar",
            "--format=gnu",
            "-C",
            "/workspace",
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
        let permissions = self.permissions();
        let rpc = self.rpc.as_mut().unwrap();
        let handle = format!("export-{}", PROCESS.fetch_add(1, Ordering::Relaxed));
        rpc.call("fs/open", json!({"handleId":handle,"path":"file:///opt/sprowt-transfer/source.tar","sandbox":permissions}))?;
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
        self.control(
            &[
                "exec",
                &self.name,
                "/bin/rm",
                "-f",
                "/opt/sprowt-transfer/source.tar",
            ],
            &AtomicBool::new(false),
        )?;
        let bytes = result?;
        let snapshot = decode_source(&bytes)?;
        workspace::replace_source(&self.root, &snapshot)?;
        Ok(snapshot)
    }

    pub fn verify(
        &mut self,
        checks: &[Check],
        cancelled: &AtomicBool,
    ) -> io::Result<(Snapshot, Vec<CheckResult>)> {
        let before = self.export(cancelled)?;
        let mut results = Vec::new();
        for check in checks {
            if cancelled.load(Ordering::Relaxed) {
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
                check: check.check.clone(),
                command: check.command.clone(),
                exit_code,
                output,
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
    };
    if let Some(inspect) = vm.inspect()? {
        let saved = fs::read(root.join("vm.json"))
            .ok()
            .map(|bytes| serde_json::from_slice::<Value>(&bytes))
            .transpose()?;
        vm.validate(&inspect, saved.as_ref())?;
        vm.control(&["delete", "--force", &vm.name], &AtomicBool::new(false))?;
    }
    Ok(())
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

fn network(root: &Path) -> io::Result<BTreeMap<String, String>> {
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
    Ok(domains
        .into_iter()
        .map(|domain| (domain, "allow".into()))
        .collect())
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
            let checks = vec![Check { check: "runtime persisted".into(), command: vec!["/bin/sh".into(), "-c".into(), "test \"$(cat /home/sprowt/runtime-proof)\" = persistent && test \"$(cat result.txt)\" = edited".into()] }];
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
