use super::*;
use crate::codex::SharedVm;
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct CommandScope {
    pub cwd: String,
    pub home: String,
    pub domains: BTreeMap<String, String>,
}

pub(super) fn permissions(scope: &CommandScope, installing: bool) -> Value {
    let cwd = scope.cwd.clone();
    let git = format!("{cwd}/.git");
    let home = scope.home.clone();
    let entries = [("/","read"),(cwd.as_str(),"write"),(git.as_str(),"read"),(home.as_str(),"write"),("/tmp","write")].map(|(path,access)| json!({"path":{"type":"path","path":format!("file://{path}")},"access":access}));
    let filesystem = if installing {
        json!({"type":"unrestricted"})
    } else {
        json!({"type":"restricted","entries":entries})
    };
    json!({"permissions":{"type":"managed","file_system":filesystem,"network":"enabled"},
            "cwd":format!("file://{cwd}"),"workspaceRoots":[format!("file://{cwd}")],"windowsSandboxLevel":"disabled","useLegacyLandlock":false})
}

pub(super) fn run_process(
    rpc: &mut Rpc,
    scope: &CommandScope,
    argv: &[String],
    cancelled: &AtomicBool,
    timeout: u64,
    cap: usize,
    installing: bool,
) -> io::Result<(Option<i64>, Vec<u8>, Vec<u8>)> {
    let sandbox = permissions(scope, installing);
    let proxy = json!({"proxy":{"enabled":true,"enableSocks5":false,"enableSocks5Udp":false,"allowUpstreamProxy":false,"dangerouslyAllowAllUnixSockets":!installing,"mode":"full","domains":scope.domains,"unixSockets":{},"allowLocalBinding":true},"auditMetadata":{}});
    let task_folder = scope.cwd.clone();
    let home = scope.home.clone();
    let process = format!("sprowt-{}", PROCESS.fetch_add(1, Ordering::Relaxed));
    let mut environment = json!({"HOME":home,"XDG_CACHE_HOME":format!("{home}/.cache"),"PATH":GUEST_PATH,"LANG":"C.UTF-8","NO_PROXY":"localhost,127.0.0.1,::1"});
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
            let (kind, message) = if cancelled.load(Ordering::Relaxed) {
                (
                    io::ErrorKind::Interrupted,
                    "Guest command stopped.".to_owned(),
                )
            } else {
                (
                    io::ErrorKind::TimedOut,
                    format!("Guest command timed out after {timeout}s."),
                )
            };
            return Err(io::Error::new(
                kind,
                format!(
                    "{message}\n{}{}",
                    String::from_utf8_lossy(&stdout),
                    String::from_utf8_lossy(&stderr)
                ),
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
                output
                    .extend_from_slice(&bytes[..bytes.len().min(cap.saturating_sub(output.len()))]);
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

pub(super) fn read_guest(
    rpc: &mut Rpc,
    scope: &CommandScope,
    path: &str,
    cancelled: &AtomicBool,
) -> io::Result<Vec<u8>> {
    let permissions = permissions(scope, false);
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

pub(super) fn run_check_command(
    rpc: &mut Rpc,
    scope: &CommandScope,
    check: &Check,
    cancelled: &AtomicBool,
) -> CheckResult {
    let mut result = CheckResult {
        timeout_seconds: Some(check.timeout()),
        duration_ms: None,
        task: check.task,
        check: check.check.clone(),
        command: check.command.clone(),
        exit_code: None,
        output: String::new(),
        missing_runtime: None,
    };
    let Some(executable) = check.command.first() else {
        result.output = "Verification requires an executable.".into();
        return result;
    };
    let probe = vec!["/usr/bin/test".into(), "-x".into(), executable.clone()];
    match run_process(rpc, scope, &probe, cancelled, 30, 8192, false) {
        Ok((Some(0), _, _)) => {}
        Ok((Some(1), _, _)) => {
            result.missing_runtime = Some(executable.clone());
            result.output = format!(
                "Check executable is unavailable: {executable}. Its task environment needs rebuilding."
            );
            return result;
        }
        other => {
            result.output = match other {
                Err(error) => error.to_string(),
                Ok(_) => "Executable check was interrupted.".into(),
            };
            return result;
        }
    }
    let started = Instant::now();
    match run_process(
        rpc,
        scope,
        &check.command,
        cancelled,
        check.timeout(),
        8192,
        false,
    ) {
        Ok((exit, stdout, stderr)) => {
            result.exit_code = exit;
            result.output = format!(
                "{}{}",
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            );
        }
        Err(error) => result.output = error.to_string(),
    }
    result.duration_ms = Some(started.elapsed().as_millis() as u64);
    result
}

pub(crate) struct TaskSession {
    controller: SharedVm,
    pub(crate) root: PathBuf,
    pub(crate) id: i64,
    pub(crate) owner: i64,
    pub(crate) source: String,
    name: String,
    scope: CommandScope,
    identity: Identity,
    child: Child,
    rpc: Rpc,
    token: u64,
    basis: Option<Snapshot>,
    gate: Arc<environment_gate::Gate>,
    authorization: Option<(PathBuf, i64)>,
}

impl TaskSession {
    pub(crate) fn capture(
        controller: &SharedVm,
        id: i64,
        owner: i64,
        source: &str,
        cancelled: &AtomicBool,
    ) -> io::Result<Self> {
        let (name, root, scope, identity, gate, mut command) = {
            let vm = crate::codex::shared_vm::cancellable_lock(controller, cancelled)?;
            vm.require_bound()?;
            if vm.tasks.owners.get(&id) != Some(&owner)
                || crate::task_worktree::task_id(source)? != id
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Session must belong to the current task owner and attempt.",
                ));
            }
            (
                vm.name.clone(),
                vm.root.clone(),
                CommandScope {
                    cwd: crate::task_worktree::folder(id),
                    home: crate::task_worktree::worker_home(owner),
                    domains: vm.domains.clone(),
                },
                vm.identity.clone(),
                vm.environment.clone(),
                vm.executor(),
            )
        };
        let (mut child, mut rpc) = Rpc::start(&mut command)?;
        if let Err(error) = initialize(&name, &mut rpc) {
            rpc::terminate(&mut child);
            return Err(error);
        }
        Ok(Self {
            controller: controller.clone(),
            root,
            id,
            owner,
            source: source.into(),
            name,
            scope,
            identity,
            child,
            rpc,
            token: PROCESS.fetch_add(1, Ordering::Relaxed),
            basis: None,
            gate,
            authorization: None,
        })
    }
    fn authorize_locked(&self, vm: &Sandbox) -> io::Result<()> {
        vm.require_bound()?;
        if vm.root != self.root
            || vm.identity != self.identity
            || vm.tasks.owners.get(&self.id) != Some(&self.owner)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Task session ownership or VM identity changed.",
            ));
        }
        if let Some((database, mod_id)) = &self.authorization {
            let store = crate::store::Store::open(database)?;
            let (id, _) =
                crate::checks::authorize(&store, &self.root, *mod_id, self.owner, &self.source)?;
            if id != self.id {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Task attempt changed.",
                ));
            }
        }
        Ok(())
    }
    pub(crate) fn authorize(&self, cancelled: &AtomicBool) -> io::Result<()> {
        let vm = lock(&self.controller, cancelled)?;
        self.authorize_locked(&vm)
    }
    pub(crate) fn accept<T>(
        &self,
        cancelled: &AtomicBool,
        accept: impl FnOnce(&mut Sandbox) -> io::Result<T>,
    ) -> io::Result<T> {
        let mut vm = lock(&self.controller, cancelled)?;
        self.authorize_locked(&vm)?;
        accept(&mut vm)
    }
    pub(crate) fn cwd(&self) -> &str {
        &self.scope.cwd
    }
    pub(crate) fn home(&self) -> &str {
        &self.scope.home
    }
    pub(crate) fn identity(&self) -> &Identity {
        &self.identity
    }
    pub(crate) fn read_lease(&self, cancelled: &AtomicBool) -> io::Result<environment_gate::Lease> {
        self.gate.read(cancelled)
    }
    pub(crate) fn command(&mut self, check: &Check, cancelled: &AtomicBool) -> CheckResult {
        run_check_command(&mut self.rpc, &self.scope, check, cancelled)
    }
    pub(crate) fn snapshot(&mut self, cancelled: &AtomicBool) -> io::Result<Snapshot> {
        let prefix = format!("/opt/sprowt-transfer/task-{}-{}", self.id, self.token);
        let snapshot = transfer::snapshot(
            &self.name,
            &self.root,
            &self.scope,
            &mut self.rpc,
            &prefix,
            self.basis.as_ref(),
            cancelled,
        )?;
        self.basis = Some(snapshot.clone());
        Ok(snapshot)
    }
    pub(crate) fn restore_scripts(&self, cancelled: &AtomicBool) -> io::Result<()> {
        let vm = crate::codex::shared_vm::cancellable_lock(&self.controller, cancelled)?;
        self.authorize_locked(&vm)?;
        crate::checks::restore_scripts(&vm, self.id, self.token, cancelled)
    }
    pub(crate) fn preserve(&self, snapshot: &Snapshot, cancelled: &AtomicBool) -> io::Result<()> {
        self.accept(cancelled, |vm| {
            vm.tasks.active = Some(self.id);
            vm.commit_source(Some(self.id), snapshot, &AtomicBool::new(false))?;
            vm.save_draft(snapshot)?;
            vm.checkpoint_tasks(&AtomicBool::new(false))?;
            let combined = vm.review_source(&AtomicBool::new(false))?;
            workspace::replace_source(&self.root, &combined)
        })
    }
    pub(crate) fn packages(&self, request: &Request, cancelled: &AtomicBool) -> io::Result<String> {
        let _lease = self.gate.write(cancelled)?;
        self.accept(cancelled, |vm| {
            vm.install_packages_locked(request, cancelled)
        })
    }
    pub(crate) fn cache(
        &self,
        action: &str,
        key: &str,
        dependencies: bool,
        cancelled: &AtomicBool,
    ) -> io::Result<crate::environment::cache::Use> {
        let _lease = self.gate.read(cancelled)?;
        self.accept(cancelled, |vm| {
            crate::environment::cache::operation_at(
                vm,
                action,
                key,
                dependencies,
                &self.scope.home,
                &self.scope.cwd,
                &format!("task-{}-{}", self.id, self.token),
                cancelled,
            )
        })
    }
    pub(crate) fn muse_configuration(&self, cancelled: &AtomicBool) -> io::Result<Value> {
        self.accept(cancelled, |vm| {
            Ok(vm.muse_configuration_at(&self.scope.cwd, &self.scope.home))
        })
    }
}
impl Drop for TaskSession {
    fn drop(&mut self) {
        rpc::terminate(&mut self.child);
    }
}

