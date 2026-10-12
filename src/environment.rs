use crate::{
    diagnostics::timing::{Phase, Timing},
    execution::{Check, CheckResult},
    sandbox::Sandbox,
    workspace::{self, Snapshot},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::atomic::AtomicBool,
};

pub(crate) mod cache;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub inputs: Vec<String>,
    #[serde(default)]
    pub system_packages: Vec<String>,
    #[serde(default)]
    pub reusable_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Resources>,
    pub setup: Vec<Command>,
    pub capabilities: Vec<Command>,
    pub baseline: Vec<Command>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    pub memory_gib: u64,
}

pub(crate) fn requested_memory(
    recipe: Option<&Recipe>,
    snapshot: &Snapshot,
) -> io::Result<Option<u64>> {
    let from_file = snapshot
        .iter()
        .find(|(path, _, _)| recipe.is_none() && path == Path::new(".sprowt/environment.json"))
        .map(|(_, bytes, _)| serde_json::from_slice::<Recipe>(bytes))
        .transpose()?;
    let Some(recipe) = recipe.or(from_file.as_ref()) else {
        return Ok(None);
    };
    recipe.validate().map_err(io::Error::other)?;
    Ok(recipe
        .resources
        .as_ref()
        .map(|resources| resources.memory_gib))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub name: String,
    pub argv: Vec<String>,
    pub timeout_seconds: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_domains: Vec<String>,
}

const MAX_SETUP_TIMEOUT: u64 = 600;

