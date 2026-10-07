use std::{
    collections::BTreeMap,
    env, fs, io,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use serde_json::{Value, json};

use crate::{
    execution::{Check, CheckResult},
    plan::{self, Plan, Role},
    router::Selection,
    rpc::{self, Rpc},
    sandbox::Sandbox,
    tools::{self, Context, Dispatcher, Output},
    workspace::Snapshot,
};

pub enum Action {
    Run {
        source: String,
        text: String,
        routing: Option<Value>,
    },
    Verify {
        source: String,
        checks: Vec<Check>,
    },
    Steer {
        source: String,
        texts: Vec<String>,
        turn: String,
    },
    Stop {
        turn: String,
    },
    Shutdown(Sender<()>),
}

pub enum Event {
    MailboxChanged,
    Preparing(String),
    Configured(Selection),
    TaskConfigured {
        source: String,
        selection: Selection,
    },
    Ready {
        thread: Value,
        model: Option<String>,
        effort: Option<String>,
    },
    Accepted {
        source: String,
        turn: String,
    },
    Notification(Value),
    Rejected {
        source: String,
        message: String,
    },
    Failed(String),
    Checked {
        source: String,
        checks: Vec<CheckResult>,
        before: Snapshot,
    },
}

pub(crate) type SharedVm = Arc<Mutex<Sandbox>>;
static VMS: Mutex<BTreeMap<PathBuf, Weak<Mutex<Sandbox>>>> = Mutex::new(BTreeMap::new());

fn shared_vm(
    root: &Path,
    tasks: &[i64],
    cancelled: &AtomicBool,
    progress: impl Fn(&str),
) -> io::Result<SharedVm> {
    let mut registry = VMS.lock().unwrap();
    registry.retain(|_, vm| vm.strong_count() > 0);
    if let Some(vm) = registry.get(root).and_then(Weak::upgrade) {
        vm.lock().unwrap().refresh_network()?;
        return Ok(vm);
    }
    let mut vm = Sandbox::prepare(root, cancelled, progress)?;
    vm.prepare_tasks(tasks, cancelled)?;
    let vm = Arc::new(Mutex::new(vm));
    registry.insert(root.to_owned(), Arc::downgrade(&vm));
    Ok(vm)
}

pub struct Resume {
    pub id: String,
    pub restart_if_missing: bool,
    pub accepted_instructions: Vec<String>,
}

struct Setup {
    role: Role,
    instructions: String,
    selection: Option<Selection>,
    permissions: &'static str,
    cancelled: Arc<AtomicBool>,
    vm: Option<Arc<Mutex<Sandbox>>>,
    context: Context,
}

pub struct Client {
    actions: Sender<Action>,
    events: Receiver<Event>,
    child: Arc<Mutex<Option<Child>>>,
    task: Option<JoinHandle<()>>,
    closed: bool,
    vm: Arc<Mutex<Option<SharedVm>>>,
    active_task: Arc<AtomicI64>,
    cancelled: Arc<AtomicBool>,
}

impl Client {
    pub fn start(
        project: &Path,
        resume: Option<Resume>,
        role: Role,
        description: &str,
        plan: Option<&Plan>,
        selection: Option<Selection>,
        context: Context,
    ) -> io::Result<Self> {
        #[cfg(not(unix))]
        return Err(io::Error::other(
            "The first Codex worker currently supports macOS and Linux.",
        ));
        let workspace = context.workspace().map(|root| root.join("work"));
        let cwd = workspace.as_deref().unwrap_or(project).to_owned();
        let project = project.to_owned();
        let (actions, inbox) = mpsc::channel();
        let (outgoing, events) = mpsc::channel();
        let mut instructions = plan::instructions(role, description, plan, workspace.is_some());
        if role == Role::Planner && context.muse {
            instructions = instructions.replace("Only Codex is connected; assign every task to codex.", "Codex and Muse are connected. Assign demanding or high-risk implementation and integration checks to codex; use muse for well-scoped independent implementation, tests or documentation. Use both when that is useful, without forcing a split.");
        }
        if role == Role::Planner {
            instructions.push_str(&format!(
                "\nProject brief (verify against source): {}",
                crate::router::context(&cwd, description)
            ));
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));
        let process = child.clone();
        let stopped = cancelled.clone();
        let controller = Arc::new(Mutex::new(None));
        let shared = controller.clone();
        let active_task = Arc::new(AtomicI64::new(0));
        let last_task = active_task.clone();
        let task = thread::spawn(move || {
            let result = (|| {
                let vm = workspace
                    .as_ref()
                    .map(|path| {
                        shared_vm(path.parent().unwrap(), &context.tasks, &stopped, |label| {
                            let _ = outgoing.send(Event::Preparing(label.into()));
                        })
                    })
                    .transpose()?;
                *shared.lock().unwrap() = vm.clone();
                if stopped.load(Ordering::Relaxed) {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "Worker startup stopped. Ctrl+R retries.",
                    ));
                }
                if context.provider == "muse" {
                    return crate::muse::serve(
                        vm.ok_or_else(|| io::Error::other("Muse requires a VM."))?,
                        resume,
                        (instructions, context),
                        stopped,
                        process.clone(),
                        inbox,
                        &outgoing,
                    );
                }
                let permissions = if vm.is_some() {
                    "sprowt_vm"
                } else {
                    "sprowt_readonly"
                };
                let host_home = vm
                    .as_ref()
                    .map(|vm| vm.lock().unwrap().worker_home(context.worker_id()))
                    .transpose()?;
                let mut command = Command::new("codex");
                command
                    .args(["app-server", "--listen", "stdio://"])
                    .current_dir(host_home.as_deref().unwrap_or(&cwd));
                command.env_clear().envs(
                    [
                        "PATH",
                        "HOME",
                        "USER",
                        "LOGNAME",
                        "LANG",
                        "TMPDIR",
                        "CODEX_HOME",
                    ]
                    .iter()
                    .filter_map(|name| env::var_os(name).map(|value| (*name, value))),
                );
                for option in configuration(&project)? {
                    command.args(["-c", &option]);
                }
                if let Some(vm) = &vm {
                    let vm = vm.lock().unwrap();
                    command.env("CODEX_HOME", host_home.as_ref().unwrap());
                    for option in vm
                        .configuration()
                        .into_iter()
                        .chain(vm.worker_configuration(context.worker_id()))
                    {
                        command.args(["-c", &option]);
                    }
                    command.args(["-c", "cli_auth_credentials_store=\"file\""]);
                }
                let (server, mut rpc) = Rpc::start(&mut command)?;
                *process.lock().unwrap() = Some(server);
                let setup = Setup {
                    role,
                    instructions,
                    selection,
                    permissions,
                    cancelled: stopped,
                    vm,
                    context,
                };
                serve(&mut rpc, &cwd, resume, setup, inbox, &outgoing)
            })();
            if let Some(child) = process.lock().unwrap().as_mut() {
                rpc::terminate(child);
            }
            let checkpoint = (|| -> io::Result<()> {
                if let Some(vm) = shared.lock().unwrap().as_ref() {
                    let mut vm = vm.lock().unwrap();
                    let id = last_task.load(Ordering::Relaxed);
                    if id > 0
                        && let Err(error) = vm.checkpoint_task(id)
                    {
                        fs::write(vm.root().join("checkpoint-error"), error.to_string())?;
                        return Err(error);
                    }
                }
                Ok(())
            })();
            if let Err(error) = result.and(checkpoint) {
                let _ = outgoing.send(Event::Failed(error.to_string()));
            }
        });
        Ok(Self {
            actions,
            events,
            child,
            task: Some(task),
            closed: false,
            vm: controller,
            active_task,
            cancelled,
        })
    }

    pub fn send(&self, action: Action) -> io::Result<()> {
        if let Action::Run { source, .. } | Action::Verify { source, .. } = &action
            && let Ok(id) = crate::task_worktree::task_id(source)
        {
            self.active_task.store(id, Ordering::Relaxed);
        }
        if matches!(
            action,
            Action::Run { .. } | Action::Verify { .. } | Action::Stop { .. }
        ) {
            self.cancelled
                .store(matches!(action, Action::Stop { .. }), Ordering::Relaxed);
        }
        self.actions.send(action).map_err(io::Error::other)
    }

    pub fn cancel_checks(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn poll(&self) -> impl Iterator<Item = Event> + '_ {
        self.events.try_iter()
    }
}

