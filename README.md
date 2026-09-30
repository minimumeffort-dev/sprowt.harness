# sprowt harness

A local CLI for building software with coding agents.

## Why I’m building this

I want to understand how coding harnesses work, so I’m building one layer at a time. The goal is to have Codex and Muse work in parallel, share context and bring their work together in a local sandbox. Better code, less waiting.

The harness works independently of sprowt.finance. I plan to open source it as it develops.

## What works today

- [Code mods and messages](docs/code-mods.md): separate goals, conversations and drafts. Edit, reorder or remove queued instructions; steer active turns.
- [Planning and Laya](docs/planning.md): turn a mod’s description into a saved task plan with file scopes, dependencies and checks.
- [Workers and isolation](docs/workers.md): separate Codex planner and executor conversations. Different mods can run in parallel.
- [Local state](docs/local-state.md): reopen a project and pick up where you left off.
- [Terminal and companion](docs/terminal.md): keyboard controls, shared dialog design and the animated Sprowt pet.

### Current limits

Workers are **read-only**: they inspect the project, plan and answer questions. Executing the plan as code changes comes next. Tool commands cannot write files or access the network. This uses Codex’s local OS sandbox; separate VM isolation comes later.

Verified on macOS with Codex CLI **0.159.2**. Run one harness instance per project.

## Architecture today

Rust owns the interface, worker lifecycle, plan validation and saved state. A small Python helper runs Laya locally. Codex handles model requests and read-only project tools.

```mermaid
flowchart TB
    terminal["You · terminal CLI"] --> harness["Rust harness"]
    harness <-->|"save / restore"| state[("SQLite · project state")]
    harness <-->|"request / model recommendation"| laya["Local Laya · Python helper"]
    harness <-->|"instructions / replies"| codex["Codex CLI · planner and executor"]
    codex -->|"tool commands"| sandbox["Read-only OS sandbox"]
    sandbox -->|"read"| project["Project source and docs"]
```

Laya recommends the planner configuration. Codex inference uses your subscription and the internet. Parallel task execution, Muse and a local VM sandbox are future layers.

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
2. Read the plan. Write follow-up instructions and press **Enter** to queue them.
3. Press **Ctrl+R** to start the executor. Press it again to pause.

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
| Fn + ↑ / ↓ on Mac | Scroll the conversation |
| Esc | Back, or quit from the conversation |
| Ctrl+C | Quit |

Dialog actions and steering are covered in [Code mods and messages](docs/code-mods.md).

## Local data

Mods, plans, model selections, conversations, queues and drafts save automatically in SQLite. On macOS:

```text
~/Library/Application Support/sprowt-harness/state.db
```

Reopen from the same project root to restore them. Credentials stay with Codex. See [Local state](docs/local-state.md) for storage and [Workers](docs/workers.md) for credential isolation.

## What’s next

Plan execution, code editing, stronger sandbox isolation and multiple Codex/Muse executors per mod. Shared context, memory, MCPs and skills follow in small batches.

## Development

```sh
cargo run
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
