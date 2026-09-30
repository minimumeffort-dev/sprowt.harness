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
            },
        ))
    }

    pub fn write(&mut self, value: Value) -> io::Result<()> {
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

    pub fn receive(&mut self, message: Value) -> io::Result<()> {
        if message.get("method").is_some() && message.get("id").is_some() {
            if message["method"] == "network/policyRequest" {
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

pub fn terminate(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}
