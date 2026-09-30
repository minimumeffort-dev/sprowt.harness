use std::{
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

use directories::ProjectDirs;
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct Selection {
    pub model: String,
    pub effort: String,
    pub reason: String,
}

impl Selection {
    pub fn fallback(reason: &str) -> Self {
        Self {
            model: "gpt-6.1-sol".into(),
            effort: "high".into(),
            reason: reason.into(),
        }
    }

    fn from_answer(answer: Value) -> Self {
        let choice = answer["choice"].as_str().unwrap_or("");
        let confidence = answer["confidence"].as_f64().unwrap_or(0.0);
        if !["simple", "complex", "demanding"].contains(&choice)
            || !(0.0..=1.0).contains(&confidence)
        {
            return Self::fallback("Laya unavailable · Sol high fallback");
        }
        // This threshold is a conservative routing heuristic, not calibrated certainty.
        if confidence < 0.70 {
            return Self::fallback("Laya uncertain · Sol high fallback");
        }
        Self {
            model: if choice == "demanding" {
                "gpt-6-astra"
            } else {
                "gpt-6.1-sol"
            }
            .into(),
            effort: if choice == "simple" { "medium" } else { "high" }.into(),
            reason: format!("Laya · {choice} · score {confidence:.2}"),
        }
    }
}

type Job = (String, Sender<Selection>);

pub struct Router {
    requests: Sender<Job>,
    child: Child,
    task: Option<thread::JoinHandle<()>>,
}

impl Router {
    pub fn start() -> io::Result<Self> {
        let mut child = command(true)?.spawn()?;
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let (requests, jobs) = mpsc::channel::<Job>();
        let task = thread::spawn(move || {
            while let Ok((state, reply)) = jobs.recv() {
                let selection = (|| -> io::Result<Selection> {
                    serde_json::to_writer(&mut stdin, &json!({"state":state}))?;
                    stdin.write_all(b"\n")?;
                    stdin.flush()?;
                    let mut line = String::new();
                    if stdout.read_line(&mut line)? == 0 {
                        return Err(io::Error::other("Laya stopped."));
                    }
                    let answer = serde_json::from_str(&line)?;
                    Ok(Selection::from_answer(answer))
                })()
                .unwrap_or_else(|_| Selection::fallback("Laya unavailable · Sol high fallback"));
                let _ = reply.send(selection);
            }
        });
        Ok(Self {
            requests,
            child,
            task: Some(task),
        })
    }

    pub fn request(&self, state: String) -> Receiver<Selection> {
        let (reply, receiver) = mpsc::channel();
        let _ = self.requests.send((state, reply));
        receiver
    }
}

impl Drop for Router {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Drop the job sender before joining the actor.
        let (sender, _) = mpsc::channel();
        drop(std::mem::replace(&mut self.requests, sender));
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

pub fn directory() -> io::Result<PathBuf> {
    ProjectDirs::from("", "", "sprowt-harness")
        .map(|dirs| dirs.data_local_dir().to_owned())
        .ok_or_else(|| io::Error::other("Cannot locate the local data directory."))
}

fn command(offline: bool) -> io::Result<Command> {
    let data = directory()?;
    let python = data.join("laya-runtime/bin/python");
    if !python.is_file() {
        return Err(io::Error::other(
            "Run `sprowt-harness setup` to install Laya.",
        ));
    }
    let mut command = Command::new(python);
    command
        .args(["-I", "-u", "-c", include_str!("laya.py")])
        .current_dir(&data)
        .env_clear()
        .env("HOME", &data)
        .env("PATH", "/usr/bin:/bin")
        .env("HF_HOME", data.join("laya-models"))
        .env("HF_HUB_DISABLE_TELEMETRY", "1")
        .env("USE_TF", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if offline {
        command.env("HF_HUB_OFFLINE", "1");
    }
    Ok(command)
}

pub fn setup() -> io::Result<()> {
    let runtime = directory()?.join("laya-runtime");
    for mut command in [
        {
            let mut c = Command::new("uv");
            c.args([
                "venv",
                "--no-config",
                "--allow-existing",
                "--python",
                "3.13",
            ])
            .arg(&runtime);
            c
        },
        {
            let mut c = Command::new("uv");
            c.args(["pip", "install", "--no-config", "--python"])
                .arg(runtime.join("bin/python"))
                .arg("laya==0.3.20");
            c
        },
    ] {
        if !command.status()?.success() {
            return Err(io::Error::other("Laya setup failed."));
        }
    }
    println!("Downloading Laya's local model…");
    let mut child = command(false)?.stderr(Stdio::inherit()).spawn()?;
    let mut input = child.stdin.take().unwrap();
    input.write_all(b"{\"state\":\"Fix a typo in a README.\"}\n")?;
    drop(input);
    let output = child.wait_with_output()?;
    let answer: Value = serde_json::from_slice(&output.stdout)?;
    if !output.status.success() || answer.get("choice").is_none() {
        return Err(io::Error::other("Laya model setup failed."));
    }
    println!("Laya is ready. Routing uses the downloaded model offline.");
    Ok(())
}

pub fn context(project: &Path, description: &str) -> String {
    let read = |name: &str| -> String {
        let path = project.join(name);
        if !path
            .canonicalize()
            .is_ok_and(|path| path.starts_with(project))
        {
            return String::new();
        }
        let mut text = String::new();
        if let Ok(file) = fs::File::open(path) {
            let _ = file.take(1500).read_to_string(&mut text);
        }
        text
    };
    let mut entries = fs::read_dir(project)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect::<Vec<_>>();
    entries.sort();
    entries.truncate(40);
    format!(
        "Request: {}\nProject entries: {}\nREADME: {}\nProject rules: {}",
        description.chars().take(6000).collect::<String>(),
        entries.join(", "),
        read("README.md"),
        read("AGENTS.md")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routes_choices_and_uncertainty_without_trusting_invalid_scores() {
        let simple = Selection::from_answer(json!({"choice":"simple", "confidence":0.9}));
        assert_eq!(
            (simple.model.as_str(), simple.effort.as_str()),
            ("gpt-6.1-sol", "medium")
        );
        assert_eq!(
            Selection::from_answer(json!({"choice":"demanding", "confidence":0.8})).model,
            "gpt-6-astra"
        );
        assert_eq!(
            Selection::from_answer(json!({"choice":"simple", "confidence":0.4})).effort,
            "high"
        );
        assert_eq!(
            Selection::from_answer(json!({"choice":"simple", "confidence":2.0})).effort,
            "high"
        );
        assert_eq!(
            Selection::from_answer(json!({"error":"failed"})).effort,
            "high"
        );
    }
}
