# Plan execution

Creating a codemod starts planning and execution automatically. Up to two Codex or Muse executors follow dependencies in the mod’s Linux VM.

```mermaid
flowchart TB
    plan["Valid saved plan"] --> vm["Create or reconnect the codemod VM"]
    vm --> task["Scheduler · choose independent ready tasks"]
    task --> route["Jev + Rust · choose model and effort for each task"]
    route --> first["Worker 1 · own task worktree + runtime"]
    route --> second["Worker 2 · own task worktree + runtime"]
    first --> checks["Controller · rerun task checks"]
    second --> checks
    checks --> merge["Combine verified changes · one at a time"]
    merge -->|"Tasks remain"| task
    merge -->|"All tasks done"| final["Rerun every check against the combined source"]
    final --> cleanup["Remove completed task folders"]
    cleanup --> ready["Version ready · send edits or publish"]
```

A failed check pauses that task and its dependents unless the worker requests a code repair from a completed owner. The other worker can finish independent work. **Ctrl+R** retries unfinished work; completed tasks stay done.

Executors receive the planner's peer links and coordinate through [saved task mailboxes](coordination.md). Links describe what to coordinate; dependencies describe what must finish first. A task with unanswered asks waits; Rust resumes it after answers arrive. Queued edit rounds wait too, so they cannot replace tasks with outstanding questions. Messages do not change file ownership or dependencies.

Provider assignments come from the planner's automatically detected worker pool. Codex remains the planner and integration worker. Both providers use the same checks, task mailboxes and source checkpoints. See [Workers](workers.md).

## Working folder

New codemods start from committed `HEAD` in their own host worktree. The first execution copies source into `/workspace` in Linux. A separate, local Git repository in the VM owns the integration branch and task worktrees. The project’s original Git metadata, remote configuration and credentials stay on the Mac.

The host’s `work/` folder holds source exports for planning, review and checkpoint saves. It is not mounted into the VM. Credentials, dependencies and caches are excluded from snapshots; `.env.example` and `.env.sample` are included. Source files remain visible even if a generated `.gitignore` would hide them.

Each worker’s runtimes and download caches stay in the VM across edit rounds and publication. Task-local dependencies are removed with their completed task folders. Closing removes that VM; reopening creates a fresh one when execution starts. See [Local Linux sandbox](sandbox.md) and [Workers](workers.md).

### Source and generated files

New test caches (`.pytest_cache`, `.mypy_cache`, `.ruff_cache`), `.egg-info` metadata and local database files stay out of checkpoints and PR diffs. This includes DuckDB, SQLite and `.db` files with their WAL, shared-memory and journal sidecars. Tracked database fixtures remain source. Already committed `.egg-info` files keep their baseline contents: dependency installation can regenerate them in the VM without changing source, failing checks or creating worker merge conflicts.

Private save copies preserve the checked files instead of filtering them against an older baseline. Saving checks their contents and permissions; changed source or outside worktree edits pause the save.

Guest Git checkpoints contain the filtered source snapshot. Runtime files remain on the VM's disk. Reconnecting repairs older checkpoint indexes and saved drafts before retrying, including conflicts confined to regenerated package metadata. Runtime files are retained; committed source conflicts still need resolution. Newly created database files are treated as runtime data.

```mermaid
flowchart TB
    files["Worker files"] --> source["Code + committed fixtures"]
    files --> runtime["Generated caches + runtime databases"]
    source --> checkpoint["Source checkpoint · checks · PR diff"]
    runtime --> disk["Stay on VM disk"]
```

## Tasks and checks

Rust dispatches at most two ready tasks into `/tasks/<task-run-id>`. Each worktree has its own branch and index, sharing Git objects. New tasks start from the latest combined source. A retry keeps the same worker and task folder. The worker can write that folder, its own runtime home and temporary files; other task folders, combined source and Git metadata remain read-only.

Slots follow the saved task providers: two Codex, two Muse or one of each. Rust reserves task ownership before connecting, parks idle workers and reuses them when their work is ready. Connecting and verification count toward the same two-slot limit. Stops stay paused; reopening waits for **Ctrl+R**. Network approval queues the original worker's retry until capacity is available.

One controller owns VM startup and shutdown, task checkpoints, Git integration and system package setup. Controller operations run one at a time; model turns and task commands overlap. Worker conversations and cancellation signals are separate.

