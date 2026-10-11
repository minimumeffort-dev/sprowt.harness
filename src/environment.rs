use crate::{
    execution::{Check, CheckResult},
    sandbox::Sandbox,
    workspace::{self, Snapshot},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs, io,
    path::{Component, Path, PathBuf},
    sync::atomic::AtomicBool,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub inputs: Vec<String>,
    #[serde(default)]
    pub system_packages: Vec<String>,
    pub setup: Vec<Command>,
    pub capabilities: Vec<Command>,
    pub baseline: Vec<Command>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub name: String,
    pub argv: Vec<String>,
    pub timeout_seconds: u64,
}

impl Recipe {
    pub fn validate(&self) -> Result<(), String> {
        if self.inputs.len() > 32
            || self.capabilities.is_empty()
            || self.setup.len() > 12
            || self.capabilities.len() > 12
            || self.baseline.len() > 12
        {
            return Err("Environment recipe needs 1–12 capability probes, at most 12 setup/baseline commands and 32 inputs.".into());
        }
        if !self.system_packages.is_empty() {
            self.packages().validate().map_err(|e| e.to_string())?;
        }
        for path in &self.inputs {
            if path.is_empty()
                || Path::new(path)
                    .components()
                    .any(|c| !matches!(c, Component::Normal(_)))
            {
                return Err("Environment inputs must be exact project-relative paths.".into());
            }
        }
        for command in self
            .setup
            .iter()
            .chain(&self.capabilities)
            .chain(&self.baseline)
        {
            if command.name.trim().is_empty()
                || command.name.len() > 160
                || !(1..=180).contains(&command.timeout_seconds)
                || command.argv.is_empty()
                || !Path::new(&command.argv[0]).is_absolute()
                || command.argv.iter().any(|a| {
                    a.contains('\0')
                        || a.contains("/tasks/")
                        || a.contains("/home/sprowt/workers/")
                        || a.len() > 16000
                })
            {
                return Err("Environment commands need a name, absolute executable, portable arguments and a 1–180 second budget; temporary worker paths are not reusable.".into());
            }
        }
        Ok(())
    }
    fn packages(&self) -> crate::packages::Request {
        crate::packages::Request {
            packages: self.system_packages.clone(),
            reason: "Project environment recipe".into(),
        }
    }
    fn key(&self, source: &Snapshot) -> io::Result<String> {
        let mut inputs: Snapshot = source
            .iter()
            .filter(|(p, _, _)| {
                let file = p.file_name().unwrap_or_default().to_string_lossy();
                p == Path::new(".sprowt/environment.json")
                    || self.inputs.iter().any(|i| p == Path::new(i))
                    || file.ends_with(".lock")
                    || file.contains("lock.")
                    || matches!(
                        file.as_ref(),
                        "package.json"
                            | "pyproject.toml"
                            | "Cargo.toml"
                            | "mise.toml"
                            | ".mise.toml"
                            | ".tool-versions"
                            | ".python-version"
                            | ".node-version"
                            | "requirements.txt"
                            | "Dockerfile"
                    )
            })
            .cloned()
            .collect();
        inputs.push((PathBuf::from("@recipe"), serde_json::to_vec(self)?, 0));
        inputs.push((
            PathBuf::from("@platform"),
            format!("linux:{}", crate::sandbox::VERSION).into_bytes(),
            0,
        ));
        workspace::fingerprint(&inputs)
    }
}

pub fn schema() -> Value {
    let command = json!({"type":"object","additionalProperties":false,"properties":{
        "name":{"type":"string"},"argv":{"type":"array","items":{"type":"string"}},
        "timeout_seconds":{"type":"integer","minimum":1,"maximum":180}},"required":["name","argv","timeout_seconds"]});
    json!({"type":["object","null"],"additionalProperties":false,"properties":{
        "inputs":{"type":"array","items":{"type":"string"}},
        "system_packages":{"type":"array","items":{"type":"string"}},
        "setup":{"type":"array","items":command},"capabilities":{"type":"array","items":command},"baseline":{"type":"array","items":command}},
        "required":["inputs","system_packages","setup","capabilities","baseline"]})
}

#[derive(Serialize, Deserialize)]
pub struct Receipt {
    pub recipe: Recipe,
    #[serde(default)]
    pub error: Option<String>,
    pub environment: String,
    pub source: String,
    pub setup: Vec<CheckResult>,
    pub capabilities: Vec<CheckResult>,
    pub baseline: Vec<CheckResult>,
}

pub fn load(root: &Path, id: i64) -> io::Result<Option<Receipt>> {
    match fs::read(root.join("environments").join(format!("{id}.json"))) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

impl Receipt {
    pub fn summary(&self) -> String {
        let passed = self
            .baseline
            .iter()
            .filter(|r| r.exit_code == Some(0))
            .count();
        let failures = self
            .baseline
            .iter()
            .filter(|r| r.failed())
            .map(|r| format!("{}: {}", r.check, r.evidence()))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Environment preflight: capabilities {}/{} passed. Baseline {passed}/{} passed on source {}. Baseline failures describe the starting source, not permission to weaken final checks.\n{failures}",
            self.capabilities
                .iter()
                .filter(|r| r.exit_code == Some(0))
                .count(),
            self.capabilities.len(),
            self.baseline.len(),
            self.source
        )
    }
}

