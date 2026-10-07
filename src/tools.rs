use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

use crate::{
    git_mod, mailbox, mod_sync, network, packages,
    plan::{Plan, Role},
    sandbox::Sandbox,
    store::CodeMod,
    workspace,
};

#[derive(Clone, Copy, PartialEq)]
enum Tool {
    SyncProject,
    InitializeProject,
    AdoptSnapshot,
    ConnectRepository,
    CreateWorktree,
    RefreshWorktree,
    CheckTarget,
    UpdateTarget,
    FinishUpdate,
    PublishPr,
    PrepareEdits,
    CleanupMod,
    CloseMod,
    ReopenMod,
    PruneMod,
    InstallPackages,
    RequestNetwork,
    SendMessage,
    ReadMessages,
    AckMessages,
}

const REGISTRY: [Tool; 20] = [
    Tool::SyncProject,
    Tool::InitializeProject,
    Tool::AdoptSnapshot,
    Tool::ConnectRepository,
    Tool::CreateWorktree,
    Tool::RefreshWorktree,
    Tool::CheckTarget,
    Tool::UpdateTarget,
    Tool::FinishUpdate,
    Tool::PublishPr,
    Tool::PrepareEdits,
    Tool::CleanupMod,
    Tool::CloseMod,
    Tool::ReopenMod,
    Tool::PruneMod,
    Tool::InstallPackages,
    Tool::RequestNetwork,
    Tool::SendMessage,
    Tool::ReadMessages,
    Tool::AckMessages,
];
static LOG: Mutex<()> = Mutex::new(());

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Self::SyncProject => "sync_project",
            Self::InitializeProject => "initialize_project",
            Self::AdoptSnapshot => "adopt_snapshot",
            Self::ConnectRepository => "connect_repository",
            Self::CreateWorktree => "create_worktree",
            Self::RefreshWorktree => "refresh_worktree",
            Self::CheckTarget => "check_target",
            Self::UpdateTarget => "update_target",
            Self::FinishUpdate => "finish_update",
            Self::PublishPr => "publish_pr",
            Self::PrepareEdits => "prepare_edits",
            Self::CleanupMod => "cleanup_mod",
            Self::CloseMod => "close_mod",
            Self::ReopenMod => "reopen_mod",
            Self::PruneMod => "prune_mod",
            Self::InstallPackages => packages::TOOL,
            Self::RequestNetwork => network::TOOL,
            Self::SendMessage => mailbox::SEND,
            Self::ReadMessages => mailbox::READ,
            Self::AckMessages => mailbox::ACK,
        }
    }

    fn permitted(self, context: &Context) -> bool {
        match self {
            Self::SendMessage | Self::ReadMessages | Self::AckMessages | Self::RequestNetwork => {
                context
                    .worker
                    .is_some_and(|(_, role)| role == Role::Executor)
                    && context.database.is_some()
                    && context.root.is_some()
            }
            Self::SyncProject => context.worker.is_none() && context.project.is_some(),
            Self::InstallPackages => {
                context
                    .worker
                    .is_some_and(|(_, role)| role == Role::Executor)
                    && context.root.is_some()
            }
            _ => context.worker.is_none() && context.mod_id.is_some(),
        }
    }
}

pub struct Context {
    pub provider: String,
    pub muse: bool,
    database: Option<PathBuf>,
    pub tasks: Vec<i64>,
    mod_id: Option<i64>,
    worker: Option<(i64, Role)>,
    root: Option<PathBuf>,
    project: Option<PathBuf>,
    description: String,
    verified: Option<String>,
}

impl Context {
    pub fn project(project: &Path, root: PathBuf) -> Self {
        Self {
            provider: "codex".into(),
            muse: false,
            database: None,
            tasks: Vec::new(),
            mod_id: None,
            worker: None,
            root: Some(root),
            project: Some(project.to_owned()),
            description: String::new(),
            verified: None,
        }
    }

