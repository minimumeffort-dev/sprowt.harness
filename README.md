# sprowt harness

A local CLI for building software with coding agents.

## Why I’m building this

I want to understand how coding harnesses work, so I’m building one layer at a time. The goal is to have Codex and Muse work in parallel, share context and bring their work together in a local sandbox. Better code, less waiting.

The harness works independently of sprowt.finance. I plan to open source it as it develops.

## What works today

- **Code mods:** separate tasks, each with its own conversation, queue and draft.
- **Queues:** edit, reorder or remove instructions before they run.
- **Steering:** send instructions into the worker’s active turn.
- **Planning:** a new mod’s description starts a Codex planner. Its saved plan lists tasks, file scopes, dependencies and completion checks.
- **Workers:** separate Codex planner and executor conversations. Different mods can run in parallel.
- **Saved state:** reopen a project and pick up where you left off.

### Current limits

Workers are **read-only**: they inspect the project, plan and answer questions. Executing the plan as code changes comes next. Tool commands cannot write files or access the network. This uses Codex’s local OS sandbox; separate VM isolation comes later.

Verified on macOS with Codex CLI **0.159.2**. Run one harness instance per project.

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

This downloads [Laya](https://huggingface.co/convaiinnovations/laya) into the harness’s local data directory. It runs locally to recommend the planner model and reasoning level: Sol medium for simple work, Sol high for complex work, Astra high for demanding work.

Routing is experimental: the stock checkpoint was uncertain or wrong in our small test. Uncertain results, missing Laya or a timeout use Sol high. Available models are checked against Codex’s catalog; access still depends on your account. Codex planning uses your subscription and an internet connection.

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

### In dialogs

- **Code mods:** arrows select, Enter opens, `d` deletes with confirmation.
- **Queue:** arrows select, Enter edits, `k` / `j` move, `d` removes.
- **Steering:** Space marks queued instructions; `s` sends the marked ones, or the selected one. If the worker is idle, they wait for its next turn.

Deleting a mod stops its workers and removes its saved harness data. Codex’s own conversation records remain.

## Local data

Mods, plans, model selections, conversations, queues and drafts save automatically in SQLite. On macOS:

```text
~/Library/Application Support/sprowt-harness/state.db
```

Reopen from the same project root to restore them. Codex uses your existing login; the harness does not copy credentials into its database. Tool commands are blocked from reading Codex credentials or the macOS Keychain.

## What’s next

Plan execution, code editing, stronger sandbox isolation and multiple Codex/Muse executors per mod. Shared context, memory, MCPs and skills follow in small batches.

## Development

```sh
cargo run
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
