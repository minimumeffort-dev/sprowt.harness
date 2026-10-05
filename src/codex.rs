use std::{
    collections::BTreeMap,
    env, fs, io,
    path::Path,
    process::{Child, Command},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
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
    Preparing(String),
    Configured(Selection),
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
    vm: Option<Sandbox>,
    context: Context,
}

pub struct Client {
    actions: Sender<Action>,
    events: Receiver<Event>,
    child: Arc<Mutex<Option<Child>>>,
    task: Option<JoinHandle<()>>,
    closed: bool,
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
        let instructions = plan::instructions(role, description, plan, workspace.is_some());
        let cancelled = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));
        let process = child.clone();
        let stopped = cancelled.clone();
        let task = thread::spawn(move || {
            let result = (|| {
                let mut vm = workspace
                    .as_ref()
                    .map(|path| {
                        Sandbox::prepare(path.parent().unwrap(), &stopped, |label| {
                            let _ = outgoing.send(Event::Preparing(label.into()));
                        })
                    })
                    .transpose()?;
                if let Some(vm) = &mut vm {
                    vm.prepare_tasks(&context.tasks, &stopped)?;
                }
                if stopped.load(Ordering::Relaxed) {
                    return Ok(());
                }
                let permissions = if vm.is_some() {
                    "sprowt_vm"
                } else {
                    "sprowt_readonly"
                };
                let mut command = Command::new("codex");
                command
                    .args(["app-server", "--listen", "stdio://"])
                    .current_dir(
                        vm.as_ref()
                            .map_or(cwd.as_path(), |vm| vm.home.parent().unwrap()),
                    );
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
                    command.env("CODEX_HOME", &vm.home);
                    for option in vm
                        .configuration()
                        .into_iter()
                        .chain(vm.task_configuration())
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
            if let Err(error) = result {
                let _ = outgoing.send(Event::Failed(error.to_string()));
            }
        });
        Ok(Self {
            actions,
            events,
            child,
            task: Some(task),
            closed: false,
            cancelled,
        })
    }

    pub fn send(&self, action: Action) -> io::Result<()> {
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
            let _ = receiver.recv_timeout(Duration::from_secs(2));
        }
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            rpc::terminate(child);
        }
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
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
    vm: &mut Option<Sandbox>,
    thread: &str,
    cancelled: &AtomicBool,
    context: &Context,
) -> io::Result<()> {
    for message in std::mem::take(&mut rpc.buffered) {
        if message["method"] == "item/tool/call" && message.get("id").is_some() {
            let params = &message["params"];
            let result = (|| {
                if params["threadId"] != thread || !params["namespace"].is_null() {
                    return Err(io::Error::other("This client tool is not available."));
                }
                Dispatcher::new(context, vm.as_mut(), cancelled)
                    .worker_call(
                        params["tool"].as_str().unwrap_or(""),
                        params["arguments"].clone(),
                        |label| {
                            let _ = outgoing.send(Event::Preparing(label.into()));
                        },
                    )
                    .map(Output::message)
            })();
            rpc.write(json!({"id":message["id"],"result":{"success":result.is_ok(),"contentItems":[{"type":"inputText","text":result.unwrap_or_else(|error|error.to_string())}]}}))?;
            continue;
        }
        if message["method"] == "turn/completed"
            && let Some(vm) = vm
        {
            stop_terminals(rpc, thread)?;
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
            json!("/home/sprowt"),
        );
    }
    let selection = selection
        .map(|selection| select_model(rpc, selection))
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
    // Old executor threads have no setup tool; keep their saved transcript and VM.
    let resume = resume.filter(|resume| {
        vm.as_ref().is_none_or(|vm| {
            fs::read_to_string(vm.home.join("system-packages-thread"))
                .ok()
                .as_deref()
                == Some(resume.id.as_str())
        })
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
    if let Some(vm) = &vm {
        fs::write(vm.home.join("system-packages-thread"), &thread)?;
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
    let effort = if let Some(effort) = result["reasoningEffort"].as_str() {
        Some(effort.to_owned())
    } else {
        model_catalog(rpc)?
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
    flush(rpc, outgoing, &mut vm, &thread, &cancelled, &context)?;
    let mut last_turn = None;
    loop {
        while let Ok(action) = actions.try_recv() {
            if let Action::Verify { source, checks } = &action {
                let (before, results) = vm
                    .as_mut()
                    .ok_or_else(|| io::Error::other("Verification requires the mod VM."))?
                    .verify_execution(source, checks, &cancelled)?;
                let _ = outgoing.send(Event::Checked {
                    source: source.clone(),
                    checks: results,
                    before,
                });
                flush(rpc, outgoing, &mut vm, &thread, &cancelled, &context)?;
                continue;
            }
            let (method, source, params) = match action {
                Action::Run { source, text } => {
                    let mut params = json!({"threadId":thread,"clientUserMessageId":source,"input":[{"type":"text","text":text}],"permissions":permissions,"approvalPolicy":"never"});
                    if let Some(effort) = &effort {
                        params["effort"] = json!(effort);
                    }
                    if role == Role::Planner {
                        params["outputSchema"] = plan::schema();
                        if let Some(selection) = &selection {
                            params["model"] = json!(selection.model);
                            params["effort"] = json!(selection.effort);
                        }
                    }
                    if source.starts_with("00000004-") {
                        let vm = vm
                            .as_mut()
                            .ok_or_else(|| io::Error::other("Task execution requires a VM."))?;
                        let id = crate::task_worktree::task_id(&source)?;
                        let _ = outgoing.send(Event::Preparing("preparing task worktree".into()));
                        stop_terminals(rpc, &thread)?;
                        vm.activate_task(id, &cancelled)?;
                        params["permissions"] = json!(format!("sprowt_task_{id}"));
                        params["environments"] =
                            json!([{"environmentId":"vm","cwd":vm.task_folder()}]);
                        params["outputSchema"] = crate::execution::schema();
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
            flush(rpc, outgoing, &mut vm, &thread, &cancelled, &context)?;
        }
        match rpc.receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(message) => {
                rpc.receive(message?)?;
                flush(rpc, outgoing, &mut vm, &thread, &cancelled, &context)?;
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

fn select_model(rpc: &mut Rpc, mut selection: Selection) -> io::Result<Selection> {
    let models = model_catalog(rpc)?;
    let available = |model: &Value| model["model"] == selection.model;
    let model = if let Some(model) = models.iter().find(|model| available(model)) {
        model
    } else {
        let model = models
            .iter()
            .find(|model| model["model"] == "gpt-6.1-sol")
            .or_else(|| models.iter().find(|model| model["isDefault"] == true))
            .ok_or_else(|| io::Error::other("No supported planning model is listed by Codex."))?;
        selection
            .reason
            .push_str(" · selected model unavailable; catalog fallback");
        selection.model = model["model"]
            .as_str()
            .ok_or_else(|| io::Error::other("Invalid model catalog."))?
            .into();
        selection.effort = "high".into();
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
        selection.effort = model["defaultReasoningEffort"]
            .as_str()
            .ok_or_else(|| io::Error::other("No supported planner reasoning level."))?
            .into();
        selection.reason.push_str(" · default reasoning fallback");
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
        "test -r \"$1\" && ! test -r \"$2\" && ! (printf x >\"$2\") && ! /usr/bin/security list-keychains >/dev/null 2>&1"
    } else {
        "test -r \"$1\" && ! test -r \"$2\" && ! (printf x >\"$2\")"
    };
    let result = rpc.call("command/exec", json!({"command":["/bin/sh","-c",script,"sprowt-check",project,canary],"cwd":project,"permissionProfile":permissions,"timeoutMs":5000}));
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
    use crate::{store::test_support::TestData, workspace};
    use std::fs;

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
