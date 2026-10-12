[Docs](README.md) / Reference

# Local state

The harness saves work automatically in SQLite and private source checkpoints. Run from the same project root to restore its codemods, drafts, queue and history.

The terminal and [macOS app](macos.md) use the same database. Only one controller can open a project at a time; close the active interface before switching. The project lock is released after worker cleanup on exit.

## What survives

| Event | Source and history | Runtime and native conversations | Does work resume? |
| --- | --- | --- | --- |
| Pause | Retained | Retained | Only on explicit resume |
| Quit | Retained | Stopped VM disk retained | Unfinished work stays paused on reopen |
| Publish | Retained, with PR URL/commit | Retained | Ready for further edits |
| Replacement plan | Source/history retained; current tasks replaced | Worker runtimes/caches retained; retired task folders removed | New valid plan executes |
| Close | Checkpoint, task branches, draft and queue retained | VM and runtime/native logs removed | No |
| Reopen closed work | Restored from checkpoint | Fresh VM created when execution resumes | Only on explicit resume |
| Retention expires | Branch/history/exports retained | Already removed at close | Worktree restored if reopened |
| Delete | Local codemod data removed; GitHub PR unchanged | Removed | No |

Closed worktree retention defaults to 30 days, checked at project startup. `--closed-worktree-days 0` disables it. Only worktrees unchanged from their checkpoint are pruned; outside edits are preserved.

## Project identity

Git projects use the canonical repository root; other folders use their canonical path. Starting in a Git subfolder resolves to the root. Moving a repository changes its identity; relocating saved worktrees is unsupported.

Run one harness instance per project. Startup discovers provider availability again, but saved task owners/providers stay fixed. A missing assigned provider pauses its work.

## Storage layout

On macOS, the main database is:

```text
~/Library/Application Support/sprowt-harness/state.db
```

| Location beside the database | Contents |
| --- | --- |
| `projects/<project>/` | Project sync checkpoints and metadata-only tool activity |
| `workspaces/<mod>-<stamp>/` | Source, Git recovery, task drafts and check bundles |
| `network.json` | Global download allowlist |
| `router.env` | Optional host-only Jev key |
| `skills/<version>/` | Read-only coding instructions embedded in the installed harness |

<details>
<summary>Complete workspace directory reference</summary>

```text
~/Library/Application Support/sprowt-harness/
├── state.db          harness state
├── state.db-wal      SQLite write-ahead log, when present
├── state.db-shm      SQLite shared memory, when present
├── network.json      guest download allowlist
├── router.env        host-only Jev API key; owner-only permissions
├── skills/           versioned instruction bundles, shared across codemods
├── projects/         project sync state, independent of codemods
│   └── <project>/
│       ├── sync-project.json pending fast-forward; removed when complete
│       ├── sync-before-index original staging checkpoint during sync
│       ├── sync-index temporary staging used for the fast-forward
│       └── tools.jsonl project sync activity; metadata only
└── workspaces/       private working folders per mod
    └── <mod>-<stamp>/
        ├── checkout/ Git worktree; retained until deletion or closed retention
        ├── git-mod.json branch, base, checkpoint commit, PR URL and recovery phase
        ├── publication.json validated commit/PR draft tied to source and evidence
        ├── publication-commit.json interrupted publication commit receipt
        ├── project-setup pending initial Git setup or snapshot adoption
        ├── repository-setup.json confirmed GitHub destination and recovery
        ├── snapshot-ready completed Git mod source transfer
        ├── sync-project.json pending project fast-forward; removed when complete
        ├── sync-before-index original staging checkpoint during sync
        ├── sync-index temporary staging used for the fast-forward
        ├── refresh-target pending starting-source refresh; removed when complete
        ├── main-update.json pending target, checkpoint, conflicts and integration plan
        ├── main-update/ immutable merge snapshots and temporary merge index
        ├── before/   latest checked target files
        ├── work/     source exported from the VM
        ├── base.git/ private diff metadata
        ├── tasks.json task round, worker ownership and integration state
        ├── checks/   saved scripts and receipts, one <task-run-id>.json per task
        ├── environments/ latest preparation receipts and prior attempts
        ├── tasks.bundle sanitized guest Git branches for VM recovery
        ├── task-drafts/ sanitized task baselines and unfinished source
        ├── checkpoint-error present if a worker checkpoint needs recovery
        ├── vm.json   VM identity and image digest; removed after VM deletion
        ├── host-codex/ private worker directories and login links; never shared with the guest
        ├── muse/     per-worker native session, task, command and accepted turn IDs
        ├── sandbox.log setup diagnostics
        ├── packages.jsonl OS package requests and results
        ├── tools.jsonl harness tool activity; metadata only
        ├── setup/    trusted guest package setup files
        └── review.patch
```

