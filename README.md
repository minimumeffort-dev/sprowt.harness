# sprowt harness

A local CLI for building software with coding agents.

## Why I’m building this

I want to understand how coding harnesses work, so I’m building one layer at a time. The goal is to have Codex and Muse work in parallel, share context and bring their work together in a local sandbox. Better code, less waiting.

The harness works independently of sprowt.finance. It is open source and still taking shape.

## What works today

- [Codemods and messages](docs/codemods.md): separate goals, conversations and drafts. Edit, reorder or remove queued instructions; steer active turns.
- [Planning and Laya](docs/planning.md): turn a mod’s description into a saved task plan with file scopes, dependencies and checks.
- [Worktrees and PRs](docs/git-workflow.md): build on separate branches, update from merged work and publish a PR. Keep editing, or close with a saved checkpoint.
- [Plan execution](docs/execution.md): up to two Codex executors work in parallel, with separate task folders and combined verification.
- [Local Linux sandbox](docs/sandbox.md): one Apple Container VM per executing mod, with local sockets for browser checks. Codex chooses runtimes and dependencies; the harness installs requested OS packages.
- [Workers and isolation](docs/workers.md): separate Codex planner and executor conversations. Workers within and across mods can run in parallel.
- [Harness tools](docs/tools.md): one dispatcher for Git, GitHub, cleanup and VM package setup, with caller checks and recorded activity.
- [Local state](docs/local-state.md): reopen a project and pick up where you left off.
- [Terminal and companion](docs/terminal.md): readable plans, worker model and effort, task spinners and a Sprowt pet that reacts to planning, work and results.

### Current limits

The planner uses a read-only host sandbox. Execution uses a Linux VM; PR publication needs your confirmation. Downloads use a host-controlled domain allowlist. iOS and macOS builds need a later macOS VM backend. Existing snapshot mods can be adopted into Git.

Verified on macOS with Codex CLI **0.159.2**. Run one harness instance per project.

## Architecture today

Rust owns the interface, scheduling, workers and saved state. A shared tool dispatcher validates harness-owned operations and routes them to host or VM adapters. A small Python helper runs Laya locally; Codex plans and edits.

```mermaid
flowchart TB
    goal["Describe a codemod"] --> sync["Check remote · safely update project branch"]
    sync --> setup["Git branch + worktree"]
    setup --> planner["Laya routes · Codex plans"]
    planner --> schedule["Rust scheduler · ready tasks"]
    schedule --> first["Codex worker 1 · own task worktree"]
    schedule --> second["Codex worker 2 · own task worktree"]
    first --> combine["Combine one result at a time · verify together"]
    second --> combine
    combine --> ready["Version ready"]
    ready --> edits["Send edits · plan the next round"]
    edits --> planner
    ready --> publish["Confirm publish · create or update PR"]
    publish --> ready
    upstream["Another PR merges"] --> update["Save checkpoint · combine latest target branch"]
    update --> verify["Codex resolves conflicts · Rust rechecks"]
    verify --> ready
```

Laya recommends the planner configuration. Codex uses your subscription and installs project dependencies in the VM. Rust reruns checks independently. The dispatcher keeps publication on the Mac and package setup in the worker’s VM. A separate Git repository inside the VM manages task branches without host credentials. Each mod runs up to two independent tasks at once. One VM controller serializes Git integration, checks and system package setup; each worker has its own runtime folder. Dependent tasks wait for verified prerequisites. Muse follows later.

Publishing keeps the VM and worktree for further edits. Closing saves source and task branches before removing the VM. Reopening restores the worktree; its next execution creates a fresh VM. Closed worktrees are pruned after 30 days, keeping the branch and history.

Merge PRs one at a time on GitHub. The harness checks target branches every 30 seconds while open. Other codemods finish their current work, save a checkpoint and update in their existing VM. A worker resolves text conflicts and rechecks the combined code. Open PRs become drafts during the update; Publish updates the same PR and marks it ready again. Product decisions pause for your input; binary conflicts need manual resolution.

## Get started

You need [Rust](https://rustup.rs), [Codex CLI](https://github.com/openai/codex) **0.159.2**, Apple silicon and macOS 26+. This connection needs a file-backed ChatGPT login on the Mac:

```sh
codex -c 'cli_auth_credentials_store="file"' login
```

Install and start [Apple Container](https://github.com/apple/container):

```sh
brew install container
container system start --enable-kernel-install
```

Install from this repository:

```sh
cargo install --path . --locked --force
```

Then open a terminal in the project you want to work on and run:

```sh
sprowt-harness
```

Start in an empty folder, an existing project or a Git repository. If Git has no commits, review and confirm the starting files. Before a new worktree, the harness fetches the current branch’s upstream and safely fast-forwards your project. Local edits and staging stay; conflicts stop creation. The codemod uses the updated committed source.

Publishing needs [GitHub CLI](https://cli.github.com/). Sign in with `gh auth login` and `gh auth setup-git`.

1. Describe a codemod and press **Enter**. Confirm Git setup if offered. Planning and execution start automatically.
2. Workers prepare a first version. **Ctrl+O** expands plan details; **Ctrl+R** stops or retries work.
3. Send a message to request edits. Messages sent during work wait for the next round; the queue also supports steering.
4. **Ctrl+D** reviews the diff. **Ctrl+S** starts publication; confirm the GitHub destination if needed, then the PR. Keep editing afterward to update the same PR.
5. In **Ctrl+P**, **c** closes with a checkpoint, **Tab** shows closed mods, **r** reopens and **d** deletes local data.

Reopening restores state; unfinished work waits for **Ctrl+R**. Finished versions can automatically update from the target branch. **Ctrl+U** checks immediately. Use `--no-motion` to disable animations, or `--closed-worktree-days 0` to keep closed worktrees indefinitely. A review worker comes later.

### Local model routing

Install [uv](https://docs.astral.sh/uv/getting-started/installation/), then run:

```sh
sprowt-harness setup
```

This downloads [Laya](https://huggingface.co/convaiinnovations/laya) locally to recommend a planner model and reasoning level. Routing is experimental; uncertain results or missing Laya use Sol high. See [Planning and Laya](docs/planning.md) for model choices and limitations.

## Controls

| Key | Action |
| --- | --- |
| Enter | Send edits or queue an instruction during work |
| Ctrl+J | Newline |
| Ctrl+P | Switch, create, close, reopen or delete a codemod |
| Ctrl+Q | Open the queue |
| Ctrl+R | Run, stop, retry or reopen a closed codemod |
| Ctrl+S | Publish verified changes as a PR |
| Ctrl+U | Check the target branch and update when current work finishes |
| Ctrl+O | Show or hide plan details |
| Ctrl+D | Review working-folder changes |
| Fn + ↑ / ↓ on Mac | Scroll the conversation |
| Esc | Back, or quit from the conversation |
| Ctrl+C | Quit |

Dialog actions and steering are covered in [Codemods and messages](docs/codemods.md).

## Local data

Mods, plans, task progress, check results, conversations, queues and drafts save automatically in SQLite. Worktrees and source exports live beside the database. On macOS:

```text
~/Library/Application Support/sprowt-harness/state.db
```

Reopen from the same project root to restore them. Credentials stay with Codex. See [Local state](docs/local-state.md) for storage and [Workers](docs/workers.md) for credential isolation.

Git ignores local environment files, credentials, logs and databases. Use placeholders in `.env.example` or `.env.sample`.

## What’s next

Muse, direct worker communication and a review worker per mod. Shared context, memory, MCPs and skills follow in small batches.

## Development

```sh
cargo run
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
