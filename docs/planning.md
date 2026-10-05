# Planning and Laya

Creating a codemod starts planning from its description. Git setup is confirmed first when needed. The [project branch is checked and safely updated](git-workflow.md#keep-the-starting-source-current) before creating its worktree; Laya and the planner inspect that committed source.

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
| Executor | Implement and verify plan tasks |

They have separate Codex conversations. The planner is read-only. A valid plan starts the executor automatically in the mod’s Linux VM; Rust dispatches tasks in dependency order. See [Plan execution](execution.md).

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

Once a valid plan is saved, execution begins automatically. **Ctrl+R** stops work or retries; **Ctrl+D** reviews source when workers are idle.

## Stop and retry

Ctrl+R stops unfinished planning or retries it after interruption or failure. An untouched worktree checks the remote again, refreshes its source and starts a fresh planner conversation. Confirmed starting snapshots and source already used for execution stay fixed. History, draft and queue are retained.

A valid plan is saved and displayed in the conversation. New plans proceed directly to execution. After an interruption, Ctrl+R explicitly resumes work.

Reopening restores progress; unfinished work waits for retry. Finished versions can update automatically from merged work, using an integration plan. Sending edits creates a new plan against the latest source, with checks for the change and regressions. The conversation stays; the current plan is replaced.