impl Client {
    pub fn shutdown(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.cancelled.store(true, Ordering::Relaxed);
        let (finished, receiver) = mpsc::channel();
        if self.actions.send(Action::Shutdown(finished)).is_ok() {
            let _ = receiver.recv_timeout(Duration::from_secs(60));
        }
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            rpc::terminate(child);
        }
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
        self.vm.lock().unwrap().take();
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn configuration(project: &Path) -> io::Result<Vec<String>> {
    let home =
        env::var_os("HOME").ok_or_else(|| io::Error::other("Cannot locate the host login."))?;
    let codex_home = env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| Path::new(&home).join(".codex"));
    let mut filesystem = BTreeMap::from([
        (":minimal".to_owned(), "read"),
        (project.to_string_lossy().into_owned(), "read"),
        (codex_home.to_string_lossy().into_owned(), "deny"),
        (
            Path::new(&home)
                .join("Library/Keychains")
                .to_string_lossy()
                .into_owned(),
            "deny",
        ),
    ]);
    filesystem.insert(
        crate::router::directory()?
            .join("router.env")
            .to_string_lossy()
            .into_owned(),
        "deny",
    );
    filesystem.insert(
        project.join(".env.local").to_string_lossy().into_owned(),
        "deny",
    );
    let binary = env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("codex"))
        .find(|path| path.is_file())
        .ok_or_else(|| io::Error::other("Install Codex CLI, then run again."))?;
    filesystem.insert(
        binary.parent().unwrap().to_string_lossy().into_owned(),
        "read",
    );
    filesystem.insert(
        binary
            .canonicalize()?
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        "read",
    );
    let rules = filesystem
        .iter()
        .map(|(path, access)| format!("{}={}", json!(path), json!(access)))
        .collect::<Vec<_>>()
        .join(",");
    let permissions = "sprowt_readonly";
    let mut config = vec![
        format!("permissions.{permissions}={{filesystem={{{rules}}},network={{enabled=false}}}}"),
        format!("default_permissions={}", json!(permissions)),
        "approval_policy=\"never\"".into(),
        "forced_login_method=\"chatgpt\"".into(),
        "shell_environment_policy.inherit=\"none\"".into(),
        "allow_login_shell=false".into(),
        "web_search=\"disabled\"".into(),
    ];
    for feature in [
        "apps",
        "plugins",
        "hooks",
        "multi_agent",
        "browser_use",
        "computer_use",
        "image_generation",
        "shell_snapshot",
    ] {
        config.push(format!("features.{feature}=false"));
    }
    Ok(config)
}

