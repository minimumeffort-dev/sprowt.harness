# Terminal and companion

The interface keeps the project and worker status at the top, the conversation left aligned and the composer at the bottom.

```text
[sprowt companion]  project + worker model + effort + activity

<codemod/>  ◇ selected mod

> your message

▤ codex · planner
ctrl+o ▸ show plan details
execution · numbered tasks

◆ codex · executor · model · effort
reply

queue / waiting steering, when present

[message composer]
keyboard hints
```

User messages have a `>` prefix and a subtle background. Agent messages show their provider, role, model and configured reasoning effort when reported by Codex. One blank row separates messages.

The worker status uses a small dot spinner during connection, execution and verification. The running task uses the same spinner in the plan, including while its checks run. Completed tasks and replies stay still. `--no-motion` uses a static activity glyph.

Codex’s configured effort is used when available. When unset, the harness reads the model’s default from Codex’s catalog and sends it explicitly with new turns. Replies save that effort for reopening. Older replies without recorded effort show **effort unknown**.

Plans show task titles, outcomes and dependencies first. **Ctrl+O · show/hide plan details** sits beside the heading. It expands file scopes, completion checks and planner model details. Check commands appear in indented blocks; multiline code keeps its source indentation, and wrapped lines stay inside the block. Expanding keeps the heading in view and leaves the draft and queue intact. Details start collapsed when you switch mods or reopen the project.

Completed planner chatter stays saved but is hidden from the transcript. The plan shows task progress; details show check results. **Ctrl+D** opens a full-width diff. Use **p** to publish verified changes as a PR, or **Ctrl+S** from the conversation. Snapshot mods first offer Git adoption. See [Plan execution](execution.md).

Worktree creation, diff loading, publication and removal show a spinner in the header. A saved PR link appears in the conversation. Failed operations show **Ctrl+R · retry Git operation**. A published mod keeps its composer for requesting edits on the same PR.

Codemod, queue and project setup dialogs share spacing and keyboard hints. Git setup shows the starting file list. GitHub setup uses Tab to choose connect existing or create private, then confirms `owner/repository` before contacting GitHub. Hints adapt to the current view and terminal width. Queue management appears only with queued instructions; run, stop and retry appear when relevant.

## Commands

| Command | Action |
| --- | --- |
| `sprowt-harness` | Open the current project |
| `sprowt-harness setup` | Install the local Laya runtime and model |
| `sprowt-harness --closed-worktree-days 0` | Keep closed worktrees indefinitely |
| `sprowt-harness --no-motion` | Open with animations disabled |
| `sprowt-harness --help` | Show available options |
| `sprowt-harness --version` | Show the installed version |

## Controls

| Key | Action |
| --- | --- |
| Enter | Create a described mod, queue a message or save an edit |
| Ctrl+J | Add a newline |
| Ctrl+P | Switch, create, close, reopen or delete a codemod |
| Ctrl+Q | Manage queued instructions |
| Ctrl+R | Run, stop, retry or reopen a closed codemod |
| Ctrl+S | Publish verified changes as a PR |
| Ctrl+O | Show or hide saved plan details |
| Ctrl+D | Review working-folder changes |
| Fn + ↑ / ↓ on Mac | Scroll the conversation |
| Page Up / Page Down | Scroll on keyboards with those keys |
| Esc | Back from a dialog; quit from the conversation |
| Ctrl+C | Quit |

Pasted text keeps its line breaks. Dialog actions are covered in [Codemods and messages](codemods.md).

## Sprowt companion

The companion uses terminal cells and glyphs, so it needs no image support. It sits beside the project heading in an 8-column, 4-row area.

It blinks, nods, hops and changes expressions. Creating a mod, queueing a message or saving a queue edit triggers a brief celebration. Worker progress appears in the status text beside it.

Run `sprowt-harness --no-motion` for a still companion and no entrance animation. On exit, the harness restores normal terminal input, paste handling and the previous screen.
