use std::{
    env, fs,
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

const IMAGE: &str = "sprowt-probe:0.159.2";

fn main() -> io::Result<()> {
    let name = format!(
        "sprowt-probe-{}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = env::temp_dir().join(&name);
    fs::create_dir(&root)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    let result = probe(&name, &root);
    let cleanup = container(&["delete", "--force", &name]);
    fs::remove_dir_all(&root)?;
    result.and(cleanup)
}

fn probe(name: &str, root: &Path) -> io::Result<()> {
    container(&[
        "run", "--detach", "--name", name, "--cpus", "2", "--memory", "2G", IMAGE,
    ])?;
    let inspect = Command::new("container").args(["inspect", name]).output()?;
    ensure(inspect.status.success(), "Cannot inspect the VM")?;
    let config: Value = serde_json::from_slice(&inspect.stdout)?;
    for field in ["mounts", "publishedPorts"] {
        ensure(
            config[0]["configuration"][field]
                .as_array()
                .is_some_and(Vec::is_empty),
            "The probe must have no host mounts or published ports",
        )?;
    }
    let source = root.join("source");
    let home = root.join("codex");
    fs::create_dir(&source)?;
    fs::create_dir(&home)?;
    let manifest = "[project]\nname = \"sandbox-proof\"\nversion = \"0.1.0\"\nrequires-python = \">=3.12\"\ndependencies = [\"fastapi\", \"httpx\"]\n";
    fs::write(source.join("pyproject.toml"), manifest)?;
    let canary = root.join("host-only.txt");
    fs::write(&canary, "host only\n")?;
    container(&[
        "copy",
        source.join("pyproject.toml").to_str().unwrap(),
        &format!("{name}:/workspace/pyproject.toml"),
    ])?;
    container(&[
        "exec",
        name,
        "sh",
        "-c",
        "! command -v python3 && test ! -e /root/.codex/auth.json",
    ])?;

    let login_home = env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").unwrap()).join(".codex"));
    let login = login_home.join("auth.json");
    if !login.is_file() {
        return Err(io::Error::other(
            "This probe needs a file-backed Codex ChatGPT login on the Mac.",
        ));
    }
    // The login link stays in the host-only directory, never in the image or guest.
    std::os::unix::fs::symlink(login, home.join("auth.json"))?;
    let program = find_program("container")?;
    fs::write(
        home.join("environments.toml"),
        format!(
            "default = \"vm\"\ninclude_local = false\n[[environments]]\nid = \"vm\"\nprogram = {}\nargs = {}\ninitialize_timeout_sec = 30\n",
            json!(program),
            json!([
                "exec",
                "--interactive",
                "--workdir",
                "/workspace",
                name,
                "env",
                "-i",
                "HOME=/root",
                "PATH=/usr/local/bin:/usr/bin:/bin",
                "codex",
                "exec-server",
                "--listen",
                "stdio"
            ])
        ),
    )?;

    let mut rpc = Rpc::start(root, &home)?;
    rpc.call("initialize", json!({"clientInfo":{"name":"sprowt_container_probe","version":"0.1.0"},"capabilities":{"experimentalApi":true}}))?;
    rpc.write(json!({"method":"initialized"}))?;
    let account = rpc.call("account/read", json!({"refreshToken":false}))?;
    ensure(
        account["account"]["type"] == "chatgpt",
        "Codex is not signed in with ChatGPT",
    )?;
    ensure(
        rpc.call("environment/info", json!({"environmentId":"local"}))
            .is_err(),
        "Host execution must be unavailable",
    )?;
    let info = rpc.call("environment/info", json!({"environmentId":"vm"}))?;
    ensure(
        info["cwd"] == "file:///workspace",
        "Executor did not connect to /workspace",
    )?;
    println!("VM execution server connected; host environment unavailable.");

    let started = rpc.call("thread/start", json!({
        "cwd":root,"model":"gpt-6.1-sol","sandbox":"danger-full-access","approvalPolicy":"never","ephemeral":true,
        "environments":[{"environmentId":"vm","cwd":"/workspace"}],
        "config":{"model_reasoning_effort":"low"},
        "developerInstructions":"You are validating a disposable Linux sandbox. All work uses the vm environment. Install the runtime and dependencies required by the project. Use mise for Python and a project .venv. Use apply_patch for source edits. Keep your final report brief. Never request or read login credentials."
    }))?;
    let thread_id = &started["thread"]["id"];
    let prompt = format!(
        "Read pyproject.toml. Implement app.py with a FastAPI GET /health returning {{\"ok\":true}}. Add test_health.py using unittest and FastAPI TestClient. Choose a Python version compatible with the manifest, install it and the project dependencies, and run the test. Before doing work, run uname -s and confirm the host-only path {} cannot be read; confirm /root/.codex/auth.json and /root/.ssh do not exist, and no OPENAI_API_KEY, CODEX_API_KEY, or CODEX_ACCESS_TOKEN is set (report presence only, never values). Leave pyproject.toml unchanged.",
        canary.display()
    );
    rpc.call("turn/start", json!({"threadId":thread_id,"input":[{"type":"text","text":prompt,"text_elements":[]}],"sandboxPolicy":{"type":"externalSandbox","networkAccess":"enabled"}}))?;
    let mut commands = 0;
    let mut edits = 0;
    let deadline = Instant::now() + Duration::from_secs(900);
    loop {
        let message = rpc.next(deadline.saturating_duration_since(Instant::now()))?;
        if message.get("id").is_some() && message.get("method").is_some() {
            rpc.write(json!({"id":message["id"],"error":{"code":-32601,"message":"No host tools or permission grants in this probe"}}))?;
        }
        if message["method"] == "item/completed" {
            let item = &message["params"]["item"];
            match item["type"].as_str() {
                Some("commandExecution") => {
                    commands += 1;
                    println!("VM command: {}", item["status"]);
                }
                Some("fileChange") if item["status"] == "completed" => {
                    edits += 1;
                    println!("VM edit: {}", item["status"]);
                }
                Some("agentMessage") => println!("{}", item["text"].as_str().unwrap_or_default()),
                _ => {}
            }
        }
        if message["method"] == "turn/completed" {
            ensure(
                message["params"]["turn"]["status"] == "completed",
                "Codex turn did not complete",
            )?;
            break;
        }
    }
    ensure(
        commands > 0 && edits > 0,
        "Expected native command and file-edit tools",
    )?;
    container(&[
        "exec",
        name,
        "sh",
        "-c",
        "test \"$(uname -s)\" = Linux && .venv/bin/python -m unittest -v test_health && test ! -e /root/.codex/auth.json && test ! -e /root/.ssh && test -z \"${OPENAI_API_KEY}${CODEX_API_KEY}${CODEX_ACCESS_TOKEN}\"",
    ])?;
    container(&[
        "exec",
        name,
        ".venv/bin/python",
        "-c",
        "import sys; from fastapi.testclient import TestClient; from app import app; assert sys.version_info >= (3, 12); response = TestClient(app).get('/health'); assert response.status_code == 200 and response.json() == {'ok': True}; print('Independent health check passed on Python', sys.version.split()[0])",
    ])?;
    container(&["exec", name, "test", "!", "-e", canary.to_str().unwrap()])?;
    let output = root.join("result");
    fs::create_dir(&output)?;
    for file in ["app.py", "test_health.py", "pyproject.toml"] {
        container(&[
            "copy",
            &format!("{name}:/workspace/{file}"),
            output.join(file).to_str().unwrap(),
        ])?;
    }
    ensure(
        fs::read_to_string(output.join("pyproject.toml"))? == manifest,
        "Manifest unexpectedly changed",
    )?;
    ensure(
        fs::read_to_string(source.join("pyproject.toml"))? == manifest
            && fs::read_dir(&source)?.count() == 1,
        "Original source changed",
    )?;
    ensure(
        fs::read_to_string(&canary)? == "host only\n",
        "Host canary changed",
    )?;
    container(&["stop", name])?;
    ensure(
        rpc.call("environment/info", json!({"environmentId":"vm"}))
            .is_err(),
        "Stopped VM unexpectedly remained available",
    )?;
    ensure(
        rpc.call("environment/info", json!({"environmentId":"local"}))
            .is_err(),
        "Host fallback unexpectedly appeared",
    )?;
    println!(
        "PASS: runtime install, edits, independent Linux test, credential checks, unchanged source and disconnected executor."
    );
    Ok(())
}

fn ensure(ok: bool, message: &str) -> io::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(io::Error::other(message))
    }
}