fn flush(
    rpc: &mut Rpc,
    outgoing: &Sender<Event>,
    vm: &mut Option<SharedVm>,
    thread: &str,
    cancelled: &AtomicBool,
    context: &Context,
    task: Option<i64>,
) -> io::Result<()> {
    for message in std::mem::take(&mut rpc.buffered) {
        if message["method"] == "item/tool/call" && message.get("id").is_some() {
            let params = &message["params"];
            let result = (|| {
                if params["threadId"] != thread || !params["namespace"].is_null() {
                    return Err(io::Error::other("This client tool is not available."));
                }
                let mut guard = vm
                    .as_ref()
                    .filter(|_| params["tool"] == crate::packages::TOOL)
                    .map(|vm| vm.lock().unwrap());
                if let Some(vm) = &mut guard {
                    vm.tasks.active = task;
                }
                Dispatcher::new(context, guard.as_deref_mut(), cancelled)
                    .worker_call(
                        params["tool"].as_str().unwrap_or(""),
                        params["arguments"].clone(),
                        |label| {
                            let _ = outgoing.send(Event::Preparing(label.into()));
                        },
                    )
                    .map(Output::message)
            })();
            let changed = result.is_ok()
                && [
                    crate::mailbox::SEND,
                    crate::mailbox::READ,
                    crate::mailbox::ACK,
                ]
                .contains(&params["tool"].as_str().unwrap_or(""));
            rpc.write(json!({"id":message["id"],"result":{"success":result.is_ok(),"contentItems":[{"type":"inputText","text":result.unwrap_or_else(|error|error.to_string())}]}}))?;
            if changed {
                let _ = outgoing.send(Event::MailboxChanged);
            }
            continue;
        }
        if message["method"] == "turn/completed"
            && let Some(vm) = vm
        {
            stop_terminals(rpc, thread)?;
            let mut vm = vm.lock().unwrap();
            vm.tasks.active = task;
            vm.export(&AtomicBool::new(false))?;
        }
        let _ = outgoing.send(Event::Notification(message));
    }
    Ok(())
}

fn stop_terminals(rpc: &mut Rpc, thread: &str) -> io::Result<()> {
    let result = rpc.call(
        "thread/backgroundTerminals/list",
        json!({"threadId":thread}),
    )?;
    let terminals = result["data"]
        .as_array()
        .ok_or_else(|| io::Error::other("Codex returned no command inventory."))?;
    for terminal in terminals {
        rpc.call(
            "thread/backgroundTerminals/terminate",
            json!({"threadId":thread,"processId":terminal["processId"]}),
        )?;
    }
    Ok(())
}

