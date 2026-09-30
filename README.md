# sprowt harness

A local CLI for building software with coding agents.

## Why I’m building this

I want to understand how coding harnesses work, so I’m building one layer at a time. The goal is to have Codex and Muse work in parallel, share context and bring their work together in a local sandbox. Better code, less waiting.

The harness works independently of sprowt.finance. I plan to open source it as it develops.

## What works today

- [Code mods and messages](docs/code-mods.md): separate goals, conversations and drafts. Edit, reorder or remove queued instructions; steer active turns.
- [Planning and Laya](docs/planning.md): turn a mod’s description into a saved task plan with file scopes, dependencies and checks.
- [Plan execution](docs/execution.md): one executor follows task dependencies in its own working folder. Verify, review the diff, then apply.
- [Workers and isolation](docs/workers.md): separate Codex planner and executor conversations. Different mods can run in parallel.
- [Local state](docs/local-state.md): reopen a project and pick up where you left off.
- [Terminal and companion](docs/terminal.md): clear message ownership, inline plan review and the animated Sprowt pet.

### Current limits

The planner is read-only. The executor edits a separate snapshot and runs checks there; applying changes needs your confirmation. Tool networking and dependency installation are disabled. This uses Codex’s local OS sandbox; a local VM comes later.

Verified on macOS with Codex CLI **0.159.2**. Run one harness instance per project.

## Architecture today

Rust owns the interface, task scheduling, worker lifecycle and saved state. A small Python helper runs Laya locally. Codex plans and edits; Rust reruns verification commands before offering changes for review.

```mermaid
flowchart TB
    terminal["You · terminal CLI"] <-->|"instructions / progress"| harness["Rust harness"]
    harness <-->|"save / restore"| state[("SQLite · plans and task progress")]
    harness <-->|"model recommendation"| laya["Local Laya"]
    harness --> planner["Codex planner · read-only"]
    planner -->|"inspect"| project["Your project"]
    harness --> executor["Codex executor · OS sandbox"]
    project -->|"snapshot including uncommitted code"| work["Separate folder per mod"]
    executor <-->|"edit / test"| work
    work --> review["Diff and check results"]
    review -->|"you confirm apply"| project
```

Laya recommends the planner configuration. Codex inference uses your subscription and the internet. Tasks run sequentially within a mod. Different mods can run in parallel. Muse and a local VM sandbox are future layers.

## Get started

You need [Rust](https://rustup.rs) and [Codex CLI](https://github.com/openai/codex). Sign in to Codex with your ChatGPT subscription using `codex login`.

Install from this repository:

```sh
cargo install --path . --locked --force
```

Then open a terminal in the project you want to work on and run:

```sh
sprowt-harness
```

1. Describe your code mod and press **Enter**. Planning starts automatically.
2. Review the numbered tasks. **Ctrl+O** shows files, checks and model details. Write follow-up instructions and press **Enter** to queue them.
3. Press **Ctrl+R** to execute the plan. No extra message needed. Press it again to pause.
4. When changes are ready, **Ctrl+D** opens the diff. Press **a**, then **Enter** to apply.

**Ctrl+R** also stops or retries unfinished planning. Reopening a project restores its state without starting workers. Existing mods keep their original workflow. Use `--no-motion` to turn off animations.

### Local model routing

Install [uv](https://docs.astral.sh/uv/getting-started/installation/), then run:

```sh
sprowt-harness setup
```

This downloads [Laya](https://huggingface.co/convaiinnovations/laya) locally to recommend a planner model and reasoning level. Routing is experimental; uncertain results or missing Laya use Sol high. See [Planning and Laya](docs/planning.md) for model choices and limitations.

## Controls

| Key | Action |
| --- | --- |
| Enter | Queue an instruction |
| Ctrl+J | Newline |
| Ctrl+P | Open code mods; switch or create one |
| Ctrl+Q | Open the queue |
| Ctrl+R | Run, stop or retry the current worker |
| Ctrl+O | Show or hide plan details |
| Ctrl+D | Review working-folder changes |
| Fn + ↑ / ↓ on Mac | Scroll the conversation |
| Esc | Back, or quit from the conversation |
| Ctrl+C | Quit |

Dialog actions and steering are covered in [Code mods and messages](docs/code-mods.md).

## Local data

Mods, plans, task progress, check results, conversations, queues and drafts save automatically in SQLite. Working folders live beside the database. On macOS:

```text
~/Library/Application Support/sprowt-harness/state.db
```

Reopen from the same project root to restore them. Credentials stay with Codex. See [Local state](docs/local-state.md) for storage and [Workers](docs/workers.md) for credential isolation.

## What’s next

Stronger sandbox isolation and multiple Codex/Muse executors per mod. Shared context, memory, MCPs and skills follow in small batches.

## Development

```sh
cargo run
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