    pub fn harness(project: &Path, code_mod: &CodeMod) -> Self {
        Self {
            provider: "codex".into(),
            muse: false,
            database: None,
            tasks: code_mod
                .execution
                .as_ref()
                .map_or_else(Vec::new, |e| e.tasks.iter().map(|t| t.id).collect()),
            mod_id: Some(code_mod.id),
            worker: None,
            root: code_mod
                .git_root
                .clone()
                .or_else(|| code_mod.execution.as_ref().map(|e| e.workspace.clone())),
            project: Some(project.to_owned()),
            description: code_mod.description.clone(),
            verified: code_mod
                .execution
                .as_ref()
                .filter(|e| e.complete() && matches!(e.status.as_str(), "review" | "applied"))
                .and_then(|e| e.fingerprint.clone()),
        }
    }

    pub fn worker(code_mod: &CodeMod, id: i64, role: Role) -> Self {
        Self {
            provider: "codex".into(),
            muse: false,
            database: None,
            tasks: code_mod
                .execution
                .as_ref()
                .map_or_else(Vec::new, |e| e.tasks.iter().map(|t| t.id).collect()),
            mod_id: Some(code_mod.id),
            worker: Some((id, role)),
            root: if role == Role::Executor {
                code_mod
                    .execution
                    .as_ref()
                    .filter(|e| e.status != "applied")
                    .map(|e| e.workspace.clone())
            } else {
                code_mod.git_root.clone()
            },
            project: None,
            description: String::new(),
            verified: None,
        }
    }

    pub fn worker_id(&self) -> i64 {
        self.worker.map_or(0, |(id, _)| id)
    }

    pub fn with_mailbox(mut self, database: &Path) -> Self {
        self.database = Some(database.to_owned());
        self
    }

    pub fn workspace(&self) -> Option<&Path> {
        self.worker
            .filter(|(_, role)| *role == Role::Executor)
            .and(self.root.as_deref())
    }

    fn root(&self) -> io::Result<&Path> {
        self.root
            .as_deref()
            .ok_or_else(|| io::Error::other("This tool needs the mod's workspace."))
    }
}

pub enum Request {
    SyncProject,
    InitializeProject,
    AdoptSnapshot,
    ConnectRepository(git_mod::RepositoryRequest),
    CreateWorktree,
    RefreshWorktree,
    CheckTarget,
    UpdateTarget {
        target: mod_sync::Target,
        plan: Plan,
    },
    FinishUpdate,
    PublishPr {
        draft: bool,
    },
    PrepareEdits,
    CleanupMod,
    CloseMod,
    ReopenMod,
    PruneMod,
    InstallPackages(packages::Request),
    NetworkAccess(network::Request),
    Message(mailbox::Request),
}

impl Request {
    fn tool(&self) -> Tool {
        match self {
            Self::SyncProject => Tool::SyncProject,
            Self::InitializeProject => Tool::InitializeProject,
            Self::AdoptSnapshot => Tool::AdoptSnapshot,
            Self::ConnectRepository(_) => Tool::ConnectRepository,
            Self::CreateWorktree => Tool::CreateWorktree,
            Self::RefreshWorktree => Tool::RefreshWorktree,
            Self::CheckTarget => Tool::CheckTarget,
            Self::UpdateTarget { .. } => Tool::UpdateTarget,
            Self::FinishUpdate => Tool::FinishUpdate,
            Self::PublishPr { .. } => Tool::PublishPr,
            Self::PrepareEdits => Tool::PrepareEdits,
            Self::CleanupMod => Tool::CleanupMod,
            Self::CloseMod => Tool::CloseMod,
            Self::ReopenMod => Tool::ReopenMod,
            Self::PruneMod => Tool::PruneMod,
            Self::InstallPackages(_) => Tool::InstallPackages,
            Self::NetworkAccess(_) => Tool::RequestNetwork,
            Self::Message(mailbox::Request::Send(_)) => Tool::SendMessage,
            Self::Message(mailbox::Request::Read) => Tool::ReadMessages,
            Self::Message(mailbox::Request::Ack(_)) => Tool::AckMessages,
        }
    }
}

#[derive(Debug)]
pub enum Output {
    Done,
    PullRequest(String),
    Text(String),
    Target(Option<mod_sync::Target>),
}

