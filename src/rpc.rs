use std::{
    io::{self, BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

pub struct Rpc {
    stdin: ChildStdin,
    pub receiver: Receiver<io::Result<Value>>,
    next_id: u64,
    pub buffered: Vec<Value>,
    pub client_tools: bool,
    pub versioned: bool,
}

impl Rpc {
    pub fn start(command: &mut Command) -> io::Result<(Child, Self)> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
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
        Ok((
            child,
            Self {
                stdin,
                receiver,
                next_id: 0,
                buffered: Vec::new(),
                client_tools: false,
                versioned: false,
            },
        ))
    }

    pub fn write(&mut self, mut value: Value) -> io::Result<()> {
        if self.versioned {
            value["jsonrpc"] = json!("2.0");
        }
        serde_json::to_writer(&mut self.stdin, &value)?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()
    }

    pub fn call(&mut self, method: &str, params: Value) -> io::Result<Value> {
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
                        if error["data"]["delivery"] == "unknown" {
                            io::ErrorKind::Other
                        } else {
                            io::ErrorKind::InvalidInput
                        },
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

    pub fn receive(&mut self, message: Value) -> io::Result<()> {
        if message["method"] == "bridge/failed" {
            return Err(bridge_error(&message));
        }
        if message.get("method").is_some() && message.get("id").is_some() {
            if self.client_tools && message["method"] == "item/tool/call" {
                self.buffered.push(message);
            } else if message["method"] == "network/policyRequest" {
                self.write(json!({"id":message["id"],"result":{"decision":{"type":"deny","reason":"Domain is not in the harness allowlist."}}}))?;
            } else {
                self.write(json!({"id":message["id"],"error":{"code":-32601,"message":"This worker cannot grant broader permissions or run client tools."}}))?;
            }
        } else if message.get("method").is_some() {
            self.buffered.push(message);
        }
        Ok(())
    }
}

pub fn bridge_error(message: &Value) -> io::Error {
    let detail: String = message["params"]["message"]
        .as_str()
        .unwrap_or("Muse bridge stopped without diagnostic details.")
        .chars()
        .filter(|ch| !ch.is_control())
        .take(800)
        .collect();
    io::Error::other(format!("{detail} Work is retained."))
}

pub fn terminate(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_failure_preserves_cause_during_requests_without_rejecting_delivery() {
        let script = "import json,sys; json.loads(sys.stdin.readline()); print(json.dumps({'method':'bridge/failed','params':{'message':'Muse provider HTTP 400: invalid request.'}}),flush=True)";
        let (mut child, mut rpc) =
            Rpc::start(Command::new("python3").args(["-c", script])).unwrap();
        let error = rpc.call("turn/start", json!({})).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(error.to_string().contains("provider HTTP 400"));
        assert!(error.to_string().contains("Work is retained"));
        terminate(&mut child);
    }

    #[test]
    fn bridge_failure_is_bounded_without_terminal_control_characters() {
        let message =
            json!({"params":{"message":format!("guest exited\u{1b}\n{}", "x".repeat(2000))}});
        let text = bridge_error(&message).to_string();
        assert!(text.starts_with("guest exited"));
        assert!(text.chars().all(|ch| !ch.is_control()));
        assert!(text.len() < 850);
    }

    #[test]
    fn uncertain_bridge_errors_are_not_delivery_rejections() {
        let script = "import json,sys; request=json.loads(sys.stdin.readline()); print(json.dumps({'id':request['id'],'error':{'message':'connection lost','data':{'delivery':'unknown'}}}),flush=True)";
        let (mut child, mut rpc) =
            Rpc::start(Command::new("python3").args(["-c", script])).unwrap();
        let error = rpc.call("turn/start", json!({})).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        terminate(&mut child);
    }

    #[test]
    fn versioned_envelopes_are_opt_in() {
        let (mut child, mut rpc) = Rpc::start(&mut Command::new("/bin/cat")).unwrap();
        for versioned in [false, true] {
            rpc.versioned = versioned;
            rpc.write(json!({"id":1,"method":"initialize","params":{}}))
                .unwrap();
            let message = rpc
                .receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap();
            assert_eq!(
                message.get("jsonrpc"),
                versioned.then(|| json!("2.0")).as_ref()
            );
            assert_eq!(message["method"], "initialize");
        }
        terminate(&mut child);
    }

    #[test]
    fn only_enabled_client_tools_are_deferred_and_permissions_stay_denied() {
        let (mut child, mut rpc) = Rpc::start(&mut Command::new("/bin/cat")).unwrap();
        let request =
            json!({"id":100,"method":"item/tool/call","params":{"tool":"install_system_packages"}});
        rpc.receive(request.clone()).unwrap();
        let response = rpc
            .receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert_eq!(response["error"]["code"], -32601);
        rpc.client_tools = true;
        rpc.receive(request.clone()).unwrap();
        assert_eq!(rpc.buffered, [request]);
        for method in ["network/policyRequest", "item/permissions/requestApproval"] {
            rpc.receive(json!({"id":101,"method":method,"params":{}}))
                .unwrap();
            let response = rpc
                .receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap();
            if method == "network/policyRequest" {
                assert_eq!(response["result"]["decision"]["type"], "deny");
            } else {
                assert_eq!(response["error"]["code"], -32601);
            }
        }
        terminate(&mut child);
    }
}
