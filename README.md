# sprowt harness

A local CLI for building software with coding agents.

## Why I’m building this

I want to understand how coding harnesses work, so I’m building one layer at a time. The goal is to have Codex and Muse work in parallel, share context and bring their work together in a local sandbox. Better code, less waiting.

The harness works independently of sprowt.finance. I plan to open source it as it develops.

## What works today

- **Code mods:** separate tasks, each with its own conversation, queue and draft.
- **Queues:** edit, reorder or remove instructions before they run.
- **Steering:** send instructions into the worker’s active turn.
- **Codex:** one worker per mod, with streamed replies. Workers in different mods can run in parallel.
- **Saved state:** reopen a project and pick up where you left off.

### Current limits

Workers are **read-only**: they can inspect the project and answer questions. Tool commands cannot write files or access the network. This uses Codex’s local OS sandbox; separate VM isolation comes later.

Verified on macOS with Codex CLI **0.159.2**. Run one harness instance per project.

## Get started

You need [Rust](https://rustup.rs) and [Codex CLI](https://github.com/openai/codex). Sign in to Codex with your ChatGPT subscription using `codex login`.

Install from this repository:

```sh
cargo install --path .
```

Then open a terminal in the project you want to work on and run:

```sh
sprowt-harness
```

1. Name your first code mod.
2. Write an instruction and press **Enter** to queue it.
3. Press **Ctrl+R** to start Codex. Press it again to interrupt and pause.

Nothing runs automatically on launch. Use `sprowt-harness --no-motion` to turn off animations.

## Controls

| Key | Action |
| --- | --- |
| Enter | Queue an instruction |
| Ctrl+J | Newline |
| Ctrl+P | Open code mods; switch or create one |
| Ctrl+Q | Open the queue |
| Ctrl+R | Run or stop the current mod’s worker |
| Fn + ↑ / ↓ on Mac | Scroll the conversation |
| Esc | Back, or quit from the conversation |
| Ctrl+C | Quit |

### In dialogs

- **Code mods:** arrows select, Enter opens, `d` deletes with confirmation.
- **Queue:** arrows select, Enter edits, `k` / `j` move, `d` removes.
- **Steering:** Space marks queued instructions; `s` sends the marked ones, or the selected one. If the worker is idle, they wait for its next turn.

Deleting a mod stops its worker and removes its saved harness data. Codex’s own conversation records remain.

## Local data

Mods, conversations, queues and drafts save automatically in SQLite. On macOS:

```text
~/Library/Application Support/sprowt-harness/state.db
```

Reopen from the same project root to restore them. Codex uses your existing login; the harness does not copy credentials into its database. Tool commands are blocked from reading Codex credentials or the macOS Keychain.

## What’s next

Code editing, stronger sandbox isolation and multiple Codex/Muse workers in each mod. Shared context, memory, MCPs, skills and the Laya coordinator follow in small batches.

## Development

```sh
cargo run
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