impl Output {
    pub fn message(self) -> String {
        match self {
            Self::Done => "Done".into(),
            Self::PullRequest(url) | Self::Text(url) => url,
            Self::Target(target) => target.map_or("No remote".into(), |t| t.head),
        }
    }
}

pub fn advertised(context: &Context) -> Vec<Value> {
    let mut tools = Vec::new();
    if Tool::InstallPackages.permitted(context) {
        tools.push(packages::tool());
    }
    if Tool::SendMessage.permitted(context) {
        tools.extend(mailbox::tools());
        tools.push(network::tool());
    }
    tools
}

pub struct Dispatcher<'a> {
    context: &'a Context,
    vm: Option<&'a mut Sandbox>,
    cancelled: &'a AtomicBool,
    github_cli: &'a Path,
}

impl<'a> Dispatcher<'a> {
    pub fn new(
        context: &'a Context,
        vm: Option<&'a mut Sandbox>,
        cancelled: &'a AtomicBool,
    ) -> Self {
        Self {
            context,
            vm,
            cancelled,
            github_cli: Path::new("gh"),
        }
    }

    pub fn execute(&mut self, request: Request, progress: impl FnMut(&str)) -> io::Result<Output> {
        self.dispatch(Some(request.tool()), || Ok(request), progress)
    }

    pub fn worker_call(
        &mut self,
        name: &str,
        arguments: Value,
        progress: impl FnMut(&str),
    ) -> io::Result<Output> {
        let tool = REGISTRY.iter().copied().find(|tool| tool.name() == name);
        self.dispatch(
            tool,
            || match name {
                network::TOOL => network::Request::parse(arguments).map(Request::NetworkAccess),
                mailbox::SEND | mailbox::READ | mailbox::ACK => {
                    mailbox::Request::parse(name, arguments).map(Request::Message)
                }
                _ => packages::Request::parse(arguments).map(Request::InstallPackages),
            },
            progress,
        )
    }

