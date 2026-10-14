use crate::engine::api::{Command, Request};
use crate::session::Session;
use std::{
    io::{self, BufRead, Read, Write},
    path::PathBuf,
    sync::mpsc,
    time::Duration,
};

const MAX_COMMAND_BYTES: u64 = 1024 * 1024;

pub fn run(
    project: PathBuf,
    data_dir: Option<PathBuf>,
    closed_worktree_days: u32,
) -> io::Result<()> {
    let mut engine = Session::new(project, data_dir.as_deref())?;
    engine
        .prune_closed(closed_worktree_days)
        .map_err(io::Error::other)?;
    let (sender, receiver) = mpsc::sync_channel(32);
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut input = stdin.lock();
        loop {
            let mut line = String::new();
            let read = input
                .by_ref()
                .take(MAX_COMMAND_BYTES + 1)
                .read_line(&mut line);
            match read {
                Ok(0) => break,
                Ok(_) if line.len() as u64 > MAX_COMMAND_BYTES => {
                    let _ = sender.send(Err(io::Error::other("Desktop command exceeds 1 MiB.")));
                    break;
                }
                Ok(_) => {
                    if sender.send(Ok(line)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = sender.send(Err(error));
                    break;
                }
            }
        }
    });
    serve(&mut engine, receiver, &mut io::stdout().lock())
}

fn serve(
    engine: &mut Session,
    receiver: mpsc::Receiver<io::Result<String>>,
    output: &mut impl Write,
) -> io::Result<()> {
    let mut last = serde_json::to_vec(&engine.response(None, None))?;
    write_line(output, &last)?;
    loop {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(line)) => {
                let (request_id, error, shutdown) = match serde_json::from_str::<Request>(&line) {
                    Ok(request) => {
                        let shutdown = matches!(request.command, Command::Shutdown {});
                        (
                            Some(request.id),
                            engine.command(request.command).err(),
                            shutdown,
                        )
                    }
                    Err(error) => {
                        let id = serde_json::from_str::<serde_json::Value>(&line)
                            .ok()
                            .and_then(|v| v.get("id").and_then(serde_json::Value::as_u64));
                        let id = id.ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("Invalid desktop request without an id: {error}"),
                            )
                        })?;
                        (
                            Some(id),
                            Some(format!("Invalid desktop command: {error}")),
                            false,
                        )
                    }
                };
                write_line(
                    output,
                    &serde_json::to_vec(&engine.response(request_id, error))?,
                )?;
                if shutdown {
                    return Ok(());
                }
            }
            Ok(Err(error)) => return Err(error),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        engine.poll_workers().map_err(io::Error::other)?;
        let next = serde_json::to_vec(&engine.response(None, None))?;
        if next != last {
            write_line(output, &next)?;
            last = next;
        }
    }
}

fn write_line(output: &mut impl Write, line: &[u8]) -> io::Result<()> {
    output.write_all(line)?;
    output.write_all(b"\n")?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::test_support::TestData;

    #[test]
    fn malformed_commands_with_ids_are_acknowledged_and_next_request_runs() {
        let data = TestData::new();
        let project = data.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let mut engine =
            Session::from_engine(crate::engine::Engine::load(project, data.store()).unwrap())
                .unwrap();
        let (sender, receiver) = mpsc::channel();
        for line in [
            r#"{"id":1,"command":{"type":"not_a_command"}}"#,
            r#"{"id":2,"command":{"type":"snapshot","unknown":true}}"#,
            r#"{"id":3,"command":{"type":"snapshot"}}"#,
            r#"{"id":4,"command":{"type":"shutdown"}}"#,
        ] {
            sender.send(Ok(line.into())).unwrap();
        }
        let mut output = Vec::new();
        serve(&mut engine, receiver, &mut output).unwrap();
        let frames = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(frames[0]["protocol_version"], 1);
        assert!(frames[0]["request_id"].is_null());
        let replies = frames
            .iter()
            .filter(|f| !f["request_id"].is_null())
            .collect::<Vec<_>>();
        assert_eq!(
            replies
                .iter()
                .map(|f| f["request_id"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        assert!(
            replies[0]["error"]
                .as_str()
                .unwrap()
                .contains("unknown variant")
        );
        assert!(
            replies[1]["error"]
                .as_str()
                .unwrap()
                .contains("unknown field")
        );
        assert!(replies[2]["error"].is_null());
        assert_eq!(replies[2]["snapshot"]["status"], "New codemod");
    }

    #[test]
    fn stdin_eof_exits_cleanly_after_the_initial_state() {
        let data = TestData::new();
        let project = data.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let mut engine =
            Session::from_engine(crate::engine::Engine::load(project, data.store()).unwrap())
                .unwrap();
        let (sender, receiver) = mpsc::channel();
        drop(sender);
        let mut output = Vec::new();
        serve(&mut engine, receiver, &mut output).unwrap();
        let lines = String::from_utf8(output).unwrap();
        assert_eq!(lines.lines().count(), 1);
        let first: serde_json::Value = serde_json::from_str(lines.trim()).unwrap();
        assert_eq!(first["snapshot"]["status"], "New codemod");
    }
    #[test]
    fn desktop_switch_polls_retained_worker_events_and_returns_to_the_same_session() {
        use crate::{codex::Event, engine::Engine, worker::Worker};
        let data = TestData::new();
        let mut store = data.store();
        let mut ids = Vec::new();
        for name in ["first", "second"] {
            let path = data.0.join(name);
            std::fs::create_dir_all(&path).unwrap();
            let path = path.canonicalize().unwrap();
            let project = store.load_project(&path).unwrap().id;
            let codemod = store.create_mod(project, name).unwrap().id;
            ids.push((path, project, codemod));
        }
        let mut session =
            Session::from_engine(Engine::load(ids[0].0.clone(), store).unwrap()).unwrap();
        let record = session.store.worker(ids[0].2).unwrap();
        let worker_id = record.id;
        let (worker, events, actions) =
            Worker::recording(&session.project, session.current_mod().unwrap(), record).unwrap();
        session.workers.insert(worker_id, worker);
        events
            .send(Event::Ready {
                thread: serde_json::json!({"id":"desktop-background-session"}),
                model: Some("retained-model".into()),
                effort: None,
            })
            .unwrap();
        let (sender, receiver) = mpsc::channel();
        for (id, command) in [
            (
                1,
                serde_json::json!({"type":"select_mod", "mod_id":ids[1].2}),
            ),
            (
                2,
                serde_json::json!({"type":"select_project", "project_id":ids[0].1}),
            ),
            (3, serde_json::json!({"type":"shutdown"})),
        ] {
            sender
                .send(Ok(
                    serde_json::json!({"id":id,"command":command}).to_string()
                ))
                .unwrap();
        }
        let mut output = Vec::new();
        serve(&mut session, receiver, &mut output).unwrap();
        let frames: Vec<serde_json::Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let switched = frames.iter().find(|f| f["request_id"] == 1).unwrap();
        assert!(switched["error"].is_null());
        assert_eq!(switched["snapshot"]["project_id"], ids[1].1);
        let returned = frames.iter().find(|f| f["request_id"] == 2).unwrap();
        assert!(returned["error"].is_null());
        assert_eq!(
            returned["snapshot"]["active_mod"]["worker_details"][0]["id"],
            worker_id
        );
        assert_eq!(
            session.workers[&worker_id].model.as_deref(),
            Some("retained-model")
        );
        assert_eq!(
            session.store.worker(ids[0].2).unwrap().thread_id.as_deref(),
            Some("desktop-background-session")
        );
        assert!(actions.try_recv().is_err());
    }
}
