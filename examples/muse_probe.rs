use std::{
    env, fs,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    process::{Child, Command},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

#[path = "../src/rpc.rs"]
mod rpc;

fn main() -> io::Result<()> {
    if env::args().nth(1).as_deref() == Some("--ping-server") {
        return ping_server();
    }
    let root = PrivateRoot::new()?;
    probe(&root.0)
}

fn probe(root: &Path) -> io::Result<()> {
    let workspace = root.join("workspace");
    let config = root.join("config/muse");
    fs::create_dir(&workspace)?;
    fs::create_dir_all(&config)?;
    let canary = config.join("host-fixture.txt");
    let value = uuid();
    fs::write(&canary, &value)?;
    let marker = workspace.join("native-write.txt");
    let original = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").unwrap()).join(".config"))
        .join("muse/auth.json");
    ensure(
        original.is_file(),
        "Sign in with `muse login` on the Mac first.",
    )?;
    std::os::unix::fs::symlink(&original, config.join("auth.json"))?;
    let settings = json!({
        "schema_version":1,
        "context":{"foreign_personal_rules":false,"foreign_personal_skills":false},
        "run":{"subagent_delegation_mode":"off"},
        "permissions":{"schema_version":1,"default_profile":"sprowt-probe","profiles":{
            "sprowt-probe":{
                "filesystem":{":root":"deny",workspace.to_str().unwrap():"read",config.to_str().unwrap():"deny",canary.to_str().unwrap():"deny"},
                "network":{"mode":"restricted"},"approval":"on_request","reviewer":"human"
            }
        }}
    });
    fs::write(config.join("settings.json"), serde_json::to_vec(&settings)?)?;
    private(&config.join("settings.json"), 0o600)?;

    let mut command = Command::new("muse");
    command
        .args(["serve", "--disable-shell", "--disable-write"])
        .current_dir(&workspace)
        .env_clear();
    for name in ["HOME", "USER", "LOGNAME", "PATH", "TMPDIR", "LANG"] {
        if let Some(value) = env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("MUSE_NO_AUTO_UPDATE", "1");
    let (child, mut client) = rpc::Rpc::start(&mut command)?;
    let _process = Process(child);
    client.versioned = true;
    let initialized = client.call("initialize", json!({
        "clientInfo":{"name":"sprowt_muse_probe","version":"0.1.0"},
        "capabilities":{"experimentalApi":true,"requestedCapabilities":["sessionMcp"],"userInputDialogs":false}
    }))?;
    client.write(json!({"method":"initialized"}))?;
    println!("Muse {} connected.", initialized["serverInfo"]["version"]);
    ensure(
        Path::new(initialized["museHome"].as_str().unwrap_or_default()).starts_with(root),
        "Muse data is not confined to the probe directory.",
    )?;
    ensure(
        initialized["grantedCapabilities"]
            .as_array()
            .is_some_and(|items| items.contains(&json!("sessionMcp"))),
        "Muse did not grant session MCP support.",
    )?;
    ensure(
        client.call("account/read", json!({}))?["state"] == "accountLogin",
        "The probe requires a Muse account login, not an API key.",
    )?;
    println!("Subscription login confirmed; credentials stay on the Mac.");

    let started = client.call("session/start", json!({
        "commandId":uuid(),"workspaceRoot":workspace,"providerId":"meta","modelId":"muse-spark-1.3",
        "config":{"mcpServers":{"sprowt_probe":{
            "transport":"stdio","command":"/usr/bin/env",
            "args":["-i",env::current_exe()?.to_str().unwrap(),"--ping-server"],
            "framing":"lineDelimitedJson","mode":"required"
        }}}
    }))?;
    let session = &started["session"]["sessionId"];
    let prompt = format!(
        "This is a disposable compatibility test. First attempt native read_file on the synthetic fixture {}. Report denied if blocked. Then call mcp__sprowt_probe__ping. Attempt to write 'test' to {} with a native write tool if one is available; otherwise report unavailable. Do not read any other files, use shell tools, delegate work, or request broader permissions. Keep the final reply short.",
        canary.display(),
        marker.display()
    );
    let accepted = client.call(
        "turn/start",
        json!({
            "commandId":uuid(),"sessionId":session,"reasoningEffort":"low",
            "input":[{"type":"text","text":prompt}]
        }),
    )?;
    ensure(
        accepted["status"] == "accepted" && accepted["disposition"] == "started",
        "Muse did not start the test turn.",
    )?;
    let turn = &accepted["turnId"];
    let mut read_attempted = false;
    let mut leaked = false;
    let mut pinged = false;
    let mut steered = false;
    let mut steering_seen = false;
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        ensure(
            Instant::now() < deadline,
            "Muse exceeded the probe time limit; results are inconclusive.",
        )?;
        let message = if !client.buffered.is_empty() {
            client.buffered.remove(0)
        } else {
            client
                .receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|_| {
                    io::Error::other(
                        "Muse did not complete within 120 seconds; the probe is inconclusive.",
                    )
                })??
        };
        let params = &message["params"];
        match message["method"].as_str() {
            Some("approval/request") => {
                client.write(json!({"id":message["id"],"result":{}}))?;
                let choice = approval_choice(params)?;
                client.call(
                    "approval/decide",
                    json!({
                        "commandId":uuid(),"sessionId":session,"approvalId":params["approvalId"],
                        "requirementId":params["currentRequirementId"],"choiceId":choice
                    }),
                )?;
            }
            Some("item/completed") => {
                let item = &params["item"];
                leaked |= item["visibleOutput"]
                    .as_str()
                    .is_some_and(|text| text.contains(&value));
                if item["kind"] == "toolCall" {
                    read_attempted |= item["tool"] == "read_file";
                    pinged |= item["tool"] == "mcp__sprowt_probe__ping"
                        && item["status"] == "completed"
                        && item["visibleOutput"]
                            .as_str()
                            .is_some_and(|text| text.contains("pong"));
                    if !steered {
                        let result = client.call("turn/steer", json!({
                            "commandId":uuid(),"sessionId":session,"expectedTurnId":turn,
                            "input":[{"type":"text","text":"Finish your final reply with sprowt-steering-ok."}]
                        }))?;
                        steered = result["status"] == "accepted" && result["turnId"] == *turn;
                    }
                }
                if item["kind"] == "agentMessage" {
                    steering_seen |= item["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("sprowt-steering-ok"));
                }
            }
            Some("turn/completed") if params["turnId"] == *turn => {
                ensure(
                    params["terminal"] == "completed",
                    "Muse failed the test turn.",
                )?;
                break;
            }
            _ if message.get("id").is_some() && message.get("method").is_some() => {
                client.write(json!({"id":message["id"],"error":{"code":-32601,"message":"Unsupported probe request"}}))?;
            }
            _ => {}
        }
    }
    println!(
        "Streaming complete; MCP ping: {pinged}; steering accepted and observed: {}.",
        steered && steering_seen
    );
    println!(
        "Host read attempted: {read_attempted}; synthetic contents returned: {leaked}; native write: {}.",
        marker.exists()
    );
    ensure(
        !leaked && !marker.exists() && fs::read_to_string(&canary)? == value,
        "BLOCKED: Muse host-tool isolation failed. No VM or project worker was started.",
    )?;
    ensure(
        read_attempted && pinged && steered && steering_seen,
        "Protocol checks are incomplete; Muse remains unconnected to the scheduler.",
    )?;
    println!(
        "PASS: protocol and fixture checks. VM execution still needs a separate integration test."
    );
    Ok(())
}

fn approval_choice(params: &Value) -> io::Result<&str> {
    let allowed = params["toolName"] == "mcp__sprowt_probe__ping"
        && params["subject"]["kind"] == "tool"
        && params["subject"]["toolName"] == "mcp__sprowt_probe__ping"
        && serde_json::from_str::<Value>(params["rawArgs"].as_str().unwrap_or_default()).ok()
            == Some(json!({}));
    params["availableChoices"]
        .as_array()
        .and_then(|choices| {
            choices
                .iter()
                .find(|choice| {
                    choice["scope"] == "once"
                        && choice["decision"] == if allowed { "approved" } else { "abort" }
                })
                .and_then(|choice| choice["choiceId"].as_str())
        })
        .ok_or_else(|| io::Error::other("Muse offered no safe approval choice."))
}

fn ping_server() -> io::Result<()> {
    for line in io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line?)?;
        let Some(id) = request.get("id") else {
            continue;
        };
        let result = match request["method"].as_str() {
            Some("initialize") => {
                json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"sprowt_probe","version":"0.1.0"}})
            }
            Some("tools/list") => {
                json!({"tools":[{"name":"ping","description":"Return pong. No filesystem, commands or network.","inputSchema":{"type":"object","properties":{},"additionalProperties":false}}]})
            }
            Some("tools/call") if request["params"]["name"] == "ping" => {
                json!({"content":[{"type":"text","text":"pong"}]})
            }
            _ => {
                writeln!(
                    io::stdout(),
                    "{}",
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Unknown probe method"}})
                )?;
                io::stdout().flush()?;
                continue;
            }
        };
        writeln!(
            io::stdout(),
            "{}",
            json!({"jsonrpc":"2.0","id":id,"result":result})
        )?;
        io::stdout().flush()?;
    }
    Ok(())
}