pub(super) fn initialize(name: &str, rpc: &mut Rpc) -> io::Result<()> {
    let initialized = rpc.call("initialize", json!({"clientName":"sprowt_verification"}))?;
    let info = &initialized["environmentInfo"];
    let version = container()
        .args(["exec", name, "/usr/local/bin/codex", "--version"])
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
    rpc.write(json!({"method":"initialized"}))
}

pub(crate) fn lock<'a>(
    vm: &'a SharedVm,
    cancelled: &AtomicBool,
) -> io::Result<std::sync::MutexGuard<'a, Sandbox>> {
    crate::codex::shared_vm::cancellable_lock(vm, cancelled)
}

pub(crate) fn install_packages(
    vm: &SharedVm,
    context: &crate::tools::Context,
    request: &Request,
    cancelled: &AtomicBool,
) -> io::Result<String> {
    let (database, mod_id) = context.execution_identity()?;
    let root = context
        .workspace()
        .ok_or_else(|| io::Error::other("Package installation needs its codemod root."))?;
    let store = crate::store::Store::open(database)?;
    let source = store
        .execution(mod_id)
        .map_err(io::Error::other)?
        .and_then(|execution| {
            execution
                .tasks
                .into_iter()
                .find(|task| {
                    task.worker == Some(context.worker_id())
                        && matches!(task.status.as_str(), "sending" | "running")
                })
                .map(|task| task.source)
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Package installation needs the worker's current task attempt.",
            )
        })?;
    let gate = {
        let vm = lock(vm, cancelled)?;
        if vm.root.canonicalize()? != root.canonicalize()? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Tool cannot access another mod's VM.",
            ));
        }
        vm.environment.clone()
    };
    let _lease = gate.write(cancelled)?;
    let mut controller = lock(vm, cancelled)?;
    controller.require_bound()?;
    let (id, _) = crate::checks::authorize(&store, root, mod_id, context.worker_id(), &source)?;
    if controller.root.canonicalize()? != root.canonicalize()? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Tool cannot access another mod's VM.",
        ));
    }
    if controller.tasks.owners.get(&id) != Some(&context.worker_id()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Package installation belongs to another task owner.",
        ));
    }
    controller.install_packages_locked(request, cancelled)
}

