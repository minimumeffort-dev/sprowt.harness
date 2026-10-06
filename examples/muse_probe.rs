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
    match env::args().nth(1).as_deref() {
        Some("--ping-server") => return ping_server(),
        Some("--tool-guard") => {
            let audit = env::args()
                .nth(2)
                .ok_or_else(|| io::Error::other("Missing guard audit path."))?;
            return tool_guard(Path::new(&audit));
        }
        Some("--guard-failure") => {
            if let Some(path) = env::args().nth(2) {
                audit_record(Path::new(&path), json!({"failed":true}))?;
            }
            return Err(io::Error::other("Synthetic guard failure."));
        }
        Some("--no-native-tools") => {
            let root = PrivateRoot::new()?;
            let result = probe(&root.0, Boundary::Empty)?;
            return result.require_boundary();
        }
        None => {}
        _ => return Err(io::Error::other("Unknown probe option.")),
    }
    let root = PrivateRoot::new()?;
    let guarded = probe(&root.0, Boundary::Guard)?;
    guarded.require_boundary()?;
    let root = PrivateRoot::new()?;
    let failed = probe(&root.0, Boundary::FailedGuard)?;
    ensure(failed.guard_failed, "The guard failure test did not run.")?;
    failed.require_boundary()?;
    println!("PASS: guarded and failed-guard checks. VM execution remains unverified.");
    Ok(())
}

#[derive(Clone, Copy)]
enum Boundary {
    Guard,
    FailedGuard,
    Empty,
}

