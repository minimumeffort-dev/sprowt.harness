use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

pub enum Action {
    Run {
        source: String,
        text: String,
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
    Ready(Value),
    Accepted { source: String, turn: String },
    Notification(Value),
    Rejected { source: String, message: String },
    Failed(String),
}

pub struct Client {
    actions: Sender<Action>,
    events: Receiver<Event>,
    child: Child,
    task: Option<JoinHandle<()>>,
    closed: bool,
}

impl Client {
    pub fn start(project: &Path, thread_id: Option<String>) -> io::Result<Self> {
        #[cfg(not(unix))]
        return Err(io::Error::other(
            "The first Codex worker currently supports macOS and Linux.",
        ));
        let mut command = Command::new("codex");
        command.args(["app-server", "--listen", "stdio://"]);
        command
            .current_dir(project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let inherited = [
            "PATH",
            "HOME",
            "USER",
            "LOGNAME",
            "LANG",
            "TMPDIR",
            "CODEX_HOME",
        ];
        command.env_clear().envs(
            inherited
                .iter()
                .filter_map(|name| env::var_os(name).map(|value| (*name, value))),
        );
        for option in configuration(project)? {
            command.args(["-c", &option]);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (incoming, receiver) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let message =
                    line.and_then(|line| serde_json::from_str(&line).map_err(io::Error::other));
                if incoming.send(message).is_err() {
                    break;
                }
            }
        });
        let (actions, inbox) = mpsc::channel();
        let (outgoing, events) = mpsc::channel();
        let project = project.to_owned();
        let task = thread::spawn(move || {
            let mut rpc = Rpc {
                stdin,
                receiver,
                next_id: 0,
                buffered: Vec::new(),
            };
            if let Err(error) = serve(&mut rpc, &project, thread_id, inbox, &outgoing) {
                let _ = outgoing.send(Event::Failed(error.to_string()));
            }
        });
        Ok(Self {
            actions,
            events,
            child,
            task: Some(task),
            closed: false,
        })
    }

    pub fn send(&self, action: Action) -> io::Result<()> {
        self.actions.send(action).map_err(io::Error::other)
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
        let (finished, receiver) = mpsc::channel();
        if self.actions.send(Action::Shutdown(finished)).is_ok() {
            let _ = receiver.recv_timeout(Duration::from_secs(2));
        }
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
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
    let mut config = vec![
        format!("permissions.sprowt_readonly={{filesystem={{{rules}}},network={{enabled=false}}}}"),
        "default_permissions=\"sprowt_readonly\"".into(),
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

struct Rpc {
    stdin: ChildStdin,
    receiver: Receiver<io::Result<Value>>,
    next_id: u64,
    buffered: Vec<Value>,
}

impl Rpc {
    fn write(&mut self, value: Value) -> io::Result<()> {
        serde_json::to_writer(&mut self.stdin, &value)?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()
    }

    fn call(&mut self, method: &str, params: Value) -> io::Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.write(json!({"id":id,"method":method,"params":params}))?;
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            let message = self
                .receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(io::Error::other)??;
            if message.get("id") == Some(&json!(id)) && message.get("method").is_none() {
                if let Some(error) = message.get("error") {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        error["message"]
                            .as_str()
                            .unwrap_or("Codex rejected the request."),
                    ));
                }
                return Ok(message["result"].clone());
            }
            self.receive(message)?;
        }
    }

    fn receive(&mut self, message: Value) -> io::Result<()> {
        if message.get("method").is_some() && message.get("id").is_some() {
            self.write(json!({"id":message["id"],"error":{"code":-32601,"message":"This read-only worker cannot grant permissions or run client tools."}}))?;
        } else if message.get("method").is_some() {
            self.buffered.push(message);
        }
        Ok(())
    }

    fn flush(&mut self, outgoing: &Sender<Event>) {
        for message in self.buffered.drain(..) {
            let _ = outgoing.send(Event::Notification(message));
        }
    }
}

fn serve(
    rpc: &mut Rpc,
    project: &Path,
    thread_id: Option<String>,
    actions: Receiver<Action>,
    outgoing: &Sender<Event>,
) -> io::Result<()> {
    rpc.call("initialize", json!({"clientInfo":{"name":"sprowt_harness","title":"Sprowt Harness","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}))?;
    rpc.write(json!({"method":"initialized"}))?;
    let account = rpc.call("account/read", json!({"refreshToken":false}))?;
    if account["account"]["type"] != "chatgpt" {
        return Err(io::Error::other(
            "Sign in with ChatGPT using `codex login`, then run again.",
        ));
    }
    check_boundary(rpc, project)?;
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
    let mut params = json!({"cwd":project,"permissions":"sprowt_readonly","approvalPolicy":"never","config":overrides,"developerInstructions":"This worker is read-only. Inspect the project and answer concisely. Do not change files, request broader permissions, access credentials, or use external tools."});
    let method = if let Some(id) = thread_id {
        params["threadId"] = json!(id);
        "thread/resume"
    } else {
        "thread/start"
    };
    let result = rpc.call(method, params)?;
    let thread = result["thread"]["id"]
        .as_str()
        .ok_or_else(|| io::Error::other("Codex returned no conversation ID."))?
        .to_owned();
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
            "External MCP tools are enabled; this read-only worker cannot start.",
        ));
    }
    outgoing
        .send(Event::Ready(result["thread"].clone()))
        .map_err(io::Error::other)?;
    rpc.flush(outgoing);
    let mut last_turn = None;
    loop {
        while let Ok(action) = actions.try_recv() {
            let (method, source, params) = match action {
                Action::Run { source, text } => (
                    "turn/start",
                    source.clone(),
                    json!({"threadId":thread,"clientUserMessageId":source,"input":[{"type":"text","text":text}],"permissions":"sprowt_readonly","approvalPolicy":"never"}),
                ),
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
                    let terminals = rpc.call(
                        "thread/backgroundTerminals/list",
                        json!({"threadId":thread}),
                    )?;
                    let terminals = terminals["data"]
                        .as_array()
                        .ok_or_else(|| io::Error::other("Codex returned no command inventory."))?;
                    for terminal in terminals {
                        rpc.call(
                            "thread/backgroundTerminals/terminate",
                            json!({"threadId":thread,"processId":terminal["processId"]}),
                        )?;
                    }
                    let _ = finished.send(());
                    return Ok(());
                }
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
            rpc.flush(outgoing);
        }
        match rpc.receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(message) => {
                rpc.receive(message?)?;
                rpc.flush(outgoing);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => return Err(io::Error::other(error)),
        }
    }
}

fn check_boundary(rpc: &mut Rpc, project: &Path) -> io::Result<()> {
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
        "test -r \"$1\" && ! test -r \"$2\" && ! /usr/bin/security list-keychains >/dev/null 2>&1"
    } else {
        "test -r \"$1\" && ! test -r \"$2\""
    };
    let result = rpc.call("command/exec", json!({"command":["/bin/sh","-c",script,"sprowt-check",project,canary],"cwd":project,"permissionProfile":"sprowt_readonly","timeoutMs":5000}));
    let _ = fs::remove_file(&canary);
    if result?["exitCode"] != 0 {
        return Err(io::Error::other(
            "The credential boundary check failed; the worker was not started.",
        ));
    }
    Ok(())
}
