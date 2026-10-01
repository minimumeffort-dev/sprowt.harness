# Code mods and messages

A code mod is one goal in one project. It owns its description, conversation, plan, queue, draft and execution workspace. New Git mods also own a branch and worktree.

```mermaid
flowchart TB
    project["Project folder"] --> first["Code mod A"]
    project --> second["Code mod B"]
    first --> context["Own description, plan and messages"]
    first --> worktree["Own Git branch and worktree"]
    worktree --> planner["Codex planner · read-only"]
    planner --> executor["Codex executor · follows the plan"]
    executor --> vm["Own Linux VM · source copy and runtime"]
```

The current harness supports one planner and one executor per mod. Workers in different mods can run at the same time. Planners read the mod worktree; executors work on its source copy in Linux. Multiple executors per mod come later. See [Plan execution](execution.md).

Publishing a PR removes the mod’s VM and worktree. Its branch, plan, messages, draft and exported source remain. Existing mods and non-Git projects keep local apply.

## Create, switch, delete

- **Create:** open **Ctrl+P**, select **New code mod**, describe the change and press Enter. In Git projects, a worktree starts from committed `HEAD`, then planning starts. The first line becomes the title; the full description is saved.
- **Switch:** open **Ctrl+P**, select a mod and press Enter. Its conversation and draft return. Other mods’ workers keep running.
- **Delete:** select a mod, press `d`, then Enter to confirm. Git mods save unfinished source changes as a draft PR before removing the VM, worktree and local history. A publication failure retains the mod. Empty mods need no PR. Legacy and non-Git mods remove their unapplied work directly.

See [Worktrees and PRs](git-workflow.md) for login, branch targets and recovery. Published Git mods retain their history; create a new mod for further changes.

The first launch in an empty project opens creation directly. Ctrl+J adds a newline. Esc cancels creation, or quits when no mod exists.

## Draft, queue, conversation

A draft is what you are typing. Enter saves it to the queue. An enabled executor takes queued instructions in order, one turn at a time. Accepted instructions and replies appear in the conversation.

Open **Ctrl+Q** to manage pending instructions:

| Key | Action |
| --- | --- |
| ↑ / ↓ | Select |
| Enter | Edit; Enter saves, Esc cancels |
| `k` / `j` | Move up / down |
| `d` | Remove |
| Space | Mark or unmark |
| `s` | Steer with marked instructions, or the selected one |
| Esc | Return to the composer |

Editing preserves your composer draft. Removing the last item closes the queue dialog. An instruction awaiting delivery confirmation cannot be edited, removed or steered.

## Steer an active turn

```mermaid
flowchart TB
    draft["Your draft"] -->|"Enter"| queue["Saved queue"]
    queue -->|"normal delivery"| next["Executor's next turn"]
    queue -->|"s · selected or marked"| waiting["Saved steering request"]
    waiting -->|"when a turn is running"| active["Active planner or executor turn"]
    next -->|"Codex accepts"| history["Conversation history"]
    active -->|"Codex accepts"| history
```

Steering moves instructions out of the normal queue, preserving queue order. They stay saved until a worker accepts them into an active turn. Without an active turn, they wait; executing a plan task or a normal queued instruction starts one.

Ordinary queued instructions wait for the executor while planning runs. For connection and recovery behavior, see [Workers](workers.md).
