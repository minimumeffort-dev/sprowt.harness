# Plan execution

A plan becomes code when you press **Ctrl+R**. One Codex executor runs tasks in dependency order in the mod’s Linux VM.

```mermaid
flowchart TB
    plan["Saved plan"] --> start["Ctrl+R · snapshot the project"]
    start --> vm["Create or reconnect the mod’s Linux VM"]
    vm --> choose["Rust · choose a task whose dependencies are done"]
    choose --> worker["Codex · prepare runtime, edit and test in Linux"]
    worker --> checks["Rust · rerun checks in the same VM"]
    checks -->|"pass · tasks remain"| choose
    checks -->|"failure or interruption"| paused["Keep files and progress · explicit retry"]
    checks -->|"all tasks done"| final["Rerun all task checks together"]
    final -->|"pass"| review["Ctrl+D · review the diff"]
    review --> confirm["a, then Enter · confirm apply"]
    confirm --> project["Apply reviewed files to your project"]
```

## Working folder

The first run snapshots your current files, including uncommitted and untracked code. In Git projects, ignored files are left out. Every mod gets its own folder outside the project, with separate copies of files and private Git metadata for the diff. New source files stay visible in review even if a generated `.gitignore` rule would hide them.

Common credential files, including `.env` and `.npmrc`, are excluded. `.env.example` and `.env.sample` are included. Installed dependencies and build caches are excluded too. Other secrets in source files are still source files; keep them out of the project.

The source snapshot is copied into `/workspace` inside a persistent VM. The host’s `work/` folder holds exported source for diff review; it is not mounted into the guest. Runtime installs and dependencies remain in the VM. See [Local Linux sandbox](sandbox.md) for setup, download policy and lifecycle.

The executor cannot access the original project, harness state, host Codex credentials or Keychain. The planner remains read-only. See [Workers](workers.md) for the boundary.

## Tasks and checks

Rust dispatches one task at a time. Dependencies must be complete first. Queued follow-ups run between tasks; steering reaches the current turn.

Codex returns a short task summary and runnable commands for each declared completion check. Rust reruns them in the same sandbox, with a 30-second limit per command. Missing checks, nonzero exits, timeouts or checks that change source files block completion. After all tasks finish, every saved check runs again against the combined result.

The plan shows task status. **Ctrl+O** expands scopes, check commands, failures and final check results. Passing commands is evidence, not a guarantee that the plan or tests are good; review the code too. Declared file scopes guide Codex, while the sandbox enforces the folder boundary.

## Pause and recover

**Ctrl+R** pauses execution. During verification, the current guest command is terminated and subsequent checks stop. Interrupted tasks keep their working files and runtime. Press Ctrl+R again to explicitly retry the unfinished task; completed tasks stay done.

Reopening restores progress without starting workers. On reconnect, confirmed completed turns are verified without asking Codex to repeat the edits. Unconfirmed delivery pauses for explicit retry. Failed final checks can be rerun without regenerating completed tasks.

## Review and apply

1. **Ctrl+D** opens the diff when execution is idle. You can inspect partial work, but apply is available only after all tasks and final checks pass.
2. Use **↑ / ↓** or **Fn + ↑ / ↓** to scroll. **Esc** returns to the conversation.
3. Press **a**, then **Enter** to confirm applying. Esc cancels.

The working files must match the snapshot that passed verification and the diff you reviewed. Every affected project file must still match its starting contents and permissions. A conflict stops the apply before any file is changed; reconcile it manually or start a fresh mod.

Applying adds, edits and deletes files without committing in your target project. Files are replaced individually; a handled error rolls back earlier replacements. A crash midway can leave a partial apply; the starting copies remain in the mod folder's `before/` directory.

After applying, queued follow-up questions use read-only tools against the project. Start a new mod for further code changes. Deleting a mod removes its VM and working folder, including unapplied work.

## Current limits

One executor per mod. Linux only; no host mounts or published app ports. Symlinks, submodules and special files are unsupported. Projects without Git work too; Git is required locally for snapshots and diffs.

Uses Codex CLI **0.159.2** through the [app-server API](https://developers.openai.com/codex/app-server).
