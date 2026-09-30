# Workers and isolation

The harness starts Codex through `codex app-server --listen stdio://`. Rust sends JSON-RPC requests over standard input and receives replies and events over standard output.

```mermaid
flowchart TB
    harness["Rust harness"] <-->|"local JSON-RPC"| codex["Codex app-server"]
    login["Existing ChatGPT login"] -->|"authentication"| codex
    codex <-->|"model requests / responses"| models["OpenAI models · internet"]
    codex -->|"tool commands"| sandbox["OS sandbox · role-specific permissions"]
    sandbox -->|"planner reads"| project["Project source and docs"]
    sandbox <-->|"executor edits / tests"| work["Mod working folder"]
```

## Login

Install Codex CLI and run `codex login`. Use your ChatGPT subscription. The harness checks that login type at startup; API-key login is not supported by this worker setup.

Codex handles authentication. The harness database stores conversation IDs and delivery state, without copying login credentials.

## Permissions today

Planner tools can read the project and required runtime files. Executor tools can read and write the mod’s working folder, with access to required runtimes. The original project is denied to executor tools. Neither role can access the tool network. The Codex credential directory and macOS Keychain directory are denied to these commands; their environment is restricted.

Host MCP servers, apps, plugins, hooks, browser tools and Codex delegation are disabled for these workers. Startup checks the permission boundary and that MCP tools are disabled before a worker can run. A failed check stops startup.

Codex’s trusted app-server uses the login and network for inference. Its tool commands run inside the restricted OS sandbox. This is the current isolation boundary; VM isolation comes later.

## Run, pause, resume

- A new mod starts its planner automatically.
- After planning, **Ctrl+R** starts or pauses plan execution. Rust selects tasks; queued instructions run between tasks. See [Plan execution](execution.md) for verification and applying changes.
- Reopening restores saved state. **Ctrl+R** reconnects a worker to its saved Codex conversation.
- Deleting a mod or quitting asks Codex to interrupt turns and terminate background commands, then stops the server and remaining child processes.

## Delivery and recovery

Each queued or steering instruction has a stable delivery ID. Before sending, the harness saves it as pending. Once Codex accepts it, a transaction moves the instruction into history and clears pending state.

On reconnect, Codex’s conversation records are checked for that ID. If delivery cannot be confirmed, the instruction is retained and automatic retry pauses. This reduces duplicate submissions; acceptance still does not mean the turn completed successfully. Tasks keep their delivery IDs, Codex turn IDs, status and checks. Only completed turns with passing verification can finish a task. If Codex has no saved file for an idle conversation, the harness starts a fresh one; uncertain task delivery never uses that fallback.

Currently verified on macOS with Codex CLI **0.159.2**. Muse, peer communication and multiple executors within a mod are future work.
