use crate::{
    diagnostics::timing::{Phase, Timing},
    execution::{Check, CheckResult},
    sandbox::Sandbox,
    workspace::{self, Snapshot},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs, io,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::atomic::AtomicBool,
};

mod cache;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub inputs: Vec<String>,
    #[serde(default)]
    pub system_packages: Vec<String>,
    #[serde(default)]
    pub reusable_paths: Vec<String>,
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
        if !self.reusable_paths.is_empty()
            && (self.reusable_paths != ["node_modules"]
                || self.setup.is_empty()
                || !["package.json", "package-lock.json"]
                    .iter()
                    .all(|path| self.inputs.iter().any(|input| input == path)))
        {
            return Err("Reusable dependencies support only node_modules, with setup and declared package.json/package-lock.json inputs.".into());
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
    fn key(&self, source: &Snapshot, identity: &crate::sandbox::Identity) -> io::Result<String> {
        if !self.reusable_paths.is_empty()
            && !["package.json", "package-lock.json"]
                .iter()
                .all(|input| source.iter().any(|(path, _, _)| path == Path::new(input)))
        {
            return Err(io::Error::other(
                "Reusable node_modules needs package.json and package-lock.json in the current source.",
            ));
        }
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
        inputs.push((PathBuf::from("@platform"), serde_json::to_vec(identity)?, 0));
        inputs.push((
            PathBuf::from("@cache-policy"),
            cache::POLICY.as_bytes().to_vec(),
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
        "reusable_paths":{"type":"array","items":{"type":"string","enum":["node_modules"]},"maxItems":1},
        "setup":{"type":"array","items":command},"capabilities":{"type":"array","items":command},"baseline":{"type":"array","items":command}},
        "required":["inputs","system_packages","reusable_paths","setup","capabilities","baseline"]})
}

#[derive(Serialize, Deserialize)]
pub struct Receipt {
    pub recipe: Recipe,
    #[serde(default)]
    pub error: Option<String>,
    pub environment: String,
    pub source: String,
    #[serde(default)]
    pub runtime_home: String,
    #[serde(default)]
    pub cache: cache::Use,
    #[serde(default)]
    pub cache_saved: Option<cache::Use>,
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
    pub fn cache_summary(&self) -> String {
        let mut summary = self.cache.summary();
        if let Some(saved) = &self.cache_saved {
            summary.push('\n');
            summary.push_str(&saved.summary());
        }
        summary
    }
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
            "Environment preflight: capabilities {}/{} passed. Baseline {passed}/{} passed on source {}. Baseline failures describe the starting source, not permission to weaken final checks.\n{}\n{failures}",
            self.capabilities
                .iter()
                .filter(|r| r.exit_code == Some(0))
                .count(),
            self.capabilities.len(),
            self.baseline.len(),
            self.source,
            self.cache_summary()
        )
    }
}

impl Sandbox {
    #[cfg(test)]
    pub fn preflight(
        &mut self,
        recipe: Option<&Recipe>,
        id: i64,
        source: &str,
        cancelled: &AtomicBool,
        progress: impl FnMut(&str),
    ) -> io::Result<String> {
        self.preflight_with_timing(recipe, id, source, cancelled, progress, Timing::quiet())
    }

    pub(crate) fn preflight_with_timing(
        &mut self,
        recipe: Option<&Recipe>,
        id: i64,
        source: &str,
        cancelled: &AtomicBool,
        mut progress: impl FnMut(&str),
        timing: Timing<'_>,
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
        let key = recipe.key(&before, self.identity())?;
        let fingerprint = workspace::fingerprint(&before)?;
        let emit = |observation| timing.emit(observation);
        let timing = Timing::new(&fingerprint, &emit).with_environment(&key);
        let dir = self.root().join("environments");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
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
            runtime_home: self.runtime_home(),
            cache: cache::Use::Miss,
            cache_saved: None,
            setup: Vec::new(),
            capabilities: Vec::new(),
            baseline: Vec::new(),
        };
        let mut result = (|| {
            // Even cached setup must prove its capabilities in the current VM and worker HOME.
            let retained = previous.as_ref().is_some_and(|r| {
                r.environment == key && r.runtime_home == receipt.runtime_home && r.error.is_none()
            });
            if retained {
                receipt.cache = cache::Use::Retained;
            }
            if !retained {
                progress("Preflight · copy prepared assets");
                receipt.cache = timing.measure(Phase::DependencySetup, || {
                    cache::operation(
                        self,
                        "restore",
                        &key,
                        !recipe.reusable_paths.is_empty(),
                        cancelled,
                    )
                })?;
            }
            if retained || receipt.cache.dependencies() {
                receipt.capabilities = timing.measure(Phase::Capabilities, || {
                    self.environment_commands(
                        &recipe.capabilities,
                        cancelled,
                        "capabilities",
                        &mut progress,
                    )
                })?;
            }
            if receipt.capabilities.is_empty()
                || receipt.capabilities.iter().any(|r| r.exit_code != Some(0))
            {
                if !recipe.system_packages.is_empty() {
                    progress("Preflight · system packages");
                    timing.measure(Phase::DependencySetup, || {
                        self.install_packages(&recipe.packages(), cancelled)
                    })?;
                }
                receipt.setup = timing.measure(Phase::DependencySetup, || {
                    self.environment_commands(&recipe.setup, cancelled, "setup", &mut progress)
                })?;
                if let Some(failed) = receipt.setup.iter().find(|r| r.exit_code != Some(0)) {
                    return Err(io::Error::other(format!(
                        "Setup failed: {}\n{}",
                        failed.check,
                        failed.evidence()
                    )));
                }
                receipt.capabilities = timing.measure(Phase::Capabilities, || {
                    self.environment_commands(
                        &recipe.capabilities,
                        cancelled,
                        "capabilities",
                        &mut progress,
                    )
                })?;
            }
            if let Some(failed) = receipt.capabilities.iter().find(|r| r.exit_code != Some(0)) {
                return Err(io::Error::other(format!(
                    "Capability unavailable: {}\n{}",
                    failed.check,
                    failed.evidence()
                )));
            }
            // Baselines run once per attempt; saved results are evidence, never a final-check cache.
            receipt.baseline = timing.measure(Phase::Baseline, || {
                self.environment_commands(&recipe.baseline, cancelled, "baseline", &mut progress)
            })?;
            Ok(())
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
        if result.is_ok()
            && (!receipt.setup.is_empty()
                || matches!(
                    receipt.cache,
                    cache::Use::Miss | cache::Use::Rejected { .. }
                ))
        {
            progress("Preflight · save prepared assets");
            let saved = timing.measure(Phase::DependencySetup, || {
                cache::operation(
                    self,
                    "publish",
                    &key,
                    !recipe.reusable_paths.is_empty(),
                    cancelled,
                )
            });
            match saved {
                Ok(saved) => receipt.cache_saved = Some(saved),
                Err(error) => {
                    receipt.error = Some(error.to_string());
                    result = Err(error);
                }
            }
        }
        // Keep every attempt and a latest pointer. Failed setup is inspectable after reconnect.
        let bytes = serde_json::to_vec_pretty(&receipt)?;
        let attempt = dir.join(format!("{id}-{source}.json"));
        for saved in [&path, &attempt] {
            use io::Write;
            fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(saved)?
                .write_all(&bytes)?;
            fs::set_permissions(saved, fs::Permissions::from_mode(0o600))?;
        }
        result.map(|()| receipt.summary())
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
mod tests;
