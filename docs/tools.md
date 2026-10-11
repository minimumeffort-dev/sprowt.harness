[Docs](README.md) / Reference

# Harness tools

The Rust dispatcher validates caller identity and inputs, then routes operations to a host or VM adapter. Project, codemod, worker and VM identity come from saved context, not model arguments.

These are internal agent/controller interfaces, not shell subcommands or a public HTTP API.

## Worker-facing tools

| Tool | Outcome |
| --- | --- |
| `run_task_checks` | Save optional scripts and return observed check results for the current attempt |
| `install_system_packages` | Install named Debian packages in the caller's VM |
| `request_network_access` | Ask the user to approve exact download domains |
| `send_worker_message` | Save an ask, reply or update in the caller's codemod |
| `read_worker_messages` | Read mailbox and relevant peer context |
| `ack_worker_messages` | Acknowledge inbox messages |

Codex receives JSON schemas; Muse uses a guest MCP bridge to the same dispatcher. Unknown tools, worker requests for host operations, and planner/reviewer calls to these executor tools are rejected. External host MCP servers are not exposed.

## Run task checks example

This example assumes task run **42**, attempt **1**, an installed task virtual environment, and a declared check named **Run the Python suite**. A real worker must use the source identity, check names and executable from its current assignment.

```json
{
  "source": "00000004-0001-4000-8000-00000000002a",
  "checks": [
    {
      "check": "Run the Python suite",
      "command": [
        "/tasks/42/.venv/bin/python",
        "-m",
        "unittest",
        "discover",
        "-s",
        "tests"
      ],
      "timeout_seconds": 60
    }
  ],
  "scripts": null
}
```

**Expected:** observed results plus source/script fingerprints. A passing tool result does not complete or merge the task. The worker returns the same commands in its final report; Rust independently reruns them.

| Field | Contract |
| --- | --- |
| `source` | Current attempt identity; must belong to the worker and VM |
| `checks` | 1–32 entries covering every declared check; exact names |
| `command` | Argument array beginning with an absolute executable; runs in the task folder |
| `timeout_seconds` | 1–180, or null for 30 seconds |
| `scripts` | Null retains the bundle; an array replaces it with `{name, content}` entries |

Script bundles allow 16 unique plain filenames and 256 KiB total. They restore read-only under `/opt/sprowt-checks/<task-run-id>/`. A stale attempt, another worker's task or a different VM is rejected. See [check semantics](execution.md#write-repeatable-checks).

## Request access example

A worker can request a blocked documentation host:

```json
{
  "domains": ["docs.example.org"],
  "reason": "Read the dependency's official migration guide."
}
```

**Expected:** a pending request visible through **Ctrl+N**, not immediate access. Only the host UI can approve or deny it. Requests allow 1–8 exact public hostnames; URLs, ports, IPs, wildcards and control characters are rejected. See [network policy](sandbox.md#downloads).

A package request uses names and a reason:

```json
{
  "packages": ["libnss3"],
  "reason": "Install the missing Chromium shared library for browser checks."
}
```

**Expected:** installation or a specific setup error in the caller's VM. No shell command, repository modification or removal can be supplied. See [system packages](sandbox.md#system-packages).

## Operation registry

| Operation | Caller | Execution |
| --- | --- | --- |
| `sync_project` | Harness | Safely fast-forward the project branch, independent of codemods |
| `initialize_project` | Harness | Create the confirmed initial commit and worktree |
| `adopt_snapshot` | Harness | Attach saved starting files and verified work to Git |
| `connect_repository` | Harness | Connect GitHub or create a confirmed private repository |
| `create_worktree` | Harness | Check and safely update the project branch, then create its worktree |
| `refresh_worktree` | Harness | Refresh an untouched worktree before retrying planning |
| `check_target` | Harness | Fetch the PR target and hold an outdated open PR as draft |
| `update_target` | Harness | Save a checkpoint and prepare combined source for VM verification |
| `finish_update` | Harness | Save the verified merge with both Git parents |
| `publish_pr` | Harness | Git and GitHub CLI on the Mac |
| `prepare_edits` | Harness | Check the saved PR when present; prepare the next edit round |
| `close_mod` | Harness | Commit a local checkpoint and remove the VM |
| `reopen_mod` | Harness | Restore the saved worktree and reopen |
| `prune_mod` | Harness | Remove an unchanged closed worktree; retain its branch |
| `cleanup_mod` | Harness | VM deletion and Git worktree cleanup |
| `install_system_packages` | Executor | Package setup inside its own Linux VM |
| `run_task_checks` | Executor | Save check scripts and run declared checks through the controller |
| `request_network_access` | Executor | Save exact download domains and a reason for user approval |
| `send_worker_message` | Executor | Save an ask, reply or update in its codemod |
| `read_worker_messages` | Executor | Read its mailbox, live assignments and relevant peer scopes and topics |
| `ack_worker_messages` | Executor | Acknowledge its inbox messages |


Harness operations originate in workflow state and relevant user confirmations. Workers cannot invoke them through the dispatcher. Initial Git setup, repository destination and publication require confirmation. Publication requires current verified source and a clean review.

The controller owns task Git operations internally. Target updates wait for idle workers and verified source; automatic updates also wait for a clean review. Credential handling remains in host adapters.

## Progress and activity

| Operation | Limit |
| --- | --- |
| Git/GitHub command | Two minutes |
| Managed package command | Fifteen minutes |
| Task check | Its saved command budget |

Adapters return typed results/errors and report progress. Cancellation remains available.

Each workspace's host-only `tools.jsonl` records call ID, tool, codemod, worker when present, caller, status and duration. Arguments, outputs and credentials are omitted. Project sync has a separate project-level log with no codemod ID.

Check bundles and latest results live in private `checks/<task-run-id>.json` files, outside project Git. An unmatched log start can indicate interruption, but the activity log does not drive retries: saved state and Git checkpoints do. Deleting a codemod removes its logs; project sync records remain.

## Extend the dispatcher

Add a typed request, registry entry with caller policy, and adapter. Worker-facing tools also need validation and a JSON schema. Test caller authorization and workspace boundaries before exposing the operation.

The dispatcher is independent of provider transport. Codex native command/file tools continue through its guest connection; Muse reaches the dispatcher through its guest MCP tunnel. Runtime plugin loading and external MCP servers are not connected.

**Next:** [Source map](development.md#source-map) · [Storage layout](local-state.md#storage-layout)