</details>

Apple Container manages VM disks separately. Temporary transfer archives are created inside the private workspace.

Muse's owner-only cache is separate from source, login and saved work:

```text
~/Library/Caches/sprowt-harness/muse/
```

Each release has its own subdirectory containing the tested Mac and Linux executables, embedded adapters and a preparation lock. Downloads are verified before atomic replacement. Earlier release directories remain available to their matching harness versions. Removing this cache requires online preparation on the next Muse launch. Your standalone Muse installation and account settings stay in their original locations. See [automatic Muse setup](workers.md#automatic-muse-setup).

## What SQLite records

| Record | Saved context |
| --- | --- |
| Codemod | Description, draft, lifecycle and deliberate user pause |
| Plan | Contracts, tasks, scopes, provider choices and routing evidence |
| Task attempt | Owner, source identity, delivery, report, checks and recovery counters |
| Messages | Conversation, queued instructions, steering and mailbox receipts |
| Review | Stable findings, triage, acceptance criteria, source fingerprints, decisions and budgets |
| Network | Requests, decisions and per-codemod domain grants |
| Git workflow | Workspace association, publication/close intent and checkpoint metadata |

Worker updates carry task and attempt identities. Replaying history preserves that association; older unlinked narration stays in Activity. Current progress cannot use a prior attempt's completion message.

Routing inputs and conversation bodies are plain text. Model/effort and routing decisions remain inspectable; env files and the Jev key are excluded from routing inputs.

## Credentials and privacy

The database is owner-readable only and its directory is private to the macOS user. This is local access control, not encrypted storage of conversations.

Host credentials are not copied into source exports or guest files. Temporary Muse login/account-resolution files are removed after use; turn capabilities expire at turn end. Private Muse recovery files contain IDs rather than credentials or message bodies. Native tool history remains on the VM disk while it exists.

Tool activity records metadata without arguments, outputs or credentials. Check scripts/results are separate private files. Generated runtime databases and caches remain on the VM disk; committed fixtures remain source. See [source filtering](execution.md#source-and-generated-files).

## Recovery records

### Task and check recovery

Verification feedback, original owner, new attempt identity and the one-attempt recovery budget save atomically. Restart and explicit retry retain the budget. Passing verification clears feedback.

**Fix failed check** saves a marker and combined-failure evidence, clears obsolete passing task results and invalidates the verified version. Peers and check bundles stay. A failed requested attempt pauses; success requires new combined checks.

Integration handoffs save their own two-attempt budget and affected-task context. Review saves a separate two-pass budget and finding ledger. New plans reset applicable budgets while keeping history. See [recovery budgets](execution.md#recovery-budgets).

### Review and message recovery

Review findings close only with current regression evidence and an explicit reviewer decision. Omitted findings stay open. Legacy findings import once, preserving evidence and regression names, and require fresh triage.

Mailbox history survives new rounds, but old task addresses retire. Pending steering and native receipts reconcile before delivery retries. Domain grants survive new rounds and closing; old task requests retire. Deletion removes mailbox and network records with the codemod.

### Git recovery

| Record | Recovery purpose |
| --- | --- |
| `git-mod.json` | Branch, base, source fingerprints, commit, PR URL and pending operation |
| `publication.json` | Model-written text tied to the reviewed source, base and previous published head |
| `publication-commit.json` | Resume an interrupted publication without creating another commit |
| `refs/sprowt/checkpoints/` | Git references retaining unpublished checkpoint history |

Closing saves a timestamp. Confirmed initial setup uses a persistent workspace; cancelling an unconfirmed file preview removes that temporary preview.

Project sync uses a temporary index and locks the real index during fast-forward. Checkpoints can finish an interrupted index update. A repository lock serializes sync with codemod setup; hooks and project filters are disabled.

Target updates install integration plans atomically. Immutable snapshots preserve the baseline; a saved merge commit and temporary index allow finalization without losing ancestry. Success removes recovery snapshots. Project sync folders remain when a codemod is deleted.

Closing exports source and a sanitized guest task-branch bundle before removing the VM. Runtime installations are not in the bundle. Publication saves its URL/commit before success is displayed. Failed operations preserve checkpoints for retry.

**Next:** [Git workflow](git-workflow.md) · [Muse receipts](workers.md#muse-recovery) · [Troubleshooting](troubleshooting.md)