fn container(args: &[&str]) -> io::Result<()> {
    let status = Command::new("container").args(args).status()?;
    ensure(status.success(), "Container command failed")
}

fn find_program(name: &str) -> io::Result<PathBuf> {
    env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
        .ok_or_else(|| io::Error::other(format!("Install {name} first")))
}

struct Rpc {
    child: Child,
    stdin: ChildStdin,
    incoming: Receiver<io::Result<Value>>,
    id: u64,
}

impl Rpc {
    fn start(cwd: &Path, home: &Path) -> io::Result<Self> {
        let mut command = Command::new("codex");
        command
            .args(["app-server", "--listen", "stdio://"])
            .current_dir(cwd)
            .env_clear()
            .env("CODEX_HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for key in ["PATH", "HOME", "USER", "LOGNAME", "LANG", "TMPDIR"] {
            if let Some(value) = env::var_os(key) {
                command.env(key, value);
            }
        }
        for option in [
            "forced_login_method=\"chatgpt\"",
            "approval_policy=\"never\"",
            "web_search=\"disabled\"",
            "shell_environment_policy.inherit=\"none\"",
            "shell_environment_policy.set.PATH=\"/usr/local/bin:/usr/bin:/bin\"",
            "shell_environment_policy.set.HOME=\"/root\"",
            "allow_login_shell=false",
            "features.apps=false",
            "features.plugins=false",
            "features.hooks=false",
            "features.multi_agent=false",
            "features.browser_use=false",
            "features.computer_use=false",
            "features.image_generation=false",
            "features.shell_snapshot=false",
        ] {
            command.args(["-c", option]);
        }
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, incoming) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let value = line.and_then(|s| serde_json::from_str(&s).map_err(io::Error::other));
                if tx.send(value).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            incoming,
            id: 0,
        })
    }

    fn write(&mut self, value: Value) -> io::Result<()> {
        serde_json::to_writer(&mut self.stdin, &value)?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()
    }

    fn next(&self, timeout: Duration) -> io::Result<Value> {
        self.incoming
            .recv_timeout(timeout)
            .map_err(io::Error::other)?
    }

    fn call(&mut self, method: &str, params: Value) -> io::Result<Value> {
        self.id += 1;
        let id = self.id;
        self.write(json!({"id":id,"method":method,"params":params}))?;
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            let message = self.next(deadline.saturating_duration_since(Instant::now()))?;
            if message["id"] == id && message.get("method").is_none() {
                if let Some(error) = message.get("error") {
                    return Err(io::Error::other(error.to_string()));
                }
                return Ok(message["result"].clone());
            }
        }
    }
}

impl Drop for Rpc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