impl Sandbox {
    pub fn preflight(
        &mut self,
        recipe: Option<&Recipe>,
        id: i64,
        source: &str,
        cancelled: &AtomicBool,
        mut progress: impl FnMut(&str),
    ) -> io::Result<String> {
        let before = self.snapshot(&self.task_folder(), cancelled)?;
        let from_file = before
            .iter()
            .find(|(p, _, _)| recipe.is_none() && p == Path::new(".sprowt/environment.json"))
            .map(|(_, bytes, _)| serde_json::from_slice::<Recipe>(bytes))
            .transpose()?;
        let Some(recipe) = recipe.or(from_file.as_ref()) else {
            return Ok("No saved environment recipe. Inspect manifests and establish setup and baseline before changing source.".into());
        };
        recipe.validate().map_err(io::Error::other)?;
        let key = recipe.key(&before)?;
        let fingerprint = workspace::fingerprint(&before)?;
        let dir = self.root().join("environments");
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{id}.json"));
        let previous = match fs::read(&path) {
            Ok(bytes) => Some(serde_json::from_slice::<Receipt>(&bytes)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        let mut receipt = Receipt {
            recipe: recipe.clone(),
            error: None,
            environment: key.clone(),
            source: fingerprint.clone(),
            setup: Vec::new(),
            capabilities: Vec::new(),
            baseline: Vec::new(),
        };
        let mut result = (|| {
            // Even cached setup must prove its capabilities in the current VM and worker HOME.
            if previous.as_ref().is_some_and(|r| r.environment == key) {
                receipt.capabilities = self.environment_commands(
                    &recipe.capabilities,
                    cancelled,
                    "capabilities",
                    &mut progress,
                )?;
            }
            if receipt.capabilities.is_empty()
                || receipt.capabilities.iter().any(|r| r.exit_code != Some(0))
            {
                if !recipe.system_packages.is_empty() {
                    progress("Preflight · system packages");
                    self.install_packages(&recipe.packages(), cancelled)?;
                }
                receipt.setup =
                    self.environment_commands(&recipe.setup, cancelled, "setup", &mut progress)?;
                if let Some(failed) = receipt.setup.iter().find(|r| r.exit_code != Some(0)) {
                    return Err(io::Error::other(format!(
                        "Setup failed: {}\n{}",
                        failed.check,
                        failed.evidence()
                    )));
                }
                receipt.capabilities = self.environment_commands(
                    &recipe.capabilities,
                    cancelled,
                    "capabilities",
                    &mut progress,
                )?;
            }
            if let Some(failed) = receipt.capabilities.iter().find(|r| r.exit_code != Some(0)) {
                return Err(io::Error::other(format!(
                    "Capability unavailable: {}\n{}",
                    failed.check,
                    failed.evidence()
                )));
            }
            // Baselines run once per attempt; saved results are evidence, never a final-check cache.
            receipt.baseline =
                self.environment_commands(&recipe.baseline, cancelled, "baseline", &mut progress)?;
            Ok(receipt.summary())
        })();
        // Preserve unexpected setup edits even when its command failed.
        if !cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            let after = self.snapshot(&self.task_folder(), cancelled)?;
            if after != before {
                self.export(cancelled)?;
                result = Err(io::Error::other(
                    "Setup or baseline changed source. Edits are retained; correct the recipe before retrying.",
                ));
            }
        }
        if let Err(error) = &result {
            receipt.error = Some(error.to_string());
        }
        // Keep every attempt and a latest pointer. Failed setup is inspectable after reconnect.
        let bytes = serde_json::to_vec_pretty(&receipt)?;
        fs::write(&path, &bytes)?;
        fs::write(dir.join(format!("{id}-{source}.json")), bytes)?;
        result
    }

    fn environment_commands(
        &mut self,
        commands: &[Command],
        cancelled: &AtomicBool,
        phase: &str,
        progress: &mut impl FnMut(&str),
    ) -> io::Result<Vec<CheckResult>> {
        let mut results = Vec::new();
        for command in commands {
            progress(&format!("Preflight · {phase} · {}", command.name));
            let check = Check {
                task: None,
                check: command.name.clone(),
                command: command.argv.clone(),
                timeout_seconds: Some(command.timeout_seconds),
            };
            let result = self.run_check_command(&check, cancelled);
            let failed = result.exit_code != Some(0);
            results.push(result);
            if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Preflight stopped.",
                ));
            }
            if failed && phase != "baseline" {
                break;
            }
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn recipe() -> Recipe {
        Recipe {
            inputs: vec!["config.txt".into()],
            system_packages: vec![],
            setup: vec![],
            capabilities: vec![Command {
                name: "Node".into(),
                argv: vec![
                    "/usr/local/bin/mise".into(),
                    "exec".into(),
                    "node@22.16.0".into(),
                    "--".into(),
                    "node".into(),
                    "--version".into(),
                ],
                timeout_seconds: 30,
            }],
            baseline: vec![],
        }
    }
    #[test]
    fn recipes_reject_ephemeral_paths_and_invalidate_on_dependency_changes() {
        let mut r = recipe();
        r.validate().unwrap();
        let mut snapshot = vec![
            (PathBuf::from("package-lock.json"), b"one".to_vec(), 0),
            (PathBuf::from("app.js"), b"a".to_vec(), 0),
        ];
        let key = r.key(&snapshot).unwrap();
        snapshot[1].1 = b"b".to_vec();
        assert_eq!(key, r.key(&snapshot).unwrap());
        snapshot[0].1 = b"two".to_vec();
        assert_ne!(key, r.key(&snapshot).unwrap());
        r.capabilities[0].argv[0] = "/tasks/65/.venv/bin/python".into();
        assert!(r.validate().is_err());
        r = recipe();
        r.inputs = vec!["../outside".into()];
        assert!(r.validate().is_err());
        r = recipe();
        r.capabilities.clear();
        assert!(r.validate().is_err());
    }
    #[test]
    #[ignore = "runs setup, reuse, failed baselines, capability gating and source preservation in a disposable Linux VM"]
    fn live_preflight_retains_evidence_and_reuses_only_proven_setup() {
        let data = crate::store::test_support::TestData::new();
        fs::create_dir_all(&data.0).unwrap();
        let root = data.0.join("preflight");
        workspace::create_snapshot(
            &vec![(PathBuf::from("source.txt"), b"original".to_vec(), 0o644)],
            &root,
        )
        .unwrap();
        let flag = AtomicBool::new(false);
        let cmd = |name: &str, shell: &str| Command {
            name: name.into(),
            argv: vec!["/bin/sh".into(), "-c".into(), shell.into()],
            timeout_seconds: 30,
        };
        let mut r = Recipe {
            inputs: vec![],
            system_packages: vec![],
            setup: vec![cmd(
                "prepare",
                "echo prepared >> \"$HOME/preflight-count\"; touch \"$HOME/preflight-ready\"",
            )],
            capabilities: vec![cmd("available", "test -f \"$HOME/preflight-ready\"")],
            baseline: vec![cmd("known failure", "test original = broken")],
        };
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> io::Result<()> {
                let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
                vm.prepare_tasks(&[1], &flag)?;
                vm.assign_task(1, 1, &flag)?;
                let first = vm.preflight(Some(&r), 1, "attempt-1", &flag, |_| {})?;
                assert!(first.contains("Baseline 0/1 passed"));
                assert!(load(&root, 1)?.unwrap().baseline[0].failed());
                vm.preflight(Some(&r), 1, "attempt-2", &flag, |_| {})?;
                assert!(load(&root, 1)?.unwrap().setup.is_empty());
                assert_eq!(
                    vm.run_check_command(
                        &Check {
                            task: None,
                            check: "setup ran once".into(),
                            command: vec![
                                "/bin/sh".into(),
                                "-c".into(),
                                "test \"$(wc -l < \"$HOME/preflight-count\")\" -eq 1".into()
                            ],
                            timeout_seconds: Some(30)
                        },
                        &flag
                    )
                    .exit_code,
                    Some(0)
                );
                drop(vm);
                let mut vm = Sandbox::prepare(&root, &flag, |_| {})?;
                vm.assign_task(1, 1, &flag)?;
                vm.preflight(Some(&r), 1, "attempt-3", &flag, |_| {})?;
                assert!(load(&root, 1)?.unwrap().setup.is_empty());
                r.capabilities = vec![cmd("missing capability", "exit 1")];
                assert!(
                    vm.preflight(Some(&r), 1, "attempt-4", &flag, |_| {})
                        .unwrap_err()
                        .to_string()
                        .contains("missing capability")
                );
                let failed = load(&root, 1)?.unwrap();
                assert!(failed.baseline.is_empty());
                assert!(failed.error.is_some());
                r.setup = vec![cmd("bad setup", "echo changed > source.txt; exit 1")];
                assert!(
                    vm.preflight(Some(&r), 1, "attempt-5", &flag, |_| {})
                        .unwrap_err()
                        .to_string()
                        .contains("changed source")
                );
                assert_eq!(
                    fs::read_to_string(root.join("work/source.txt"))?,
                    "changed\n"
                );
                assert!(root.join("environments/1-attempt-1.json").exists());
                Ok(())
            }));
        crate::sandbox::delete(&root).unwrap();
        outcome.unwrap().unwrap();
    }
}