fn serve(
    rpc: &mut Rpc,
    project: &Path,
    resume: Option<Resume>,
    setup: Setup,
    actions: Receiver<Action>,
    outgoing: &Sender<Event>,
) -> io::Result<()> {
    let Setup {
        role,
        instructions,
        selection,
        permissions,
        cancelled,
        mut vm,
        context,
    } = setup;
    rpc.call("initialize", json!({"clientInfo":{"name":"sprowt_harness","title":"Sprowt Harness","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}))?;
    rpc.write(json!({"method":"initialized"}))?;
    let account = rpc.call("account/read", json!({"refreshToken":false}))?;
    if account["account"]["type"] != "chatgpt" {
        return Err(io::Error::other(
            "Sign in with ChatGPT using `codex login`, then run again.",
        ));
    }
    if vm.is_some() {
        if rpc
            .call("environment/info", json!({"environmentId":"local"}))
            .is_ok()
        {
            return Err(io::Error::other(
                "Host execution must be unavailable to the VM worker.",
            ));
        }
        let info = rpc.call("environment/info", json!({"environmentId":"vm"}))?;
        if info["cwd"] != "file:///workspace" {
            return Err(io::Error::other("Codex did not connect to the mod VM."));
        }
    } else {
        check_boundary(rpc, project, permissions)?;
    }
    let config = rpc.call("config/read", json!({"cwd":project,"includeLayers":false}))?;
    let mut overrides = serde_json::Map::new();
    if let Some(servers) = config["config"]["mcp_servers"].as_object() {
        for name in servers.keys() {
            overrides.insert(format!("mcp_servers.{name}.enabled"), json!(false));
        }
    }
    if let Some(variables) = config["config"]["shell_environment_policy"]["set"].as_object() {
        for name in variables.keys() {
            overrides.insert(format!("shell_environment_policy.set.{name}"), json!(""));
        }
    }
    if vm.is_some() {
        overrides.insert(
            "shell_environment_policy.set.PATH".into(),
            json!("/usr/local/bin:/usr/bin:/bin"),
        );
        overrides.insert(
            "shell_environment_policy.set.HOME".into(),
            json!(crate::task_worktree::worker_home(context.worker_id())),
        );
    }
    let catalog = model_catalog(rpc)?;
    let selection = selection
        .map(|selection| resolve_model(&catalog, selection))
        .transpose()?;
    if let Some(selection) = &selection {
        overrides.insert("model_reasoning_effort".into(), json!(selection.effort));
        let _ = outgoing.send(Event::Configured(selection.clone()));
    }
    let mut params = json!({"cwd":project,"permissions":permissions,"approvalPolicy":"never","config":overrides,"developerInstructions":instructions});
    if vm.is_some() {
        params["environments"] = json!([{"environmentId":"vm","cwd":"/workspace"}]);
        params["dynamicTools"] = json!(tools::advertised(&context));
        rpc.client_tools = true;
    }
    if let Some(selection) = &selection {
        params["model"] = json!(selection.model);
    }
    let mut fresh = params.clone();
    if let Some(resume) = &resume
        && !resume.accepted_instructions.is_empty()
    {
        fresh["developerInstructions"] = json!(format!(
            "{instructions}\nPreviously accepted user instructions, in order (later corrections take precedence): {}",
            json!(resume.accepted_instructions)
        ));
    }
    let marker = vm
        .as_ref()
        .map(|vm| vm.lock().unwrap().worker_home(context.worker_id()))
        .transpose()?
        .map(|home| home.join("worker-network-v1-thread"));
    let resume = resume.filter(|resume| {
        marker
            .as_ref()
            .is_none_or(|path| fs::read_to_string(path).ok().as_deref() == Some(resume.id.as_str()))
    });
    let result = if let Some(resume) = resume {
        let mut resume_params = params.clone();
        resume_params["threadId"] = json!(resume.id);
        resume_params
            .as_object_mut()
            .unwrap()
            .remove("dynamicTools");
        match rpc.call("thread/resume", resume_params) {
            Err(error)
                if resume.restart_if_missing
                    && error.kind() == io::ErrorKind::InvalidInput
                    && error
                        .to_string()
                        .starts_with("no rollout found for thread id") =>
            {
                rpc.call("thread/start", fresh)?
            }
            result => result?,
        }
    } else {
        rpc.call("thread/start", fresh)?
    };
    let thread = result["thread"]["id"]
        .as_str()
        .ok_or_else(|| io::Error::other("Codex returned no conversation ID."))?
        .to_owned();
    if let Some(marker) = marker {
        fs::write(marker, &thread)?;
    }
    let inventory = rpc.call("mcpServerStatus/list", json!({"threadId":thread}))?;
    if inventory["data"].as_array().is_none_or(|servers| {
        servers.iter().any(|server| {
            server["runtimeStatus"] != "disabled"
                || server["tools"]
                    .as_object()
                    .is_none_or(|tools| !tools.is_empty())
        })
    }) {
        return Err(io::Error::other(
            "External MCP tools are enabled; this worker cannot start.",
        ));
    }
    let effort = if let Some(selection) = &selection {
        Some(selection.effort.clone())
    } else if let Some(effort) = result["reasoningEffort"].as_str() {
        Some(effort.to_owned())
    } else {
        catalog
            .iter()
            .find(|model| model["model"] == result["model"])
            .and_then(|model| model["defaultReasoningEffort"].as_str())
            .map(str::to_owned)
    };
    outgoing
        .send(Event::Ready {
            thread: result["thread"].clone(),
            model: result["model"].as_str().map(str::to_owned),
            effort: effort.clone(),
        })
        .map_err(io::Error::other)?;
    let router = (role == Role::Executor).then(crate::router::Router::start);
    let mut task = None;
    flush(rpc, outgoing, &mut vm, &thread, &cancelled, &context, task)?;
    let mut last_turn = None;
    loop {
        while let Ok(action) = actions.try_recv() {
            if let Action::Verify { source, checks } = &action {
                task = crate::task_worktree::task_id(source).ok();
                let (before, results) = vm
                    .as_mut()
                    .ok_or_else(|| io::Error::other("Verification requires the mod VM."))?
                    .lock()
                    .unwrap()
                    .verify_execution(source, checks, &cancelled)?;
                if let Some(vm) = &vm {
                    let vm = vm.lock().unwrap();
                    if source.starts_with("final:")
                        || task.is_some_and(|id| vm.tasks.integrated.contains(&id))
                    {
                        task = None;
                    }
                }
                let _ = outgoing.send(Event::Checked {
                    source: source.clone(),
                    checks: results,
                    before,
                });
                flush(rpc, outgoing, &mut vm, &thread, &cancelled, &context, task)?;
                continue;
            }
            let (method, source, params) = match action {
                Action::Run {
                    source,
                    text,
                    routing,
                } => {
                    let mut params = json!({"threadId":thread,"clientUserMessageId":source,"input":[{"type":"text","text":text}],"permissions":permissions,"approvalPolicy":"never"});
                    if let Some(effort) = &effort {
                        params["effort"] = json!(effort);
                    }
                    if role == Role::Planner {
                        params["outputSchema"] = plan::schema();
                        if context.muse {
                            params["outputSchema"]["properties"]["tasks"]["items"]["properties"]
                                ["worker"]["enum"] = json!(["codex", "muse"]);
                        }
                        if let Some(selection) = &selection {
                            params["model"] = json!(selection.model);
                            params["effort"] = json!(selection.effort);
                        }
                    }
                    if source.starts_with("00000004-") {
                        let report_schema = crate::execution::task_schema(routing.as_ref());
                        let repair = routing
                            .as_ref()
                            .is_some_and(|state| !state["repair"].is_null());
                        let _ = outgoing.send(Event::Preparing("choosing task model".into()));
                        let chosen = match (&router, routing) {
                            (Some(Ok(router)), Some(state)) => router.route(state),
                            (Some(Err(error)), _) => Selection::fallback(&error.to_string()),
                            _ => Selection::fallback("Task routing context unavailable"),
                        };
                        let chosen = resolve_model(&catalog, chosen)?;
                        if cancelled.load(Ordering::Relaxed) {
                            return Err(io::Error::new(
                                io::ErrorKind::Interrupted,
                                "Task routing stopped.",
                            ));
                        }
                        params["model"] = json!(chosen.model);
                        params["effort"] = json!(chosen.effort);
                        let _ = outgoing.send(Event::TaskConfigured {
                            source: source.clone(),
                            selection: chosen,
                        });
                        let vm = vm
                            .as_mut()
                            .ok_or_else(|| io::Error::other("Task execution requires a VM."))?;
                        let id = crate::task_worktree::task_id(&source)?;
                        let _ = outgoing.send(Event::Preparing("preparing task worktree".into()));
                        stop_terminals(rpc, &thread)?;
                        let mut vm = vm.lock().unwrap();
                        vm.assign_task(id, context.worker_id(), &cancelled)?;
                        if repair {
                            vm.refresh_for_repair(id, &source, &cancelled)?;
                        }
                        task = Some(id);
                        params["permissions"] = json!(format!("sprowt_task_{id}"));
                        params["environments"] =
                            json!([{"environmentId":"vm","cwd":vm.task_folder()}]);
                        params["outputSchema"] = report_schema;
                    } else if vm.is_some() {
                        return Err(io::Error::other(
                            "Execution requires an assigned task worktree.",
                        ));
                    }
                    ("turn/start", source, params)
                }
                Action::Steer {
                    source,
                    texts,
                    turn,
                } => (
                    "turn/steer",
                    source.clone(),
                    json!({"threadId":thread,"clientUserMessageId":source,"expectedTurnId":turn,"input":texts.into_iter().map(|text|json!({"type":"text","text":text})).collect::<Vec<_>>()}),
                ),
                Action::Stop { turn } => (
                    "turn/interrupt",
                    String::new(),
                    json!({"threadId":thread,"turnId":turn}),
                ),
                Action::Shutdown(finished) => {
                    // Let Codex reap command processes before terminating the server.
                    if let Some(turn) = &last_turn {
                        let _ =
                            rpc.call("turn/interrupt", json!({"threadId":thread,"turnId":turn}));
                    }
                    stop_terminals(rpc, &thread)?;
                    if let Some(vm) = &mut vm {
                        let mut vm = vm.lock().unwrap();
                        vm.tasks.active = task;
                        vm.export(&AtomicBool::new(false))?;
                    }
                    let _ = finished.send(());
                    return Ok(());
                }
                Action::Verify { .. } => unreachable!(),
            };
            match rpc.call(method, params) {
                Ok(result) if method != "turn/interrupt" => {
                    let turn = result["turn"]["id"]
                        .as_str()
                        .or_else(|| result["turnId"].as_str())
                        .ok_or_else(|| io::Error::other("Codex returned no turn ID."))?;
                    last_turn = Some(turn.to_owned());
                    let _ = outgoing.send(Event::Accepted {
                        source,
                        turn: turn.to_owned(),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                    let _ = outgoing.send(Event::Rejected {
                        source,
                        message: error.to_string(),
                    });
                }
                Err(error) => return Err(error),
                _ => {}
            }
            flush(rpc, outgoing, &mut vm, &thread, &cancelled, &context, task)?;
        }
        match rpc.receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(message) => {
                rpc.receive(message?)?;
                flush(rpc, outgoing, &mut vm, &thread, &cancelled, &context, task)?;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => return Err(io::Error::other(error)),
        }
    }
}

fn model_catalog(rpc: &mut Rpc) -> io::Result<Vec<Value>> {
    let mut models = Vec::new();
    let mut cursor = Value::Null;
    loop {
        let result = rpc.call(
            "model/list",
            json!({"limit":100,"includeHidden":false,"cursor":cursor}),
        )?;
        models.extend(
            result["data"]
                .as_array()
                .ok_or_else(|| io::Error::other("Codex returned no model catalog."))?
                .iter()
                .cloned(),
        );
        cursor = result["nextCursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    Ok(models)
}

fn resolve_model(models: &[Value], mut selection: Selection) -> io::Result<Selection> {
    let available = |model: &Value| model["model"] == selection.model;
    let model = if let Some(model) = models.iter().find(|model| available(model)) {
        model
    } else {
        let model = models
            .iter()
            .find(|model| model["model"] == "gpt-6.1-sol")
            .or_else(|| models.iter().find(|model| model["isDefault"] == true))
            .ok_or_else(|| io::Error::other("No supported model is listed by Codex."))?;
        selection
            .reason
            .push_str(" · selected model unavailable; catalog fallback");
        selection.model = model["model"]
            .as_str()
            .ok_or_else(|| io::Error::other("Invalid model catalog."))?
            .into();
        selection.effort = "xhigh".into();
        model
    };
    if !model["supportedReasoningEfforts"]
        .as_array()
        .is_some_and(|levels| {
            levels
                .iter()
                .any(|level| level["reasoningEffort"] == selection.effort)
        })
    {
        selection.effort = ["xhigh", "high", "medium"]
            .into_iter()
            .find(|effort| {
                model["supportedReasoningEfforts"]
                    .as_array()
                    .is_some_and(|levels| {
                        levels
                            .iter()
                            .any(|level| level["reasoningEffort"] == *effort)
                    })
            })
            .or_else(|| model["defaultReasoningEffort"].as_str())
            .ok_or_else(|| io::Error::other("No supported reasoning level."))?
            .into();
        selection.reason.push_str(" · supported reasoning fallback");
    }
    Ok(selection)
}

fn check_boundary(rpc: &mut Rpc, project: &Path, permissions: &str) -> io::Result<()> {
    static NEXT_CHECK: AtomicU64 = AtomicU64::new(0);
    let canary = env::temp_dir().join(format!(
        "sprowt-credential-check-{}-{}",
        std::process::id(),
        NEXT_CHECK.fetch_add(1, Ordering::Relaxed)
    ));
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&canary)?;
    drop(file);
    let script = if cfg!(target_os = "macos") {
        "test -r \"$1\" && ! test -r \"$2\" && ! (printf x >\"$2\") && ! test -r \"$3\" && ! test -r \"$4\" && ! /usr/bin/security list-keychains >/dev/null 2>&1"
    } else {
        "test -r \"$1\" && ! test -r \"$2\" && ! (printf x >\"$2\") && ! test -r \"$3\" && ! test -r \"$4\""
    };
    let result = rpc.call("command/exec", json!({"command":["/bin/sh","-c",script,"sprowt-check",project,canary,crate::router::directory()?.join("router.env"),project.join(".env.local")],"cwd":project,"permissionProfile":permissions,"timeoutMs":5000}));
    let _ = fs::remove_file(&canary);
    if result?["exitCode"] != 0 {
        return Err(io::Error::other(
            "The credential boundary check failed; the worker was not started.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "runs one Astra xhigh planning turn with the host ChatGPT subscription"]
    fn astra_xhigh_planner_returns_contracts_without_changing_source() {
        use crate::store::test_support::TestData;
        let data = TestData::new();
        let project = data.0.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join("README.md"),
            "A greeting program. Keep changes focused.",
        )
        .unwrap();
        fs::write(project.join("hello.py"), "print('hello')\n").unwrap();
        fs::write(
            project.join(".env.local"),
            "TYPESAFE_API_KEY=test-only-canary",
        )
        .unwrap();
        let before = crate::workspace::source_state(&project).unwrap();
        let mut store = data.store();
        let id = store.load_project(&project).unwrap().id;
        let goal = "Plan two independent changes: update hello.py to print hello sprowt, and document running it in README.md. No other changes. Define any shared contract first.";
        let m = store.create_mod(id, goal).unwrap();
        let record = store.worker_for(m.id, Role::Planner).unwrap();
        let mut client = Client::start(
            &project,
            None,
            Role::Planner,
            goal,
            None,
            Some(Selection::planner()),
            Context::worker(&m, record.id, Role::Planner),
        )
        .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(180);
        let mut final_plan = None;
        let mut completed = false;
        while std::time::Instant::now() < deadline && !completed {
            for event in client.poll() {
                match event {
                    Event::Ready { model, effort, .. } => {
                        assert_eq!(model.as_deref(), Some("gpt-6-astra"));
                        assert_eq!(effort.as_deref(), Some("xhigh"));
                        client
                            .send(Action::Run {
                                source: m.planning.as_ref().unwrap().source.clone(),
                                text: goal.into(),
                                routing: None,
                            })
                            .unwrap();
                    }
                    Event::Notification(message) if message["method"] == "item/completed" => {
                        let item = &message["params"]["item"];
                        if item["type"] == "agentMessage" && item["phase"] != "commentary" {
                            final_plan = item["text"].as_str().map(str::to_owned);
                        }
                    }
                    Event::Notification(message) if message["method"] == "turn/completed" => {
                        assert_eq!(message["params"]["turn"]["status"], "completed");
                        completed = true;
                    }
                    Event::Failed(error) | Event::Rejected { message: error, .. } => {
                        panic!("{error}")
                    }
                    _ => {}
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
        client.shutdown();
        assert!(completed, "Planner turn timed out");
        let text = final_plan.unwrap();
        let plan = Plan::parse(&text).unwrap();
        let output: Value = serde_json::from_str(&text).unwrap();
        for field in ["contracts", "assumptions", "non_goals"] {
            assert!(output[field].is_array());
        }
        let owned_files = plan
            .tasks
            .iter()
            .flat_map(|task| &task.files)
            .collect::<Vec<_>>();
        assert!(
            owned_files
                .iter()
                .all(|file| ["hello.py", "README.md"].contains(&file.as_str()))
        );
        assert!(
            owned_files.iter().any(|file| file.as_str() == "hello.py")
                && owned_files.iter().any(|file| file.as_str() == "README.md")
        );
        assert!(before == crate::workspace::source_state(&project).unwrap());
    }

    #[test]
    fn catalog_validation_preserves_xhigh_and_falls_back_to_supported_profiles() {
        let model = |name, efforts: &[&str], default| {
            json!({"model":name,
            "supportedReasoningEfforts":efforts.iter().map(|effort| json!({"reasoningEffort":effort})).collect::<Vec<_>>(),
            "defaultReasoningEffort":default,"isDefault":name=="gpt-6.1-sol"})
        };
        let catalog = vec![
            model("gpt-6-astra", &["high", "xhigh"], "high"),
            model("gpt-6.1-sol", &["medium", "high", "xhigh"], "medium"),
        ];
        let selection = resolve_model(&catalog, Selection::planner()).unwrap();
        assert_eq!(
            (selection.model.as_str(), selection.effort.as_str()),
            ("gpt-6-astra", "xhigh")
        );
        let limited = vec![model("gpt-6.1-sol", &["medium", "high"], "medium")];
        let fallback = resolve_model(&limited, Selection::planner()).unwrap();
        assert_eq!(
            (fallback.model.as_str(), fallback.effort.as_str()),
            ("gpt-6.1-sol", "high")
        );
        assert!(fallback.reason.contains("catalog fallback"));
        assert!(resolve_model(&[], Selection::planner()).is_err());
    }
    use crate::{store::test_support::TestData, workspace};
    use std::fs;

    #[test]
    #[ignore = "checks Chromium through a real worker and verifier in a temporary VM"]
    fn browser_turn_and_verification_preserve_boundaries() {
        let data = TestData::new();
        let project = data.0.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join("browser_check.py"),
            include_str!("../tests/fixtures/browser_check.py"),
        )
        .unwrap();
        let parent = data.0.join("workspaces");
        fs::create_dir(&parent).unwrap();
        let root = parent.join(format!("browser-{}", std::process::id()));
        workspace::create(&project, &root).unwrap();
        let mut store = data.store();
        let project_id = store.load_project(&project).unwrap().id;
        let mut m = store
            .create_mod(project_id, "Check browser startup")
            .unwrap();
        let plan = Plan::parse(&json!({"summary":"Browser checks", "tasks":[
            {"id":"browser","title":"Check browser","outcome":"Browser starts","files":["marker.txt"],"depends_on":[],"worker":"codex","checks":["Browser and boundaries pass"]},
            {"id":"peer","title":"Peer fixture","outcome":"Peer stays isolated","files":["peer.txt"],"depends_on":[],"worker":"codex","checks":["Peer stays isolated"]}
        ]}).to_string()).unwrap();
        store.create_execution(m.id, &root, &plan).unwrap();
        m.execution = store.execution(m.id).unwrap();
        let worker = store.worker_at(m.id, Role::Executor, 0).unwrap().id;
        let peer = store.worker_at(m.id, Role::Executor, 1).unwrap().id;
        let runs = &m.execution.as_ref().unwrap().tasks;
        let flag = AtomicBool::new(false);
        let command = vec![
            "/usr/bin/python3".into(),
            "browser_check.py".into(),
            format!("/tasks/{}", runs[1].id),
            format!("/home/sprowt/workers/{peer}"),
        ];
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> io::Result<()> {
                let mut vm = Sandbox::prepare(&root, &flag, |label| eprintln!("{label}"))?;
                vm.install_packages(&crate::packages::Request::parse(json!({
                    "packages":["chromium","python3"], "reason":"Check browser IPC and sandbox boundaries"
                }))?, &flag)?;
                vm.prepare_tasks(&runs.iter().map(|run| run.id).collect::<Vec<_>>(), &flag)?;
                vm.assign_task(runs[1].id, peer, &flag)?;
                vm.assign_task(runs[0].id, worker, &flag)?;
                let check = Check {
                    task: Some(runs[0].id),
                    check: "Browser and boundaries pass".into(),
                    command: command.clone(),
                };
                let (_, checked) = vm.verify(std::slice::from_ref(&check), &flag)?;
                assert_eq!(checked[0].exit_code, Some(0), "{}", checked[0].output);
                eprintln!("Verifier browser and boundary checks passed");
                drop(vm);
                let mut client = Client::start(
                    &project,
                    None,
                    Role::Executor,
                    &m.description,
                    Some(&plan),
                    None,
                    Context::worker(&m, worker, Role::Executor),
                )?;
                let deadline = std::time::Instant::now() + Duration::from_secs(180);
                let mut complete = false;
                while std::time::Instant::now() < deadline && !complete {
                    for event in client.poll() {
                        match event {
                            Event::Ready { .. } => client.send(Action::Run {
                                source: runs[0].source.clone(),
                                routing: None,
                                text: format!("Run exactly {} in your task folder. It checks Chromium startup, local Unix sockets, network restrictions and peer write boundaries. It starts and stops its own local app. Only if it exits 0, write exactly ok to marker.txt and report completed. Do not modify browser_check.py, install anything or write other source files. The completion check is 'Browser and boundaries pass'; use that same Python command.", command.join(" ")),
                            })?,
                            Event::Notification(message) if message["method"] == "turn/completed" => {
                                assert_eq!(message["params"]["turn"]["status"], "completed");
                                complete = true;
                            }
                            Event::Failed(error) | Event::Rejected { message: error, .. } => return Err(io::Error::other(error)),
                            _ => {}
                        }
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                assert!(complete, "Browser worker timed out");
                client.shutdown();
                assert_eq!(fs::read_to_string(root.join("work/marker.txt"))?, "ok");
                eprintln!("Worker browser and boundary checks passed");
                Ok(())
            },
        ));
        crate::sandbox::delete(&root).unwrap();
        result.unwrap().unwrap();
    }

    #[test]
    #[ignore = "runs two concurrent Codex tasks in one temporary Apple Container VM"]
    fn two_clients_share_a_vm_and_run_isolated_tasks_concurrently() {
        let data = TestData::new();
        let project = data.0.join("project");
        fs::create_dir_all(&project).unwrap();
        let parent = data.0.join("workspaces");
        fs::create_dir(&parent).unwrap();
        let root = parent.join(format!("parallel-{}", std::process::id()));
        workspace::create(&project, &root).unwrap();
        let mut store = data.store();
        let project_id = store.load_project(&project).unwrap().id;
        let mut m = store
            .create_mod(project_id, "Create two markers concurrently")
            .unwrap();
        let plan = Plan::parse(&json!({"summary":"Two markers", "tasks":[
            {"id":"a","title":"A","outcome":"A exists","files":["a.txt"],"depends_on":[],"worker":"codex","checks":["A exists"]},
            {"id":"b","title":"B","outcome":"B exists","files":["b.txt"],"depends_on":[],"worker":"codex","checks":["B exists"]}
        ]}).to_string()).unwrap();
        store.create_execution(m.id, &root, &plan).unwrap();
        m.execution = store.execution(m.id).unwrap();
        let runs = &m.execution.as_ref().unwrap().tasks;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> io::Result<()> {
                let mut clients = Vec::new();
                let mut ids = Vec::new();
                for slot in 0..2 {
                    let record = store.worker_at(m.id, Role::Executor, slot).unwrap();
                    ids.push(record.id);
                    clients.push(Client::start(
                        &project,
                        None,
                        Role::Executor,
                        &m.description,
                        Some(&plan),
                        None,
                        Context::worker(&m, record.id, Role::Executor),
                    )?);
                }
                let mut accepted = [false; 2];
                let mut completed = [false; 2];
                let mut checked = [false; 2];
                let deadline = std::time::Instant::now() + Duration::from_secs(240);
                while std::time::Instant::now() < deadline && !checked.iter().all(|v| *v) {
                    for i in 0..2 {
                        let file = if i == 0 { "a.txt" } else { "b.txt" };
                        let peer = if i == 0 { "b.txt" } else { "a.txt" };
                        for event in clients[i].poll() {
                            match event {
                            Event::Ready { .. } => clients[i].send(Action::Run {
                                source: runs[i].source.clone(),
                                routing: None,
                                text: format!("Your working directory is /tasks/{}. Run one shell command that: checks pwd; checks HOME is /home/sprowt/workers/{}; checks you cannot write /workspace/denied, /tasks/{}/denied, /home/sprowt/workers/{}/denied or .git; writes ok to {file}; then waits up to 90 seconds for /tasks/{}/{peer} to contain ok. Another real worker must create that peer file; do not create or edit it yourself. Use a one-second polling loop, with an error if the peer never appears. Write no other source files and install nothing. Report completed only after the peer appears. Completion check: '{}', using /bin/sh -c and relative {file}.", runs[i].id, ids[i], runs[1-i].id, ids[1-i], runs[1-i].id, if i==0 {"A exists"} else {"B exists"}),
                            })?,
                            Event::Accepted { .. } => accepted[i] = true,
                            Event::Notification(message) if message["method"] == "turn/completed" => {
                                assert!(accepted.iter().all(|v| *v), "Both tasks must start before either finishes");
                                assert_eq!(message["params"]["turn"]["status"], "completed");
                                completed[i] = true;
                                clients[i].send(Action::Verify { source: runs[i].source.clone(), checks: vec![Check {
                                    task: Some(runs[i].id), check: "Marker exists".into(),
                                    command: vec!["/bin/sh".into(), "-c".into(), format!("test \"$(cat {file})\" = ok && test \"$HOME\" = /home/sprowt/workers/{}", ids[i])],
                                }] })?;
                            }
                            Event::Checked { checks, .. } => {
                                assert_eq!(checks.len(),1);
                                assert_eq!(checks[0].exit_code,Some(0), "{}",checks[0].output);
                                checked[i] = true;
                            }
                            Event::Failed(error) | Event::Rejected { message:error, .. } => return Err(io::Error::other(error)),
                            _ => {}
                        }
                        }
                    }
                    thread::sleep(Duration::from_millis(30));
                }
                assert!(
                    completed.iter().all(|v| *v) && checked.iter().all(|v| *v),
                    "Parallel tasks timed out"
                );
                let first = clients[0].vm.lock().unwrap().as_ref().unwrap().clone();
                let second = clients[1].vm.lock().unwrap().as_ref().unwrap().clone();
                assert!(Arc::ptr_eq(&first, &second));
                clients[0].shutdown();
                // Dropping one worker must leave the shared VM usable.
                let flag = AtomicBool::new(false);
                let check = Check {
                    task: Some(runs[1].id),
                    check: "Combined markers".into(),
                    command: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        "test -f a.txt && test -f b.txt".into(),
                    ],
                };
                assert_eq!(
                    second
                        .lock()
                        .unwrap()
                        .verify_execution("final:1", &[check], &flag)?
                        .1[0]
                        .exit_code,
                    Some(0)
                );
                clients[1].shutdown();
                assert_eq!(fs::read_to_string(root.join("work/a.txt"))?.trim(), "ok");
                assert_eq!(fs::read_to_string(root.join("work/b.txt"))?.trim(), "ok");
                Ok(())
            },
        ));
        crate::sandbox::delete(&root).unwrap();
        result.unwrap().unwrap();
    }

    #[test]
    #[ignore = "runs one short Codex task in a temporary Apple Container VM"]
    fn task_turn_uses_its_worktree_and_permission_profile() {
        let data = TestData::new();
        let project = data.0.join("project");
        fs::create_dir_all(&project).unwrap();
        let parent = data.0.join("workspaces");
        fs::create_dir(&parent).unwrap();
        let root = parent.join(format!("agent-{}", std::process::id()));
        workspace::create(&project, &root).unwrap();
        let mut store = data.store();
        let project_id = store.load_project(&project).unwrap().id;
        let mut code_mod = store
            .create_mod(project_id, "Create marker.txt containing ok")
            .unwrap();
        let plan = Plan::parse(&json!({"summary":"Create marker", "tasks":[{"id":"marker","title":"Create marker","outcome":"Marker exists","files":["marker.txt"],"depends_on":[],"worker":"codex","checks":["Marker contains ok"]}]}).to_string()).unwrap();
        store.create_execution(code_mod.id, &root, &plan).unwrap();
        code_mod.execution = store.execution(code_mod.id).unwrap();
        let record = store.worker_for(code_mod.id, Role::Executor).unwrap();
        let run = &code_mod.execution.as_ref().unwrap().tasks[0];
        let flag = AtomicBool::new(false);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> io::Result<()> {
                let mut client = Client::start(
                    &project,
                    None,
                    Role::Executor,
                    &code_mod.description,
                    Some(&plan),
                    None,
                    Context::worker(&code_mod, record.id, Role::Executor),
                )?;
                let deadline = std::time::Instant::now() + Duration::from_secs(180);
                let mut complete = false;
                while std::time::Instant::now() < deadline && !complete {
                    for event in client.poll() {
                        match event {
                        Event::Ready { .. } => client.send(Action::Run {
                            source: run.source.clone(),
                            routing: None,
                            text: format!("Your task worktree is /tasks/{}. Use the shell to confirm pwd is this path, confirm writes to /workspace/denied and .git are refused, then write exactly ok to marker.txt. Return completed only if those boundary checks pass. The sole completion check is 'Marker contains ok'; use /bin/sh -c with a relative marker.txt path. Do not install anything or modify other files.", run.id),
                        })?,
                        Event::Notification(message) if message["method"] == "turn/completed" => {
                            assert_eq!(message["params"]["turn"]["status"], "completed");
                            complete = true;
                        }
                        Event::Failed(error) | Event::Rejected { message: error, .. } => return Err(io::Error::other(error)),
                        _ => {}
                    }
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                assert!(complete, "Codex task did not finish within three minutes");
                client.shutdown();
                assert_eq!(fs::read_to_string(root.join("work/marker.txt"))?, "ok");
                let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
                let check = Check {
                    task: Some(run.id),
                    check: "Marker contains ok".into(),
                    command: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        "test \"$(cat marker.txt)\" = ok && test ! -e /workspace/denied".into(),
                    ],
                };
                let (_, results) =
                    vm.verify_execution(&run.source, std::slice::from_ref(&check), &flag)?;
                assert_eq!(results[0].exit_code, Some(0));
                assert_eq!(
                    vm.verify_execution("final:1", &[check], &flag)?.1[0].exit_code,
                    Some(0)
                );
                drop(vm);
                Ok(())
            },
        ));
        crate::sandbox::delete(&root).unwrap();
        result.unwrap().unwrap();
    }
}
