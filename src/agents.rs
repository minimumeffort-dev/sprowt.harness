use std::{
    env, io,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::{muse, rpc, sandbox};

pub fn detect() -> io::Result<bool> {
    detect_with(
        |name, args| {
            let mut command = Command::new(name);
            command
                .args(args)
                .current_dir(env::temp_dir())
                .env_clear()
                .envs(
                    [
                        "PATH",
                        "HOME",
                        "USER",
                        "LOGNAME",
                        "LANG",
                        "TMPDIR",
                        "CODEX_HOME",
                        "XDG_CONFIG_HOME",
                    ]
                    .iter()
                    .filter_map(|name| env::var_os(name).map(|value| (*name, value))),
                );
            output(command, Duration::from_secs(15))
        },
        muse::account_state,
    )
}

fn detect_with(
    mut probe: impl FnMut(&str, &[&str]) -> io::Result<Output>,
    account: impl FnOnce() -> io::Result<String>,
) -> io::Result<bool> {
    let version = probe("codex", &["--version"]).map_err(|_| {
        io::Error::other(format!(
            "Install Codex CLI {} before starting the harness.",
            sandbox::VERSION
        ))
    })?;
    if !version.status.success()
        || String::from_utf8_lossy(&version.stdout).trim()
            != format!("codex-cli {}", sandbox::VERSION)
    {
        return Err(io::Error::other(format!(
            "This harness needs Codex CLI {}.",
            sandbox::VERSION
        )));
    }
    let login = probe(
        "codex",
        &[
            "-c",
            "cli_auth_credentials_store=\"file\"",
            "login",
            "status",
        ],
    )
    .map_err(|_| {
        io::Error::other("Could not check Codex login. Try starting the harness again.")
    })?;
    if !login.status.success()
        || ![&login.stdout, &login.stderr]
            .iter()
            .any(|bytes| String::from_utf8_lossy(bytes).contains("Logged in using ChatGPT"))
    {
        return Err(io::Error::other(
            "Sign in first: codex -c 'cli_auth_credentials_store=\"file\"' login",
        ));
    }
    let version = match probe("muse", &["--version"]) {
        Ok(version) => version,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(_) => {
            return Err(io::Error::other(
                "Could not check Muse. Try starting the harness again.",
            ));
        }
    };
    if !version.status.success()
        || String::from_utf8_lossy(&version.stdout).trim()
            != format!(
                "Muse Code {} ({})",
                muse::VERSION.split('-').next().unwrap(),
                muse::VERSION
            )
    {
        return Err(io::Error::other(format!(
            "This harness needs Muse {}. Update and reinstall the harness after Muse updates.",
            muse::VERSION
        )));
    }
    if account()? != "accountLogin" {
        return Err(io::Error::other(
            "Sign in with your Muse account first: muse login",
        ));
    }
    Ok(true)
}

pub(crate) fn output(mut command: Command, timeout: Duration) -> io::Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output(),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            _ => {
                rpc::terminate(&mut child);
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Agent status check timed out.",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn reply(text: &str) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: text.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    fn installed(name: &str, args: &[&str]) -> io::Result<Output> {
        Ok(reply(match (name, args) {
            ("codex", ["--version"]) => "codex-cli 0.159.2\n",
            ("codex", _) => "Logged in using ChatGPT\n",
            ("muse", _) => "Muse Code 1.4.3 (1.4.3-R5018.1)\n",
            _ => unreachable!(),
        }))
    }

    #[test]
    fn accounts_are_detected_without_starting_workers_or_inference() {
        assert!(detect_with(installed, || Ok("accountLogin".into())).unwrap());
        assert!(
            !detect_with(
                |name, args| if name == "muse" {
                    Err(io::ErrorKind::NotFound.into())
                } else {
                    installed(name, args)
                },
                || panic!("Missing Muse must not start an account process")
            )
            .unwrap()
        );
    }

    #[test]
    fn installed_agents_with_broken_login_or_version_are_not_silently_skipped() {
        for state in ["notLoggedIn", "apiKey"] {
            assert!(
                detect_with(installed, || Ok(state.into()))
                    .unwrap_err()
                    .to_string()
                    .contains("muse login")
            );
        }
        assert!(
            detect_with(
                |name, args| if name == "muse" {
                    Ok(reply("muse incompatible"))
                } else {
                    installed(name, args)
                },
                || panic!("Unsupported Muse must not start")
            )
            .is_err()
        );
        let older = detect_with(
            |name, args| {
                if name == "muse" {
                    Ok(reply("Muse Code 1.4.1 (1.4.1-R4503.1)\n"))
                } else {
                    installed(name, args)
                }
            },
            || panic!("Unsupported Muse must not start"),
        )
        .unwrap_err();
        assert!(
            older
                .to_string()
                .contains("Update and reinstall the harness")
        );
        let error = detect_with(
            |name, args| {
                if name == "codex" && args != ["--version"] {
                    Ok(reply("Logged in using API key: synthetic-secret"))
                } else {
                    installed(name, args)
                }
            },
            || panic!("Codex account required"),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("login") && !error.to_string().contains("synthetic-secret")
        );
    }

    #[test]
    fn status_checks_have_a_deadline() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 5"]);
        let started = Instant::now();
        assert_eq!(
            output(command, Duration::from_millis(30))
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    #[ignore = "Checks the installed subscription CLIs without inference or VM startup"]
    fn installed_subscription_accounts_are_available() {
        assert!(detect().unwrap());
    }
}
