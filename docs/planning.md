# Planning and Laya

Creating a codemod starts planning from its description. Git setup is confirmed first when needed. New codemods create a worktree from committed `HEAD`; Laya and the planner inspect that source. You can describe the whole change immediately.

```mermaid
flowchart TB
    goal["Codemod description"] --> route["Local Laya · estimate complexity"]
    route --> config["Rust · choose model and reasoning"]
    config --> planner["Codex planner · inspect source and docs"]
    planner --> validate["Rust · validate the structured plan"]
    validate --> saved["SQLite · saved plan and configuration"]
    saved --> executor["Executor context · goal plus plan"]
```

## Two roles

| Role | Job today |
| --- | --- |
| Planner | Inspect the project and produce a task plan |
| Executor | Implement plan tasks and answer queued instructions |

They have separate Codex conversations. The planner is read-only. **Ctrl+R** starts the executor in the mod’s Linux VM; Rust dispatches tasks in dependency order. See [Plan execution](execution.md).

## Local model routing

Install [uv](https://docs.astral.sh/uv/getting-started/installation/), then run `sprowt-harness setup`. Setup installs Python 3.13, `laya==0.3.20` and the multilingual checkpoint under the [local data directory](local-state.md).

Laya runs locally in a separate Python process. It receives the description, a top-level file listing and short excerpts from README.md and AGENTS.md. Codex then inspects the project itself to plan the work.

| Laya result | Planner model | Reasoning |
| --- | --- | --- |
| Simple | `gpt-6.1-sol` | medium |
| Complex | `gpt-6.1-sol` | high |
| Demanding | `gpt-6-astra` | high |
| Uncertain, unavailable or timed out | `gpt-6.1-sol` | high |

The score threshold is 0.70; routing times out after 30 seconds. This is an experimental heuristic. The stock checkpoint was uncertain or wrong in our small test. Laya recommends configuration; Codex generates the plan.

Rust checks the selected model and reasoning level against Codex’s catalog. Missing models fall back to Sol or the catalog default. Account access is checked only when inference runs. The expanded plan details show the chosen configuration and routing reason.

## What a plan contains

A summary and tasks with IDs, titles, outcomes, file scopes, dependencies, worker assignments and completion checks.

Rust checks that:

- There are 1–32 tasks with unique IDs and nonempty outcomes and checks.
- Dependencies exist and contain no cycles.
- File scopes use exact project-relative paths, with no globs or parent traversal.
- Tasks sharing file or directory scopes have a dependency between them.
- Every task uses the connected provider: Codex.

These checks validate structure and declared scopes. They do not prove the plan will solve the request.

## Review the plan

The conversation shows a numbered outline: task titles, outcomes and dependencies such as **after task 1**. The numbering matches the displayed order, even when the saved task IDs are words.

Press **Ctrl+O** for file scopes, individual completion checks and model details. Press it again to collapse them. The full plan remains saved; changing its display does not change the plan or start a worker.

**Plan ready** means planning finished. The Linux VM boundary appears below the outline. **Ctrl+R** executes the saved plan; **Ctrl+D** reviews the resulting diff.

## Stop and retry

Ctrl+R stops unfinished planning or retries it after interruption or failure. A valid plan is saved and displayed in the conversation. Once it is ready, Ctrl+R executes the plan without needing another message.

Reopening restores the saved plan without starting workers. Mods created before planning was added retain their original queue workflow.