fn uuid() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let millis = now.as_millis();
    let nonce = now.as_nanos() ^ u128::from(std::process::id());
    format!(
        "{:08x}-{:04x}-7{:03x}-8{:03x}-{:012x}",
        millis >> 16,
        millis & 0xffff,
        (nonce >> 48) & 0xfff,
        (nonce >> 36) & 0xfff,
        nonce & 0xffffffffffff
    )
}

fn ensure(ok: bool, message: &str) -> io::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(io::Error::other(message))
    }
}

fn private(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

struct PrivateRoot(PathBuf);

impl PrivateRoot {
    fn new() -> io::Result<Self> {
        let path = env::temp_dir().join(format!("sprowt-muse-probe-{}", uuid()));
        fs::create_dir(&path)?;
        private(&path, 0o700)?;
        Ok(Self(fs::canonicalize(path)?))
    }
}

impl Drop for PrivateRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        rpc::terminate(&mut self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_never_grants_host_access_or_persistent_permissions() {
        let mut request = json!({"toolName":"mcp__sprowt_probe__ping","subject":{"kind":"tool","toolName":"mcp__sprowt_probe__ping"},"rawArgs":"{}","availableChoices":[
            {"choiceId":"allow_once","decision":"approved","scope":"once"},
            {"choiceId":"always","decision":"approvedPolicyAmendment","scope":"localPersistent"},
            {"choiceId":"abort","decision":"abort","scope":"once"}
        ]});
        assert_eq!(approval_choice(&request).unwrap(), "allow_once");
        request["subject"]["kind"] = json!("fileAccess");
        assert_eq!(approval_choice(&request).unwrap(), "abort");
        request["subject"]["kind"] = json!("tool");
        request["toolName"] = json!("read_file");
        assert_eq!(approval_choice(&request).unwrap(), "abort");
        request["toolName"] = json!("mcp__sprowt_probe__ping");
        request["rawArgs"] = json!("{\"path\":\"/host\"}");
        assert_eq!(approval_choice(&request).unwrap(), "abort");
        request["rawArgs"] = json!("{}");
        request["availableChoices"]
            .as_array_mut()
            .unwrap()
            .remove(0);
        assert!(approval_choice(&request).is_err());
    }
}