    fn dispatch(
        &mut self,
        tool: Option<Tool>,
        request: impl FnOnce() -> io::Result<Request>,
        mut progress: impl FnMut(&str),
    ) -> io::Result<Output> {
        let started = Instant::now();
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        if matches!(tool, Some(Tool::CreateWorktree | Tool::SyncProject))
            && self.context.worker.is_none()
        {
            let root = self.context.root()?;
            fs::create_dir_all(root)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
            }
        }
        self.record(tool, id, "started", 0)?;
        let result = (|| {
            let tool = tool.ok_or_else(|| {
                io::Error::new(io::ErrorKind::PermissionDenied, "Unknown harness tool.")
            })?;
            if !tool.permitted(self.context) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "This caller cannot use that tool.",
                ));
            }
            if self.cancelled.load(Ordering::Relaxed) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Tool stopped; retry when ready.",
                ));
            }
            let request = request()?;
            if request.tool() != tool {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Tool arguments do not match the operation.",
                ));
            }
            let root = self.context.root()?;
            match request {
                Request::NetworkAccess(request) => {
                    let store =
                        crate::store::Store::open(self.context.database.as_deref().unwrap())?;
                    let execution = store
                        .execution(self.context.mod_id.unwrap())
                        .map_err(io::Error::other)?
                        .ok_or_else(|| io::Error::other("Network requests require the mod VM."))?;
                    if execution.backend != "apple-container"
                        || execution.workspace.canonicalize()? != root.canonicalize()?
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "Network request is outside this mod's VM.",
                        ));
                    }
                    let request = request.validate()?;
                    let policy = crate::sandbox::network(root)?;
                    let already_allowed = request.domains.iter().all(|domain| {
                        policy.keys().any(|allowed| {
                            domain == allowed
                                || allowed
                                    .strip_prefix('*')
                                    .is_some_and(|suffix| domain.ends_with(suffix))
                        })
                    });
                    store
                        .request_network(
                            self.context.mod_id.unwrap(),
                            self.context.worker_id(),
                            request,
                            already_allowed,
                        )
                        .map(Output::Text)
                        .map_err(io::Error::other)
                }
                Request::Message(request) => {
                    let mut store =
                        crate::store::Store::open(self.context.database.as_deref().unwrap())?;
                    let result = store
                        .mailbox_call(
                            self.context.mod_id.unwrap(),
                            self.context.worker_id(),
                            request,
                        )
                        .map_err(io::Error::other)?;
                    Ok(Output::Text(result.to_string()))
                }
                Request::SyncProject => {
                    progress("syncing project branch");
                    crate::git_sync::project(
                        self.context.project.as_deref().unwrap(),
                        root,
                        self.cancelled,
                    )?;
                    Ok(Output::Done)
                }
                Request::InitializeProject => {
                    let project = self.context.project.as_deref().unwrap();
                    if !git_mod::has_commit(project)
                        && workspace::fingerprint(&workspace::project_state(project, root)?)?
                            != workspace::fingerprint(&workspace::source_state(
                                &root.join("before"),
                            )?)?
                    {
                        return Err(io::Error::other(
                            "Starting files changed after review. Restore them or recreate this codemod.",
                        ));
                    }
                    progress("setting up Git");
                    git_mod::adopt(project, root, self.cancelled)?;
                    Ok(Output::Done)
                }
                Request::AdoptSnapshot => {
                    progress("adopting saved codemod");
                    git_mod::adopt(
                        self.context.project.as_deref().unwrap(),
                        root,
                        self.cancelled,
                    )?;
                    Ok(Output::Done)
                }
                Request::ConnectRepository(request) => {
                    progress("connecting GitHub repository");
                    git_mod::connect_repository(root, request, self.github_cli, self.cancelled)?;
                    Ok(Output::Done)
                }
                Request::CreateWorktree => {
                    if !root.join("git-mod.json").exists() {
                        progress("checking project branch");
                        crate::git_sync::project(
                            self.context.project.as_deref().unwrap(),
                            root,
                            self.cancelled,
                        )?;
                    }
                    progress("creating worktree");
                    git_mod::prepare(
                        self.context.project.as_deref().unwrap(),
                        root,
                        self.cancelled,
                    )?;
                    Ok(Output::Done)
                }
                Request::RefreshWorktree => {
                    progress("checking project branch");
                    git_mod::refresh(
                        self.context.project.as_deref().unwrap(),
                        root,
                        self.cancelled,
                    )?;
                    Ok(Output::Done)
                }
                Request::PublishPr { draft } => {
                    if !draft
                        && !git_mod::load(root)?.published()
                        && self.context.verified.as_deref()
                            != Some(&workspace::review(root)?.fingerprint)
                    {
                        return Err(io::Error::other(
                            "PR publication needs the verified source. Recheck before publishing.",
                        ));
                    }
                    if !draft {
                        mod_sync::require_current(root, self.github_cli, self.cancelled)?;
                    }
                    progress("publishing PR");
                    git_mod::publish(
                        root,
                        &self.context.description,
                        draft,
                        self.github_cli,
                        self.cancelled,
                    )
                    .map(Output::PullRequest)
                }
                Request::PrepareEdits => {
                    progress("preparing edits");
                    git_mod::prepare_edits(root, self.github_cli, self.cancelled)?;
                    Ok(Output::Done)
                }
                Request::CheckTarget => {
                    let target = mod_sync::target(root, self.github_cli, self.cancelled)?;
                    let state = git_mod::load(root)?;
                    if target.as_ref().is_some_and(|t| {
                        t.head != state.base && t.pr_state.as_deref() == Some("OPEN")
                    }) {
                        mod_sync::hold_pr(root, self.github_cli, self.cancelled)?;
                    }
                    Ok(Output::Target(target))
                }
                Request::UpdateTarget { target, plan } => {
                    if mod_sync::load(root)?.is_none()
                        && (self.context.verified.is_none()
                            || self.context.verified.as_deref()
                                != Some(&workspace::review(root)?.fingerprint))
                    {
                        return Err(io::Error::other(
                            "Wait for the current version to finish before updating.",
                        ));
                    }
                    mod_sync::hold_pr(root, self.github_cli, self.cancelled)?;
                    mod_sync::prepare(root, &target, &plan, self.cancelled)?;
                    Ok(Output::Done)
                }
                Request::FinishUpdate => {
                    let fingerprint = self.context.verified.as_deref().ok_or_else(|| {
                        io::Error::other("Combined checks must pass before saving the merge.")
                    })?;
                    mod_sync::finish(root, fingerprint, self.cancelled)?;
                    Ok(Output::Done)
                }
                Request::CloseMod => {
                    progress("saving checkpoint");
                    git_mod::checkpoint(root, self.cancelled)?;
                    Ok(Output::Done)
                }
                Request::ReopenMod => {
                    progress("restoring worktree");
                    git_mod::reopen(root, self.cancelled)?;
                    Ok(Output::Done)
                }
                Request::PruneMod => {
                    progress("pruning closed worktree");
                    git_mod::prune(root, self.cancelled)?;
                    Ok(Output::Done)
                }
                Request::CleanupMod => {
                    progress("cleaning up mod");
                    if root.join("git-mod.json").exists() {
                        git_mod::cleanup(root, self.cancelled)?;
                    } else {
                        crate::sandbox::delete(root)?;
                    }
                    Ok(Output::Done)
                }
                Request::InstallPackages(request) => {
                    let request = request.validate()?;
                    let vm = self
                        .vm
                        .as_mut()
                        .ok_or_else(|| io::Error::other("System packages require the mod VM."))?;
                    if vm.root().canonicalize()? != root.canonicalize()? {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "Tool cannot access another mod's VM.",
                        ));
                    }
                    progress(&format!(
                        "installing {} system packages",
                        request.packages.len()
                    ));
                    vm.install_packages(&request, self.cancelled)
                        .map(Output::Text)
                }
            }
        })();
        progress("");
        let status = match &result {
            Ok(_) => "ok",
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => "denied",
            Err(_) if self.cancelled.load(Ordering::Relaxed) => "cancelled",
            Err(_) => "error",
        };
        // A completed side effect stays successful if recording its result fails.
        let _ = self.record(tool, id, status, started.elapsed().as_millis());
        result
    }

    fn record(&self, tool: Option<Tool>, id: u128, status: &str, duration: u128) -> io::Result<()> {
        let Some(root) = &self.context.root else {
            return Ok(());
        };
        if !root.exists() {
            return Ok(());
        }
        let _guard = LOG
            .lock()
            .map_err(|error| io::Error::other(error.to_string()))?;
        let path = root.join("tools.jsonl");
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }
        writeln!(
            file,
            "{}",
            json!({"id":id.to_string(),"tool":tool.map_or("unknown", Tool::name),
            "mod_id":self.context.mod_id,"worker_id":self.context.worker.map(|(id,_)|id),
            "caller":self.context.worker.map_or("harness", |(_,role)|role.name()),"status":status,"duration_ms":duration})
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::test_support::TestData;

    fn fixture() -> (TestData, Context, PathBuf) {
        let (data, repo, root, gh) = git_mod::tests::fixture();
        let mut store = data.store();
        let project = store.load_project(&repo).unwrap().id;
        let mut code_mod = store.create_mod(project, "A small change").unwrap();
        store.save_git_root(code_mod.id, &root).unwrap();
        code_mod.git_root = Some(root);
        (data, Context::harness(&repo, &code_mod), gh)
    }

    fn worker_context(context: &Context, role: Role) -> Context {
        Context {
            provider: "codex".into(),
            muse: false,
            database: None,
            tasks: Vec::new(),
            mod_id: context.mod_id,
            worker: Some((9, role)),
            root: context.root.clone(),
            project: None,
            description: String::new(),
            verified: None,
        }
    }

    fn records(root: &Path) -> Vec<Value> {
        fs::read_to_string(root.join("tools.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn project_sync_has_no_mod_and_cannot_run_mod_operations() {
        let (data, repo, _, target) = crate::git_sync::tests::remote_change();
        let root = data.0.join("project-sync");
        let context = Context::project(&repo, root.clone());
        let flag = AtomicBool::new(false);
        let mut tools = Dispatcher::new(&context, None, &flag);
        tools.execute(Request::SyncProject, |_| {}).unwrap();
        assert_eq!(
            git_mod::git(&root, &repo, &["rev-parse", "HEAD"], &flag).unwrap(),
            target
        );
        assert_eq!(
            tools
                .execute(Request::CleanupMod, |_| {})
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(advertised(&context).is_empty());
        assert!(records(&root).iter().all(|entry| entry["mod_id"].is_null()));
    }

    #[test]
    fn workers_cannot_discover_or_invoke_host_operations() {
        let (_data, context, _) = fixture();
        let flag = AtomicBool::new(false);
        assert!(advertised(&context).is_empty());
        let worker = worker_context(&context, Role::Executor);
        assert_eq!(
            advertised(&worker)
                .iter()
                .map(|tool| tool["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [packages::TOOL]
        );
        let mut tools = Dispatcher::new(&worker, None, &flag);
        for name in [
            "sync_project",
            "initialize_project",
            "adopt_snapshot",
            "connect_repository",
            "create_worktree",
            "publish_pr",
            "prepare_edits",
            "cleanup_mod",
            "close_mod",
            "reopen_mod",
            "prune_mod",
            "check_target",
            "update_target",
            "finish_update",
            "unknown",
        ] {
            let error = tools.worker_call(name, json!({}), |_| {}).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        }
        assert_eq!(
            tools
                .execute(Request::CreateWorktree, |_| {})
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(!git_mod::checkout(context.root().unwrap()).exists());
        let log = records(context.root().unwrap());
        assert_eq!(
            log.iter()
                .filter(|entry| entry["status"] == "denied")
                .count(),
            16
        );
        assert!(
            log.iter()
                .all(|entry| entry["worker_id"] == 9 && entry["mod_id"] == json!(context.mod_id))
        );
        let planner = worker_context(&context, Role::Planner);
        assert!(advertised(&planner).is_empty());
        assert_eq!(
            Dispatcher::new(&planner, None, &flag)
                .worker_call(packages::TOOL, json!({}), |_| {})
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn invalid_inputs_cannot_select_a_workspace_or_bypass_validation() {
        let (_data, context, _) = fixture();
        let flag = AtomicBool::new(false);
        let worker = worker_context(&context, Role::Executor);
        let mut tools = Dispatcher::new(&worker, None, &flag);
        for extra in ["mod_id", "workspace", "command"] {
            let mut arguments = json!({"packages":["jq"],"reason":"fixture-secret"});
            arguments[extra] = json!("another-mod");
            assert!(
                tools
                    .worker_call(packages::TOOL, arguments, |_| {})
                    .is_err()
            );
        }
        let request = packages::Request {
            packages: vec!["-y".into()],
            reason: "fixture-secret".into(),
        };
        assert!(
            tools
                .execute(Request::InstallPackages(request), |_| {})
                .unwrap_err()
                .to_string()
                .contains("package names")
        );
        assert!(
            tools
                .worker_call(
                    packages::TOOL,
                    json!({"packages":["jq"],"reason":"fixture-secret"}),
                    |_| {}
                )
                .unwrap_err()
                .to_string()
                .contains("mod VM")
        );
        let log = fs::read_to_string(context.root().unwrap().join("tools.jsonl")).unwrap();
        assert!(!log.contains("fixture-secret") && !log.contains("another-mod"));
        assert!(!context.root().unwrap().join("packages.jsonl").exists());
    }

    #[test]
    fn cancellation_is_recorded_without_starting_the_adapter() {
        let (_data, context, _) = fixture();
        let flag = AtomicBool::new(true);
        assert_eq!(
            Dispatcher::new(&context, None, &flag)
                .execute(Request::CreateWorktree, |_| {})
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(!context.root().unwrap().join("git-mod.json").exists());
        let log = records(context.root().unwrap());
        assert_eq!(log.len(), 2);
        assert_eq!(log[0]["id"], log[1]["id"]);
        assert_eq!(log[1]["status"], "cancelled");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(context.root().unwrap().join("tools.jsonl"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn publication_requires_verified_source_and_reuses_saved_recovery() {
        let (data, mut context, gh) = fixture();
        let flag = AtomicBool::new(false);
        let root = context.root().unwrap().to_owned();
        Dispatcher::new(&context, None, &flag)
            .execute(Request::CreateWorktree, |_| {})
            .unwrap();
        workspace::create(&git_mod::checkout(&root), &root).unwrap();
        fs::write(root.join("work/a.txt"), "edited\n").unwrap();
        git_mod::git(
            &root,
            context.project.as_ref().unwrap(),
            &["push", "origin", "main"],
            &flag,
        )
        .unwrap();
        let mut tools = Dispatcher::new(&context, None, &flag);
        tools.github_cli = &gh;
        assert!(
            tools
                .execute(Request::PublishPr { draft: false }, |_| {})
                .unwrap_err()
                .to_string()
                .contains("verified source")
        );
        assert_eq!(git_mod::load(&root).unwrap().phase, "ready");
        context.verified = Some(workspace::review(&root).unwrap().fingerprint);
        let mut tools = Dispatcher::new(&context, None, &flag);
        tools.github_cli = &gh;
        fs::write(data.0.join("fail-once"), "").unwrap();
        assert!(
            tools
                .execute(Request::PublishPr { draft: false }, |_| {})
                .is_err()
        );
        assert_eq!(git_mod::load(&root).unwrap().phase, "pushed");
        let output = tools
            .execute(Request::PublishPr { draft: false }, |_| {})
            .unwrap();
        assert_eq!(
            output.message(),
            "https://github.com/fixture/project/pull/1"
        );
        tools.execute(Request::CleanupMod, |_| {}).unwrap();
        assert!(!git_mod::checkout(&root).exists());
        assert_eq!(
            fs::read_to_string(data.0.join("gh-args"))
                .unwrap()
                .lines()
                .filter(|line| *line == "create")
                .count(),
            1
        );
        assert!(
            records(&root)
                .iter()
                .any(|entry| entry["tool"] == "cleanup_mod" && entry["status"] == "ok")
        );
    }

    #[test]
    #[ignore = "installs jq through the dispatcher in a temporary Apple Container VM"]
    fn package_calls_are_bound_to_the_workers_vm() {
        let (data, context, _) = fixture();
        let flag = AtomicBool::new(false);
        let root = context.root().unwrap().to_owned();
        Dispatcher::new(&context, None, &flag)
            .execute(Request::CreateWorktree, |_| {})
            .unwrap();
        workspace::create(&git_mod::checkout(&root), &root).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> io::Result<()> {
                let mut vm = Sandbox::prepare(&root, &flag, |label| eprintln!("{label}"))?;
                let mut other = worker_context(&context, Role::Executor);
                other.mod_id = other.mod_id.map(|id| id + 1);
                other.root = Some(data.0.join("other-mod"));
                fs::create_dir_all(other.root().unwrap())?;
                let arguments = json!({"packages":["jq"],"reason":"Read fixture JSON"});
                let error = Dispatcher::new(&other, Some(&mut vm), &flag)
                    .worker_call(packages::TOOL, arguments.clone(), |_| {})
                    .unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
                assert!(!root.join("packages.jsonl").exists());
                let worker = worker_context(&context, Role::Executor);
                let output = Dispatcher::new(&worker, Some(&mut vm), &flag).worker_call(
                    packages::TOOL,
                    arguments,
                    |label| eprintln!("{label}"),
                )?;
                assert!(output.message().contains("jq"));
                let check = crate::execution::Check { task: None, check: "Package works; normal permissions remain restricted".into(),
                command: vec!["/bin/sh".into(), "-c".into(), "/usr/bin/jq --version && test \"$(cat a.txt)\" = original && ! touch /usr/local/bin/sprowt-tool-canary".into()] };
                assert_eq!(vm.verify(&[check], &flag)?.1[0].exit_code, Some(0));
                drop(vm);
                Dispatcher::new(&context, None, &flag).execute(Request::CleanupMod, |_| {})?;
                assert!(!root.join("vm.json").exists());
                let log = records(&root);
                assert!(log.iter().any(|entry| entry["tool"] == packages::TOOL
                    && entry["worker_id"] == 9
                    && entry["status"] == "ok"));
                Ok(())
            },
        ));
        crate::sandbox::delete(&root).unwrap();
        result.unwrap().unwrap();
    }
}
