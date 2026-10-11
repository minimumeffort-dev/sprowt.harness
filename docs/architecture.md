[Docs](README.md) / Understand the system

# Architecture

Rust owns the workflow. Agents propose plans, make scoped changes and report evidence. The controller validates that evidence and decides which transition is allowed.

## Where things run

```mermaid
flowchart TB
    subgraph host["Your Mac"]
        ui["Terminal + Rust controller"]
        state[("Saved state + source checkpoints")]
        planner["Read-only planner"]
        writer["Read-only publication writer"]
        git["Git + GitHub publication"]
        ui <--> state
        ui --> planner
        ui --> writer
        writer -->|"Text for your confirmation"| git
        ui --> git
    end
    subgraph guest["One Linux VM per executing codemod"]
        first["Executor 1 · task worktree + home"]
        second["Executor 2 · task worktree + home"]
        combined["Combined source + check runner"]
        reviewer["Fresh read-only reviewer"]
        first --> combined
        second --> combined
        combined --> reviewer
    end
    ui --> first
    ui --> second
    ui --> reviewer
    combined -->|"Source export"| state
```

Source is copied into the VM. Host project folders and Git credentials are not mounted there. The production Codex connection keeps its app server on the host and sends execution into Linux; Muse runs its CLI in Linux behind a host credential broker.

## Responsibilities

| Component | Owns | Read more |
| --- | --- | --- |
| Terminal | Timeline, selection, drafts and contextual actions | [Terminal](terminal.md) |
| Planner | Contracts, tasks, scopes, dependencies and checks | [Planning](planning.md) |
| Scheduler | Ready tasks, two active slots and saved provider assignments | [Execution](execution.md) |
| Executors | Implementation within a task folder; repeatable check commands | [Workers](workers.md) |
| VM controller | Environment recipes, capability probes, baselines, checkpoints and independent checks | [Environments](environments.md), [Sandbox](sandbox.md) |
| Reviewer | Fresh assessment, finding triage and explicit closure decisions | [Review](review.md) |
| Publication writer | Commit title, PR title and summary from the reviewed diff | [Publication](git-workflow.md#commit-and-pr-text) |
| Tool dispatcher | Caller validation and host/guest operation boundaries | [Tools](tools.md) |
| Store and Git adapters | Durable state, recovery checkpoints and confirmed publication | [Local state](local-state.md), [Git workflow](git-workflow.md) |

The controller serializes checkpoints, integration, checks and system package installation. Independent model turns and ordinary task commands can run concurrently. Review repairs run one owner at a time.

## The workflow

```mermaid
stateDiagram-v2
    state "Needs attention" as NeedsAttention
    state "Ready to publish" as Ready
    [*] --> Planning: Submit request
    Planning --> Preparing: Valid plan
    Preparing --> Building: Capabilities ready and baseline recorded
    Preparing --> NeedsAttention: Setup or capability failure
    Building --> Checking: Tasks integrated
    Checking --> Reviewing: Combined checks pass
    Checking --> NeedsAttention: Check fails
    Reviewing --> Ready: Clean current review
    Reviewing --> NeedsAttention: Findings or review failure
    NeedsAttention --> Building: Choose repair
    NeedsAttention --> Preparing: Revise setup
    NeedsAttention --> Checking: Retry checks
    NeedsAttention --> Reviewing: Investigate or retry review
    Ready --> Published: Confirm publication
    Published --> Planning: Request edits
    Ready --> Planning: Request edits
```

Bounded automatic recovery happens inside this flow. User pauses, missing permission, uncertain delivery and exhausted repair budgets stop advancement. Reopening never restarts unfinished work automatically.

## Evidence follows the source

Task completion, combined verification, review and publication are separate facts:

1. A worker returns commands for every declared check.
2. Rust runs those commands and accepts the task only if its source remains unchanged.
3. All tasks' checks run against the integrated version.
4. A fresh reviewer examines that version and the saved finding ledger.
5. Publication requires passing checks and a clean review for the current source.

Changing source invalidates previous evidence. Saved results stay readable as history. A PR can remain open while newer local changes still need verification.

## Boundaries today

- Linux execution only; native macOS and iOS builds need another backend.
- Planning is read-only on the host. Independent review is read-only in the VM, with network access disabled.
- Downloads pass through an allowlist. Workers can request access; only you can approve it.
- External MCP servers, runtime plugins, shared memory and repository indexing are not connected.
- GitHub publication is supported; PR merging remains outside the harness.

**Next:** [How tasks are planned](planning.md) · [How checks establish completion](execution.md)