Workers prepare project runtimes and dependencies; missing OS packages go through the [VM setup tool](sandbox.md#system-packages).

Each task returns a short summary and runnable commands for every declared check. Rust reruns them with a 30-second limit per command. Missing checks, nonzero exits, timeouts or checks that change source files block completion. Passing tasks are merged into the integration branch one at a time, as they finish. A conflict pauses execution and keeps both versions. The saved draft includes non-conflicting changes and conflict markers. **Ctrl+R** asks the worker to resolve them, or send an edit request to replan.

After all tasks finish, the harness updates each task folder to the combined source and reruns its checks there. Commands keep their original paths and installed dependencies. Successful final verification removes the task folders. Failed checks retain them for retry.

Each active task has a spinner and worker ID through implementation and checks. **Ctrl+O** expands scopes, commands and failures. File scopes guide workers; the sandbox enforces the folder boundary. Passing checks are evidence; review the code too.

Finished versions show compact task rows, the final check count and the publish action. **Ctrl+T** opens saved worker narration and handoffs separately from plan details. Questions for you stay visible in the conversation.

## Automatic repairs

When integration finds a reproducible regression in a completed task, the worker returns a repair request: owner task, exact files, declared failing check, runnable command and observed failure. The failing check belongs to the reporting task; the owner names whose code needs fixing. Both providers' report schemas constrain check names to the reporting task. Rust validates ownership and waits for active turns and checks to finish. Invalid requests identify the field that needs correcting; **Ctrl+R** retries with saved work intact.

```mermaid
flowchart TB
    failure["Integration finds a regression"] --> request["Save failure evidence + responsible task"]
    request --> owner["Owner repairs the latest combined code"]
    owner --> affected["Rerun the verifier and affected dependent tasks"]
    affected --> final["Rerun every final check"]
    final --> ready["Checks pass · changes ready"]
```

The original worker, task identity and file scope stay. Independent completed tasks remain done. The owner's worktree refreshes from combined source; unfinished verifier edits merge with the repaired source. Text conflicts remain visible for resolution. Runtime files stay in the VM. Delivery markers prevent a restart from resetting an in-progress repair.

Requests, failure evidence and the two-attempt budget save in SQLite. Reopened tasks lose their old passing checks, and publication waits for fresh final verification. **Ctrl+R** stops work; explicit retry preserves the repair budget. Each new plan starts a fresh budget. If a failure persists after two handoffs, send an edit request to revise the plan. **Ctrl+O** shows the repair check and failure evidence; the conversation keeps a short notice.

Missing access, environment blockers, uncertainty and product decisions still pause. Repairs do not expand file ownership or approve network access. A worker must identify a completed owner and provide evidence; ordinary blocked reports and failed controller checks keep their retry flow.

## Updates from merged work

When the PR target changes, a finished codemod saves a checkpoint and runs an integration plan in the same VM. Codex resolves text conflicts; Rust independently reruns the original and combined checks. Current workers finish first. Publication waits for verification. See [Worktrees and PRs](git-workflow.md#when-another-codemod-merges).

## Request edits

Send a message through the composer. During implementation, ordinary messages wait in the queue until the current version passes verification. The next message starts a new plan against the latest source, including checks for the change and regressions. The execution workspace and conversation stay; the current plan and task results are replaced.

After a failed run, sending a message starts a revised plan against the saved source, including unfinished work.

Use the queue’s **s** action to steer an active turn immediately. Steering and ordinary edit rounds are separate. See [Codemods and messages](codemods.md).

## Pause, review and publish

**Ctrl+R** pauses both workers and any verification or setup command. Files and runtime remain. Reconnecting verifies confirmed turns without repeating their edits; uncertain delivery waits for explicit retry. Failed final checks can rerun without regenerating completed tasks.

**Ctrl+D** reviews source while workers are idle. **Ctrl+S**, or **p** inside the diff, starts publication after verification. Confirm Git adoption or a GitHub destination if needed, then the PR. Publication commits and pushes to the codemod branch and retains the VM for more edits.

Closing stops both workers, saves each unfinished draft, then saves a local checkpoint and a Git bundle of the VM’s task branches before removing the VM. Reopening restores unfinished task source and branch relationships. Draft exports include edits from both tasks; overlapping text edits get conflict markers. Binary conflicts retain the VM until resolved. Installed runtimes and dependencies must be prepared again in the fresh VM; saved check commands may need an edit request to rebuild their environment. Deleting discards local work. See [Worktrees and PRs](git-workflow.md) for retention and recovery.

## Current limits

Maximum two executors per codemod, with task-addressed messaging. [Muse recovery](workers.md#muse-recovery) requires its retained VM disk; closing removes native history. Login refresh is not implemented. Agent availability is checked at startup; sign in or install a CLI, then restart. Linux only; no host mounts or published app ports. Symlinks, submodules and special files are unsupported. Uses Codex CLI **0.159.2** through the [app-server API](https://developers.openai.com/codex/app-server).

## Verify scheduling and repairs

These checks use disposable VMs and remove them afterward. The provider test uses your Codex and Muse subscriptions.

```sh
cargo test repair_handoff_preserves_drafts_and_rechecks_in_vm -- --ignored
cargo test codex_verifier_hands_a_regression_back_to_muse -- --ignored
```

Scheduling tests cover provider combinations, dependencies and saved owners. The VM check runs two Codex tasks followed by two Muse tasks, checks real overlap and removes the VM afterward.

```sh
cargo test scheduler
cargo test worker_slots_follow_provider_waves_in_vm -- --ignored
```