impl Boundary {
    fn label(self) -> &'static str {
        match self {
            Self::Guard => "MCP-only hook",
            Self::FailedGuard => "failed MCP-only hook",
            Self::Empty => "empty native toolset",
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Observed {
    read_attempted: bool,
    read_denied: bool,
    leaked: bool,
    wrote: bool,
    native_completed: bool,
    guard_failed: bool,
    pinged: bool,
    steered: bool,
    steering_seen: bool,
}

impl Observed {
    fn require_boundary(&self) -> io::Result<()> {
        ensure(
            !self.leaked && !self.wrote && !self.native_completed,
            "BLOCKED: Muse host-tool isolation failed. No VM or project worker was started.",
        )?;
        ensure(
            self.read_attempted
                && self.read_denied
                && self.pinged
                && self.steered
                && self.steering_seen,
            "BLOCKED: native-tool denial and working MCP delivery were not both verified.",
        )
    }
}

fn probe(root: &Path, boundary: Boundary) -> io::Result<Observed> {
    println!("Checking {}.", boundary.label());
    let workspace = root.join("workspace");
    let config = root.join("config/muse");
    fs::create_dir(&workspace)?;
    fs::create_dir_all(&config)?;
    let canary = config.join("host-fixture.txt");
    let value = uuid();
    fs::write(&canary, &value)?;
    let marker = workspace.join("native-write.txt");
    let audit = config.join("guard.jsonl");
    let original = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").unwrap()).join(".config"))
        .join("muse/auth.json");
    ensure(
        original.is_file(),
        "Sign in with `muse login` on the Mac first.",
    )?;
    std::os::unix::fs::symlink(&original, config.join("auth.json"))?;
    let mut settings = json!({
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
    for name in [
        "memory",
        "skill-reminder",
        "todo-reminder",
        "goal-reminder",
        "verify-reminder",
        "scope-reminder",
    ] {
        settings["runtime_capabilities"][format!("plugin:tbh-reminders:reminder:{name}")] =
            json!({"enabled":false});
    }
    if let Boundary::Empty = boundary {
        settings["run"]["toolset"] = json!([]);
    } else {
        let mode = match boundary {
            Boundary::Guard => "--tool-guard",
            Boundary::FailedGuard => "--guard-failure",
            Boundary::Empty => unreachable!(),
        };
        let command = format!(
            "{} {mode} {}",
            shell_quote(env::current_exe()?.to_str().unwrap()),
            shell_quote(audit.to_str().unwrap())
        );
        settings["hooks"] =
            json!({"PreToolUse":[{"hooks":[{"type":"command","command":command,"timeout":5}]}]});
    }
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
    let steering = client.call(
        "turn/steer",
        json!({
            "commandId":uuid(),"sessionId":session,"expectedTurnId":turn,
            "input":[{"type":"text","text":"Finish your final reply with sprowt-steering-ok."}]
        }),
    )?;
    let mut observed = Observed {
        steered: steering["status"] == "accepted" && steering["turnId"] == *turn,
        ..Observed::default()
    };
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
                observed.leaked |= item["visibleOutput"]
                    .as_str()
                    .is_some_and(|text| text.contains(&value));
                if item["kind"] == "toolCall" {
                    observed.native_completed |=
                        item["status"] == "completed" && item["tool"] != "mcp__sprowt_probe__ping";
                    if item["tool"] == "read_file" {
                        observed.read_attempted = true;
                        observed.read_denied |= item["status"] == "failed"
                            && item["visibleOutput"]
                                .as_str()
                                .is_some_and(|text| text.contains("sprowt-tool-denied"));
                    }
                    observed.pinged |= item["tool"] == "mcp__sprowt_probe__ping"
                        && item["status"] == "completed"
                        && item["visibleOutput"]
                            .as_str()
                            .is_some_and(|text| text.contains("pong"));
                }
                if item["kind"] == "agentMessage" {
                    observed.leaked |= item["text"]
                        .as_str()
                        .is_some_and(|text| text.contains(&value));
                    observed.steering_seen |= item["text"]
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
    if let Ok(records) = fs::read_to_string(&audit) {
        for line in records.lines() {
            let record: Value = serde_json::from_str(line)?;
            observed.guard_failed |= record["failed"] == true;
            if record["tool"] == "read_file" && record["denied"] == true {
                observed.read_attempted = true;
                observed.read_denied = true;
            }
        }
    }
    println!(
        "Streaming complete; MCP ping: {}; steering accepted and observed: {}.",
        observed.pinged,
        observed.steered && observed.steering_seen
    );
    observed.wrote = marker.exists();
    println!(
        "Host read attempted: {}; guard denial: {}; synthetic contents returned: {}; native write: {}; native tool completed: {}; guard failure: {}.",
        observed.read_attempted,
        observed.read_denied,
        observed.leaked,
        observed.wrote,
        observed.native_completed,
        observed.guard_failed
    );
    ensure(
        fs::read_to_string(&canary)? == value,
        "BLOCKED: Muse changed the synthetic host fixture.",
    )?;
    Ok(observed)
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn tool_guard(audit: &Path) -> io::Result<()> {
    let input: Value = serde_json::from_reader(io::stdin().lock())?;
    let output = guard_decision(&input);
    audit_record(
        audit,
        json!({"tool":input["tool_name"],"denied":output["hookSpecificOutput"]["permissionDecision"]=="deny"}),
    )?;
    serde_json::to_writer(io::stdout().lock(), &output)?;
    Ok(())
}

fn audit_record(path: &Path, value: Value) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{value}")
}

fn guard_decision(input: &Value) -> Value {
    if input["tool_name"] == "mcp__sprowt_probe__ping" && input["tool_input"] == json!({}) {
        json!({})
    } else {
        json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"sprowt-tool-denied"}})
    }
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
    fn guard_only_admits_the_exact_ping_call() {
        assert_eq!(
            guard_decision(&json!({"tool_name":"mcp__sprowt_probe__ping","tool_input":{}})),
            json!({})
        );
        for name in [
            "read_file",
            "search",
            "write_file",
            "edit_file",
            "bash",
            "code_exec",
            "workflow",
            "subagent_spawn",
            "mcp__other__ping",
        ] {
            assert_eq!(
                guard_decision(&json!({"tool_name":name,"tool_input":{}}))["hookSpecificOutput"]["permissionDecision"],
                "deny"
            );
        }
        for input in [
            json!({}),
            json!({"tool_name":"mcp__sprowt_probe__ping","tool_input":{"path":"/host"}}),
        ] {
            assert_eq!(
                guard_decision(&input)["hookSpecificOutput"]["permissionDecision"],
                "deny"
            );
        }
    }

    #[test]
    fn boundary_needs_denial_and_mcp_evidence_without_host_access() {
        let observed = Observed {
            read_attempted: true,
            read_denied: true,
            pinged: true,
            steered: true,
            steering_seen: true,
            ..Observed::default()
        };
        assert!(observed.require_boundary().is_ok());
        for invalid in [
            Observed {
                leaked: true,
                ..observed
            },
            Observed {
                wrote: true,
                ..observed
            },
            Observed {
                native_completed: true,
                ..observed
            },
            Observed {
                read_attempted: false,
                ..observed
            },
            Observed {
                read_denied: false,
                ..observed
            },
            Observed {
                pinged: false,
                ..observed
            },
            Observed {
                steered: false,
                ..observed
            },
            Observed {
                steering_seen: false,
                ..observed
            },
        ] {
            assert!(invalid.require_boundary().is_err());
        }
    }

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
