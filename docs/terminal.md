# Terminal and companion

The interface keeps the project and worker status at the top, the conversation left aligned and the composer at the bottom.

```text
[sprowt companion]  project + worker model + effort + activity

<codemod/>  ◇ selected mod    2 workers running

> your message

▤ codex · planner
ctrl+o ▸ show plan details
execution · numbered tasks

◆ codex · executor · w1 · model · effort
reply

queue / waiting steering, when present

[message composer]
keyboard hints
```

User messages have a `>` prefix and a subtle background. Agent messages show their provider, role, worker ID, model and configured reasoning effort when reported by Codex. One blank row separates messages.

The worker status uses a small dot spinner during connection, execution and verification. Each running task shows its worker ID and uses the same spinner in the plan, including while its checks run. Completed tasks and replies stay still. `--no-motion` uses a static activity glyph.

Codex’s configured effort is used when available. When unset, the harness reads the model’s default from Codex’s catalog and sends it explicitly with new turns. Replies save that effort for reopening. Older replies without recorded effort show **effort unknown**.

Plans show task titles, outcomes and dependencies first. **Ctrl+O · show/hide plan details** sits beside the heading. It expands file scopes, completion checks and planner model details. Check commands appear in indented blocks; multiline code keeps its source indentation, and wrapped lines stay inside the block. Expanding keeps the heading in view and leaves the draft and queue intact. Details start collapsed when you switch mods or reopen the project.

The same toggle reveals saved [worker messages](coordination.md). Questions for you stay visible with the sender and message ID; the composer says **answer #ID** and Enter saves your reply. Questions arriving while you type preserve the draft. Routine coordination stays collapsed.

Completed planner chatter stays saved but is hidden from the transcript. The plan shows task progress; details show check results. **Ctrl+D** opens a full-width diff. Use **p** to publish verified changes as a PR, or **Ctrl+S** from the conversation. Snapshot mods first offer Git adoption. See [Plan execution](execution.md).

Worktree creation, diff loading, publication and removal show a spinner in the header. A saved PR link appears in the conversation. Failed operations show **Ctrl+R · retry Git operation**. A published mod keeps its composer for requesting edits on the same PR.

Codemod, queue and project setup dialogs share spacing and keyboard hints. Empty codemod lists say **No active codemods** or **No closed codemods**, with a new-codemod action. Hints adapt to the available actions and terminal width. Plans and publish confirmations omit repeated VM guidance; the conversation keeps room for tasks and replies.

Git setup shows the starting file list. GitHub setup uses Tab to choose connect existing or create private, then confirms `owner/repository` before contacting GitHub. Queue management appears only with queued instructions; run, stop and retry appear when relevant.

## Commands

| Command | Action |
| --- | --- |
| `sprowt-harness` | Open the current project |
| `sprowt-harness setup` | Configure Jev from the harness’s `.env.local` |
| `sprowt-harness --closed-worktree-days 0` | Keep closed worktrees indefinitely |
| `sprowt-harness --no-motion` | Open with animations disabled |
| `sprowt-harness --help` | Show available options |
| `sprowt-harness --version` | Show the installed version |

## Controls

| Key | Action |
| --- | --- |
| Enter | Create a described mod, answer a highlighted question, queue a message or save an edit |
| Ctrl+J | Add a newline |
| Ctrl+P | Switch, create, close, reopen or delete a codemod |
| Ctrl+Q | Manage queued instructions |
| Ctrl+R | Run, stop, retry or reopen a closed codemod |
| Ctrl+S | Publish verified changes as a PR |
| Ctrl+U | Sync the project branch and check the codemod's target |
| Ctrl+O | Show or hide plan details and worker messages |
| Ctrl+D | Review working-folder changes |
| Fn + ↑ / ↓ on Mac | Scroll the conversation |
| Page Up / Page Down | Scroll on keyboards with those keys |
| Esc | Back from a dialog; quit from the conversation |
| Ctrl+C | Quit |

Pasted text keeps its line breaks. Dialog actions are covered in [Codemods and messages](codemods.md).

Target updates show an **integration plan** with the same task progress and details toggle. The status names target changes or a merged PR; publication stays unavailable until combined checks pass. See [Worktrees and PRs](git-workflow.md#when-another-codemod-merges).

Project sync runs independently on startup and every 30 seconds, including with no codemods and `--no-motion`. **Ctrl+U** also works in the new-mod screen and picker. An unsafe update leaves files intact and shows the cause beside the project heading.

## Sprowt companion

The companion uses terminal cells and glyphs, so it needs no image support. It sits beside the project heading in an 8-column, 4-row area.

Sprowt follows the selected codemod:

| State | Expression and movement |
| --- | --- |
| Idle | Double blink, sideways glances and a curious face |
| Planning | Thoughtful eyes and an occasional leaf twitch |
| Working or checking | Focused eyes and rhythmic leaf movement |
| Changes ready | One short bounce and wink, then a happy face |
| Blocked or failed | A concerned face, held still |

The body keeps its shape and footprint. Switching codemods resets the animation to that mod’s state. Queueing or editing an instruction can trigger a brief idle celebration; it never overrides active work or an error. Closing or stopping returns Sprowt to idle. Worker progress remains in the status text beside it.

Run `sprowt-harness --no-motion` for a still companion and no entrance animation. On exit, the harness restores normal terminal input, paste handling and the previous screen.