impl Recipe {
    pub fn validate(&self) -> Result<(), String> {
        if self
            .resources
            .as_ref()
            .is_some_and(|resources| resources.memory_gib != 8)
        {
            return Err("Explicit VM resources support memory_gib: 8. Omit resources for the ordinary allocation.".into());
        }
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
        for (phase, commands, maximum) in [
            ("setup", &self.setup, MAX_SETUP_TIMEOUT),
            (
                "capabilities",
                &self.capabilities,
                crate::execution::MAX_CHECK_TIMEOUT,
            ),
            (
                "baseline",
                &self.baseline,
                crate::execution::MAX_CHECK_TIMEOUT,
            ),
        ] {
            for command in commands {
                if command.name.trim().is_empty()
                    || command.name.len() > 160
                    || !(1..=maximum).contains(&command.timeout_seconds)
                    || command.argv.is_empty()
                    || !Path::new(&command.argv[0]).is_absolute()
                    || command.argv.iter().any(|a| {
                        a.contains('\0')
                            || a.contains("/tasks/")
                            || a.contains("/home/sprowt/workers/")
                            || a.len() > 16000
                    })
                {
                    return Err(format!(
                        "Environment {phase} commands need a name, absolute executable, portable arguments and a 1–{maximum} second budget; temporary worker paths are not reusable."
                    ));
                }
                command
                    .network_request()
                    .map_err(|error| error.to_string())?;
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

impl Command {
    fn network_request(&self) -> io::Result<Option<crate::network::Request>> {
        if self.network_domains.is_empty() {
            return Ok(None);
        }
        crate::network::Request {
            domains: self.network_domains.clone(),
            reason: format!("Preparation: {}", self.name),
        }
        .validate()
        .map(Some)
    }
}

pub fn schema() -> Value {
    let command = |maximum| {
        json!({"type":"object","additionalProperties":false,"properties":{
        "name":{"type":"string"},"argv":{"type":"array","items":{"type":"string"}},
        "timeout_seconds":{"type":"integer","minimum":1,"maximum":maximum},
        "network_domains":{"type":"array","items":{"type":"string"},"maxItems":8}},"required":["name","argv","timeout_seconds","network_domains"]})
    };
    json!({"type":["object","null"],"additionalProperties":false,"properties":{
        "inputs":{"type":"array","items":{"type":"string"}},
        "system_packages":{"type":"array","items":{"type":"string"}},
        "resources":{"type":["object","null"],"additionalProperties":false,"properties":{"memory_gib":{"type":"integer","enum":[8]}},"required":["memory_gib"]},
        "reusable_paths":{"type":"array","items":{"type":"string","enum":["node_modules"]},"maxItems":1},
        "setup":{"type":"array","items":command(MAX_SETUP_TIMEOUT)},
        "capabilities":{"type":"array","items":command(crate::execution::MAX_CHECK_TIMEOUT)},
        "baseline":{"type":"array","items":command(crate::execution::MAX_CHECK_TIMEOUT)}},
        "required":["inputs","system_packages","reusable_paths","resources","setup","capabilities","baseline"]})
}

#[derive(Serialize, Deserialize)]
pub struct Receipt {
    pub recipe: Recipe,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_changes: Vec<PathBuf>,
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
    pub fn changed_source(&self) -> bool {
        !self.source_changes.is_empty() || self.error.as_deref().is_some_and(source_change_failure)
    }
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

pub(crate) fn source_change_failure(reason: &str) -> bool {
    reason
        .strip_prefix("Environment preflight failed: ")
        .unwrap_or(reason)
        .starts_with("Setup or baseline changed source")
}

fn changed_paths(before: &Snapshot, after: &Snapshot) -> Vec<PathBuf> {
    let before: BTreeMap<_, _> = before.iter().map(|(p, b, m)| (p, (b, m))).collect();
    let after: BTreeMap<_, _> = after.iter().map(|(p, b, m)| (p, (b, m))).collect();
    before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .map(|path| (*path).clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

trait Runner {
    fn root(&self) -> &Path;
    fn identity(&self) -> &crate::sandbox::Identity;
    fn home(&self) -> String;
    fn snapshot(&mut self, cancelled: &AtomicBool) -> io::Result<Snapshot>;
    fn packages(
        &mut self,
        request: &crate::packages::Request,
        cancelled: &AtomicBool,
    ) -> io::Result<String>;
    fn cache(
        &mut self,
        action: &str,
        key: &str,
        dependencies: bool,
        cancelled: &AtomicBool,
    ) -> io::Result<cache::Use>;
    fn batch(
        &mut self,
        commands: &[Command],
        cancelled: &AtomicBool,
        phase: &str,
        progress: &mut dyn FnMut(&str),
    ) -> io::Result<Vec<CheckResult>>;
    fn preserve(&mut self, snapshot: &Snapshot, cancelled: &AtomicBool) -> io::Result<()>;
    fn save_receipt(
        &self,
        id: i64,
        source: &str,
        receipt: &Receipt,
        cancelled: &AtomicBool,
    ) -> io::Result<()>;
}

fn preflight(
    runner: &mut impl Runner,
    recipe: Option<&Recipe>,
    id: i64,
    source: &str,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(&str),
    timing: Timing<'_>,
) -> io::Result<String> {
    let before = runner.snapshot(cancelled)?;
    let from_file = before
        .iter()
        .find(|(p, _, _)| recipe.is_none() && p == Path::new(".sprowt/environment.json"))
        .map(|(_, bytes, _)| serde_json::from_slice::<Recipe>(bytes))
        .transpose()?;
    let Some(recipe) = recipe.or(from_file.as_ref()) else {
        return Ok("No saved environment recipe. Inspect manifests and establish setup and baseline before changing source.".into());
    };
    recipe.validate().map_err(io::Error::other)?;
    let key = recipe.key(&before, runner.identity())?;
    let fingerprint = workspace::fingerprint(&before)?;
    let emit = |observation| timing.emit(observation);
    let timing = Timing::new(&fingerprint, &emit).with_environment(&key);
    let dir = runner.root().join("environments");
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
        source_changes: Vec::new(),
        environment: key.clone(),
        source: fingerprint.clone(),
        runtime_home: runner.home(),
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
                runner.cache(
                    "restore",
                    &key,
                    !recipe.reusable_paths.is_empty(),
                    cancelled,
                )
            })?;
        }
        if retained || receipt.cache.dependencies() {
            receipt.capabilities = timing.measure(Phase::Capabilities, || {
                commands(
                    runner,
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
                    runner.packages(&recipe.packages(), cancelled)
                })?;
            }
            receipt.setup = timing.measure(Phase::DependencySetup, || {
                commands(runner, &recipe.setup, cancelled, "setup", &mut progress)
            })?;
            if let Some(failed) = receipt.setup.iter().find(|r| r.exit_code != Some(0)) {
                return Err(io::Error::other(format!(
                    "Setup failed: {}\n{}",
                    failed.check,
                    failed.evidence()
                )));
            }
            receipt.capabilities = timing.measure(Phase::Capabilities, || {
                commands(
                    runner,
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
            commands(
                runner,
                &recipe.baseline,
                cancelled,
                "baseline",
                &mut progress,
            )
        })?;
        Ok(())
    })();
    if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
        result = Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Environment preparation stopped.",
        ));
    }
    // Preserve unexpected setup edits even when its command failed.
    if !cancelled.load(std::sync::atomic::Ordering::Relaxed) {
        let after = runner.snapshot(cancelled)?;
        if after != before {
            runner.preserve(&after, cancelled)?;
            receipt.source_changes = changed_paths(&before, &after);
            let mut paths = receipt
                .source_changes
                .iter()
                .take(4)
                .map(|path| {
                    path.to_string_lossy()
                        .escape_debug()
                        .take(120)
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join(", ");
            if receipt.source_changes.len() > 4 {
                paths.push_str(&format!(" (+{} more)", receipt.source_changes.len() - 4));
            }
            let original = result
                .as_ref()
                .err()
                .map(|error| format!("\nOriginal preparation failure:\n{error}"))
                .unwrap_or_default();
            result = Err(io::Error::other(format!(
                "Setup or baseline changed source: {paths}.\nEdits are retained. Revise the setup recipe before retrying.{original}"
            )));
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
            runner.cache(
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
    runner.save_receipt(id, source, &receipt, cancelled)?;
    result.map(|()| receipt.summary())
}

fn save_attempt(root: &Path, id: i64, source: &str, receipt: &Receipt) -> io::Result<PathBuf> {
    let directory = root.join("environments");
    let bytes = serde_json::to_vec_pretty(receipt)?;
    let attempt = directory.join(format!("{id}-{source}.json"));
    let pending = directory.join(format!("{id}-{source}.pending"));
    for saved in [&attempt, &pending] {
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
    Ok(pending)
}

fn publish_receipt(root: &Path, id: i64, pending: &Path) -> io::Result<()> {
    fs::rename(
        pending,
        root.join("environments").join(format!("{id}.json")),
    )
}

fn commands(
    runner: &mut impl Runner,
    commands: &[Command],
    cancelled: &AtomicBool,
    phase: &str,
    progress: &mut dyn FnMut(&str),
) -> io::Result<Vec<CheckResult>> {
    runner.batch(commands, cancelled, phase, progress)
}

fn command_batch(
    commands: &[Command],
    cancelled: &AtomicBool,
    phase: &str,
    progress: &mut dyn FnMut(&str),
    mut run: impl FnMut(&Check) -> CheckResult,
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
        let result = run(&check);
        let failed = result.exit_code != Some(0);
        results.push(result);
        if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        if failed && phase != "baseline" {
            break;
        }
    }
    Ok(results)
}

impl Runner for Sandbox {
    fn root(&self) -> &Path {
        self.root()
    }
    fn identity(&self) -> &crate::sandbox::Identity {
        self.identity()
    }
    fn home(&self) -> String {
        self.runtime_home()
    }
    fn snapshot(&mut self, cancelled: &AtomicBool) -> io::Result<Snapshot> {
        self.snapshot(&self.task_folder(), cancelled)
    }
    fn packages(
        &mut self,
        request: &crate::packages::Request,
        cancelled: &AtomicBool,
    ) -> io::Result<String> {
        self.install_packages(request, cancelled)
    }
    fn cache(
        &mut self,
        action: &str,
        key: &str,
        dependencies: bool,
        cancelled: &AtomicBool,
    ) -> io::Result<cache::Use> {
        cache::operation(self, action, key, dependencies, cancelled)
    }
    fn batch(
        &mut self,
        commands: &[Command],
        cancelled: &AtomicBool,
        phase: &str,
        progress: &mut dyn FnMut(&str),
    ) -> io::Result<Vec<CheckResult>> {
        command_batch(commands, cancelled, phase, progress, |check| {
            self.run_check_command(check, cancelled)
        })
    }
    fn preserve(&mut self, _: &Snapshot, _: &AtomicBool) -> io::Result<()> {
        self.export(&AtomicBool::new(false)).map(|_| ())
    }
    fn save_receipt(
        &self,
        id: i64,
        source: &str,
        receipt: &Receipt,
        _: &AtomicBool,
    ) -> io::Result<()> {
        let pending = save_attempt(self.root(), id, source, receipt)?;
        publish_receipt(self.root(), id, &pending)
    }
}

impl Runner for crate::sandbox::session::TaskSession {
    fn root(&self) -> &Path {
        &self.root
    }
    fn identity(&self) -> &crate::sandbox::Identity {
        self.identity()
    }
    fn home(&self) -> String {
        self.home().into()
    }
    fn snapshot(&mut self, cancelled: &AtomicBool) -> io::Result<Snapshot> {
        self.snapshot(cancelled)
    }
    fn packages(
        &mut self,
        request: &crate::packages::Request,
        cancelled: &AtomicBool,
    ) -> io::Result<String> {
        crate::sandbox::session::TaskSession::packages(self, request, cancelled)
    }
    fn cache(
        &mut self,
        action: &str,
        key: &str,
        dependencies: bool,
        cancelled: &AtomicBool,
    ) -> io::Result<cache::Use> {
        crate::sandbox::session::TaskSession::cache(self, action, key, dependencies, cancelled)
    }
    fn batch(
        &mut self,
        commands: &[Command],
        cancelled: &AtomicBool,
        phase: &str,
        progress: &mut dyn FnMut(&str),
    ) -> io::Result<Vec<CheckResult>> {
        let _lease = self.read_lease(cancelled)?;
        self.authorize(cancelled)?;
        for command in commands {
            if let Some(request) = command.network_request()? {
                self.require_network(request, cancelled)?;
            }
        }
        command_batch(commands, cancelled, phase, progress, |check| {
            self.command(check, cancelled)
        })
    }
    fn preserve(&mut self, snapshot: &Snapshot, cancelled: &AtomicBool) -> io::Result<()> {
        crate::sandbox::session::TaskSession::preserve(self, snapshot, cancelled)
    }
    fn save_receipt(
        &self,
        id: i64,
        source: &str,
        receipt: &Receipt,
        cancelled: &AtomicBool,
    ) -> io::Result<()> {
        let pending = save_attempt(&self.root, id, source, receipt)?;
        let result = self.accept(cancelled, |_| publish_receipt(&self.root, id, &pending));
        if result.is_err() {
            let _ = fs::remove_file(pending);
        }
        result
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
        preflight(
            self,
            recipe,
            id,
            source,
            cancelled,
            progress,
            Timing::quiet(),
        )
    }
}

impl crate::sandbox::session::TaskSession {
    pub(crate) fn preflight(
        &mut self,
        recipe: Option<&Recipe>,
        cancelled: &AtomicBool,
        progress: impl FnMut(&str),
        timing: Timing<'_>,
    ) -> io::Result<String> {
        preflight(
            self,
            recipe,
            self.id,
            &self.source.clone(),
            cancelled,
            progress,
            timing,
        )
    }
}

#[cfg(test)]
mod tests;
