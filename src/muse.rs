use std::{
    env, fs, io,
    process::{Child, Command},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, Sender, TryRecvError},
    },
    thread,
    time::Duration,
};

use directories::ProjectDirs;
use serde_json::json;

use crate::{
    codex::{Action, Event, Resume, SharedVm},
    router::Selection,
    rpc::Rpc,
    tools::{self, Context, Dispatcher, Output},
};

pub const MODEL: &str = "muse-spark-1.3";
pub const VERSION: &str = "1.4.3-R5018.1";
static FILE: AtomicU64 = AtomicU64::new(0);

fn helper() -> io::Result<std::path::PathBuf> {
    let dirs = ProjectDirs::from("", "", "sprowt-harness")
        .ok_or_else(|| io::Error::other("Cannot locate the Muse cache."))?;
    let root = dirs.cache_dir().join("muse");
    fs::create_dir_all(&root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    }
    for (name, content) in [
        ("bridge.py", include_str!("muse_bridge.py")),
        ("muse_transport.py", include_str!("muse_transport.py")),
    ] {
        let target = root.join(name);
        if fs::read(&target).ok().as_deref() == Some(content.as_bytes()) {
            continue;
        }
        // Concurrent workers must never import a partially written adapter.
        let next = root.join(format!(
            ".{name}-{}-{}",
            std::process::id(),
            FILE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&next, content)?;
        fs::rename(next, target)?;
    }
    Ok(root)
}

fn command(root: &std::path::Path) -> Command {
    let mut command = Command::new("python3");
    command.arg(root.join("bridge.py")).env_clear().envs(
        [
            "PATH",
            "HOME",
            "USER",
            "LOGNAME",
            "LANG",
            "TMPDIR",
            "XDG_CONFIG_HOME",
        ]
        .iter()
        .filter_map(|name| env::var_os(name).map(|value| (*name, value))),
    );
    command
}

pub fn account_state() -> io::Result<String> {
    let root = helper()?;
    let account = root.join(format!(
        ".account-{}-{}",
        std::process::id(),
        FILE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&account)?;
    let result = (|| {
        let mut command = command(&root);
        command.arg("--account").arg(&account);
        let output = crate::agents::output(command, Duration::from_secs(15)).map_err(|_| {
            io::Error::other("Could not check Muse login. Ensure Python 3 is installed, then run muse login and restart the harness.")
        })?;
        if output.status.success()
            && let Some(state) = serde_json::from_slice::<serde_json::Value>(&output.stdout)
                .ok()
                .and_then(|v| v["state"].as_str().map(str::to_owned))
        {
            return Ok(state);
        }
        Err(io::Error::other(
            "Could not read your Muse account. Run muse login and restart the harness.",
        ))
    })();
    fs::remove_dir_all(account)?;
    result
}

pub fn serve(
    vm: SharedVm,
    resume: Option<Resume>,
    setup: (String, Context),
    cancelled: Arc<AtomicBool>,
    process: Arc<Mutex<Option<Child>>>,
    actions: Receiver<Action>,
    outgoing: &Sender<Event>,
) -> io::Result<()> {
    let (instructions, context) = setup;
    let _ = outgoing.send(Event::Preparing("preparing Muse".into()));
    let root = helper()?;
    let binary = root.join(format!("muse-{VERSION}"));
    // Verify the pinned artifact on every connection, even when cached.
    let output = command(&root)
        .args(["--artifact", root.to_str().unwrap()])
        .output()?;
    if !output.status.success() || !binary.is_file() {
        return Err(io::Error::other(format!(
            "Muse setup failed. Install Muse {VERSION} and log in with your account."
        )));
    }
    vm.lock()
        .unwrap()
        .prepare_muse(&root, &binary, &cancelled)?;
    let (child, mut rpc) = Rpc::start(&mut command(&root))?;
    *process.lock().unwrap() = Some(child);
    rpc.client_tools = true;
    let ready = rpc
        .receiver
        .recv_timeout(Duration::from_secs(45))
        .map_err(io::Error::other)??;
    if ready["method"] != "bridge/ready" {
        if ready["method"] == "bridge/failed" {
            return Err(crate::rpc::bridge_error(&ready));
        }
        return Err(io::Error::other(
            "Muse account login could not be resolved. Run muse and sign in first.",
        ));
    }
    let state = context
        .workspace()
        .unwrap()
        .join("muse")
        .join(format!("{}.json", context.worker_id()));
    let mut task = None;
    let mut thread = json!({"id":format!("muse-worker-{}", context.worker_id()),"turns":[]});
    if resume.is_some() && state.exists() {
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&state)?)?;
        if let Some(id) = saved["task"]
            .as_i64()
            .filter(|id| context.tasks.contains(id))
        {
            let config = {
                let mut vm = vm.lock().unwrap();
                vm.assign_muse_task(id, context.worker_id(), &cancelled)?;
                vm.muse_configuration(context.worker_id())
            };
            let _ = outgoing.send(Event::Preparing("restoring Muse session".into()));
            thread = rpc.call(
                "session/resume",
                json!({"state":state,"config":config,
                "tools":tools::advertised(&context),
                "allowNew":resume.as_ref().is_some_and(|r| r.restart_if_missing)}),
            )?["thread"]
                .clone();
            task = Some(id);
        }
    }
    let _ = outgoing.send(Event::Ready {
        thread,
        model: Some(MODEL.into()),
        effort: Some("high".into()),
    });
    let mut accepted = resume.map_or_else(Vec::new, |r| r.accepted_instructions);
    loop {
        while let Ok(action) = actions.try_recv() {
            let (method, source, params) = match action {
                Action::Run {
                    source,
                    text,
                    routing,
                } => {
                    let report_schema = crate::execution::task_schema(routing.as_ref());
                    let id = crate::task_worktree::task_id(&source)?;
                    let config = {
                        let mut vm = vm.lock().unwrap();
                        vm.assign_muse_task(id, context.worker_id(), &cancelled)?;
                        if routing.as_ref().is_some_and(|state| {
                            !state["repair"].is_null()
                                || state["review_fix"] == true
                                || state["runtime_recovery"] == true
                        }) {
                            vm.refresh_for_repair(id, &source, &cancelled)?;
                        }
                        vm.muse_configuration(context.worker_id())
                    };
                    task = Some(id);
                    let _ = outgoing.send(Event::TaskConfigured {
                        source: source.clone(),
                        selection: Selection {
                            model: MODEL.into(),
                            effort: "high".into(),
                            reason: "Muse executor".into(),
                            evidence: None,
                        },
                    });
                    let prompt = format!(
                        "{instructions}\nAccepted user instructions: {}\n{text}",
                        accepted.join("\n\n")
                    );
                    (
                        "turn/start",
                        source.clone(),
                        json!({"source":source,"text":prompt,"config":config,"state":state,
                            "tools":tools::advertised(&context),"schema":report_schema}),
                    )
                }
                Action::Steer {
                    source,
                    texts,
                    turn,
                } => (
                    "turn/steer",
                    source.clone(),
                    json!({"source":source,"texts":texts,"turn":turn}),
                ),
                Action::Stop { turn } => ("turn/interrupt", String::new(), json!({"turn":turn})),
                Action::Verify { source, checks } => {
                    let (before, checks) = vm
                        .lock()
                        .unwrap()
                        .verify_execution(&source, &checks, &cancelled)?;
                    let _ = outgoing.send(Event::Checked {
                        source,
                        checks,
                        before,
                    });
                    continue;
                }
                Action::Shutdown(finished) => {
                    rpc.call("shutdown", json!({}))?;
                    if let Some(id) = task {
                        vm.lock().unwrap().checkpoint_task(id)?;
                    }
                    let _ = finished.send(());
                    return Ok(());
                }
            };
            let steering: Vec<String> = params["texts"].as_array().map_or_else(Vec::new, |texts| {
                texts
                    .iter()
                    .filter_map(|text| text.as_str().map(str::to_owned))
                    .collect()
            });
            match rpc.call(method, params) {
                Ok(result) if !source.is_empty() => {
                    let turn = result["turnId"]
                        .as_str()
                        .or_else(|| result["acceptedTurnId"].as_str())
                        .ok_or_else(|| io::Error::other("Muse returned no accepted turn ID."))?;
                    accepted.extend(steering);
                    let _ = outgoing.send(Event::Accepted {
                        source,
                        turn: turn.into(),
                    });
                }
                Ok(_) => (),
                Err(error) if !source.is_empty() && error.kind() == io::ErrorKind::InvalidInput => {
                    let _ = outgoing.send(Event::Rejected {
                        source,
                        message: error.to_string(),
                    });
                }
                Err(error) => return Err(error),
            }
        }
        let mut receive_error = None;
        loop {
            match rpc.receiver.try_recv() {
                Ok(value) => {
                    if let Err(error) = value.and_then(|value| rpc.receive(value)) {
                        receive_error = Some(error);
                        break;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let status = process
                        .lock()
                        .unwrap()
                        .as_mut()
                        .and_then(|child| child.try_wait().ok().flatten())
                        .map_or_else(|| "output closed".into(), |status| status.to_string());
                    receive_error = Some(io::Error::other(format!(
                        "Muse host adapter stopped ({status}). Work is retained; Ctrl+R retries."
                    )));
                    break;
                }
            }
        }
        for message in std::mem::take(&mut rpc.buffered) {
            if message["method"] == "item/tool/call" && message.get("id").is_some() {
                if receive_error.is_some() {
                    continue;
                }
                let params = &message["params"];
                let mut guard =
                    (params["name"] == crate::packages::TOOL).then(|| vm.lock().unwrap());
                if let Some(vm) = &mut guard {
                    vm.tasks.active = task;
                }
                let result = Dispatcher::new(&context, guard.as_deref_mut(), &cancelled)
                    .worker_call(
                        params["name"].as_str().unwrap_or(""),
                        params["arguments"].clone(),
                        |label| {
                            let _ = outgoing.send(Event::Preparing(label.into()));
                        },
                    )
                    .map(Output::message);
                rpc.write(json!({"id":message["id"],"result":{"isError":result.is_err(),"content":[{"type":"text","text":result.unwrap_or_else(|e|e.to_string())}]}}))?;
                let _ = outgoing.send(Event::MailboxChanged);
            } else {
                if message["method"] == "turn/completed"
                    && let Some(id) = task
                {
                    vm.lock().unwrap().checkpoint_task(id)?;
                }
                let _ = outgoing.send(Event::Notification(message));
            }
        }
        if let Some(error) = receive_error {
            return Err(error);
        }
        thread::sleep(Duration::from_millis(25));
    }
}