pub(crate) fn prepare_task(
    vm: &SharedVm,
    context: &crate::tools::Context,
    source: &str,
    repair: bool,
    cancelled: &AtomicBool,
) -> io::Result<TaskSession> {
    let (database, mod_id) = context.execution_identity()?;
    let root = context
        .workspace()
        .ok_or_else(|| io::Error::other("Task preparation needs its codemod root."))?;
    let store = crate::store::Store::open(database)?;
    let (id, _) = crate::checks::authorize(&store, root, mod_id, context.worker_id(), source)?;
    {
        let gate = lock(vm, cancelled)?.environment.clone();
        let _lease = if repair {
            Some(gate.write(cancelled)?)
        } else {
            None
        };
        let mut controller = lock(vm, cancelled)?;
        crate::checks::authorize(&store, root, mod_id, context.worker_id(), source)?;
        if controller.root.canonicalize()? != root.canonicalize()? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Task preparation belongs to another VM.",
            ));
        }
        if context.muse {
            controller.assign_muse_task(id, context.worker_id(), cancelled)?;
        } else {
            controller.assign_task(id, context.worker_id(), cancelled)?;
        }
        if repair {
            controller.refresh_for_repair(id, source, cancelled)?;
        }
    }
    let mut session = TaskSession::capture(vm, id, context.worker_id(), source, cancelled)?;
    session.authorization = Some((database.to_owned(), mod_id));
    session.authorize(cancelled)?;
    Ok(session)
}

pub(crate) fn checked_session(
    vm: &SharedVm,
    store: &crate::store::Store,
    root: &Path,
    mod_id: i64,
    owner: i64,
    source: &str,
    cancelled: &AtomicBool,
) -> io::Result<TaskSession> {
    let (id, _) = crate::checks::authorize(store, root, mod_id, owner, source)?;
    let mut session = TaskSession::capture(vm, id, owner, source, cancelled)?;
    session.authorization = Some((
        PathBuf::from(
            store
                .0
                .path()
                .ok_or_else(|| io::Error::other("Task checks need a persistent store."))?,
        ),
        mod_id,
    ));
    session.authorize(cancelled)?;
    Ok(session)
}

pub(crate) fn verify_execution(
    vm: &SharedVm,
    source: &str,
    checks: &[Check],
    cancelled: &AtomicBool,
    progress: impl FnMut(&str),
) -> io::Result<(Snapshot, Vec<CheckResult>)> {
    let gate = lock(vm, cancelled)?.environment.clone();
    let lease = gate.read(cancelled)?;
    lock(vm, cancelled)?.with_environment_read(&lease, |vm| {
        vm.verify_execution_with_progress(source, checks, cancelled, progress)
    })
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
