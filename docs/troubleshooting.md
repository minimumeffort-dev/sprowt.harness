[Docs](README.md) / Use the harness

# Troubleshooting

Start with the state in the dock. **Ctrl+T** opens its evidence; **Ctrl+G** lists available actions. Saved worker narration can describe an earlier attempt, so compare it with the current check or review result.

## Find the symptom

| Symptom | Start here |
| --- | --- |
| Startup asks for a different Muse or Codex version | [Worker startup](#worker-startup) |
| Muse disconnected, returned no report or delivery is unknown | [Muse stopped](#muse-stopped) |
| A check fails, times out or never finishes | [Checks](#checks-fail-or-take-too-long) |
| Review keeps finding issues | [Review](#review-is-not-converging) |
| GitHub error while local work continues | [GitHub and sync](#github-and-sync-errors) |
| Task changes conflict | [Conflicts](#task-changes-conflict) |
| Download domain is blocked | [Network access](#a-download-is-blocked) |
| Reopened work is idle | [Resume](#reopened-work-does-not-start) |

## Worker startup

Check the [supported versions and login requirements](workers.md#supported-versions). Codex is required; Muse joins automatically if installed. An installed Muse with the wrong version or missing account login stops startup.

1. Install the supported provider CLI version.
2. Sign in using the [login instructions](workers.md#login).
3. If the adapter pin changed, reinstall the harness from the current repository:

   ```sh
   cargo install --path . --locked --force
   ```

4. Restart the harness from the same project.

Reinstalling does not resume paused tasks or reset repair budgets. If Apple Container is unavailable, start its service using the [quickstart](quickstart.md#1-prepare-the-workers).

## Muse stopped

| Message or condition | Meaning | Next step |
| --- | --- | --- |
| Bridge or transport disconnected | Connection ended; source is retained | Let the one automatic reconnect finish, then inspect the task if it pauses |
| Delivery could not be confirmed | The instruction may have reached Muse | Retry through the harness so saved receipts can be checked; do not send the instruction again as a new message |
| No task report | The turn ended without a valid completion report | Inspect task Activity and retry the task; narration alone cannot complete it |
| Response allowance exhausted | Muse did not finish within its response budget | Inspect the saved work and retry; a previous report cannot verify the current attempt |
| Permanent request rejection | Provider rejected the request | Read the redacted cause; correct it before retrying |
| Authentication failure | Login needs attention | Sign in again on the host and restart the connection |

Select the failed task in **Details → Tasks**, then press **r**. Independent peers can continue. Unknown delivery stays paused until reconciled; the harness does not blindly resend it. See [Muse recovery](workers.md#muse-recovery).

## Checks fail or take too long

Open **Ctrl+T → 2 · Checks**, then press **e** for the saved command, output, elapsed time and limit.

| Evidence | Appropriate action |
| --- | --- |
| Assertion failure in the current combined source | **Fix failed check** returns it to the recorded owner |
| Temporary external failure; command remains valid | **Retry final checks** reruns without source edits |
| Executable missing | Allow the bounded environment recovery; inspect setup if it fails again |
| Inner test timeout before the command limit | Repair the test lifecycle or its measured budget, preserving assertions |
| Commands marked **not run** | Resolve the earlier failure; they were skipped, not passed |
| Automatic repair limit reached | **Revise repair plan** with the saved evidence |

A worker saying “all tests pass” describes its reported result. The controller's later combined check can still fail on newer integrated source.

Checks start in fresh processes. Earlier shell exports and background servers are not retained check setup. A browser check must start and clean up its own server, wait for the correct response and use a writable fixture directory. `/static` is a URL path, not a temporary filesystem location.

The command budget defaults to 30 seconds and can be set to 1–180 seconds. Raising the outer limit cannot fix a shorter inner deadline or a deadlock. Inspect the blocked operation first. See [repeatable checks](execution.md#write-repeatable-checks).

## Review is not converging

Open **Details → Review** and compare the stable finding IDs. A new issue can be a regression from a repair, an earlier missed defect or uncertain evidence; the reviewer must distinguish these with evidence.

**Fix review issues** authorizes at most two passes. No progress, uncertain evidence or an exhausted budget stops automatic work. Choose **Investigate findings** or **Revise repair plan** as offered. Completed source and the finding history stay saved.

An old review cannot be used as current proof. If it says **Awaiting fresh review**, checks may have passed while independent resolution is still pending. See [review closure](review.md#what-counts-as-resolved).

## GitHub and sync errors

A background sync failure is a separate warning. Local tasks and review can continue. Press **Ctrl+G → s** for the full error and **Ctrl+U** to retry that check after connectivity or authentication is restored.

For authentication setup, use `gh auth login` and `gh auth setup-git`. For a failed foreground publication, follow its saved retry action. The harness retains Git checkpoints and records publication progress before showing success.

If history diverged, local edits overlap, or Git is already mid-operation, resolve that project condition first. The harness does not force-push, rebase or discard edits to make sync succeed. See [Git workflow](git-workflow.md#keep-the-project-current).

## Task changes conflict

Both versions and non-conflicting changes are saved. A text conflict within the owner's scope gets one automatic resolution attempt per task and plan. Repeated, binary, rename, deletion or out-of-scope conflicts pause.

Open the task's Details for paths, owners and attempt count. Retry a paused task when the conflict can be resolved within its scope, or send edits to revise the plan. Do not delete its workspace to clear the error. See [conflict recovery](execution.md#task-conflict-recovery).

## A download is blocked

Press **Ctrl+N** to see exact domains and the reason. **a** approves for this codemod; **d** denies; **Esc** leaves the request pending. Approval reconnects the affected worker and queues its retry. Other workers continue.

The request does not change the global allowlist. See [downloads](sandbox.md#downloads) for configuration and limits.

## Reopened work does not start

This is expected for unfinished work. Reopening restores source, history, draft and queue; **Ctrl+R** explicitly resumes eligible work. A prior user pause and consumed recovery budgets remain saved.

If a codemod was closed, its next execution creates a fresh VM and may need to restore dependencies. Quitting instead retains the stopped VM disk. See [retention](local-state.md#what-survives).

## Evidence to keep when reporting a problem

Include the visible state, task or finding ID, the failed check command, its output and whether the source changed since that result. **Details → Activity** and [metadata-only tool logs](tools.md#progress-and-activity) help establish the sequence. Redact secrets and private project content before sharing.

## A reproduction is blocking its repair

Choose **Revise repair plan** when a tests-only prerequisite fails and its production repair is waiting. The planner retains the reproduction and combines it with the fix under one owner. Passing a reproduction is different from proving that the product is repaired; combined checks and fresh review remain required. See [diagnostic recovery](execution.md#a-diagnostic-task-blocks-its-repair).
