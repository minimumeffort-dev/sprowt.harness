# Planning and routing

Creating a codemod starts a read-only Codex planner at **Astra xhigh**. Git setup and [starting-source synchronization](git-workflow.md#keep-the-starting-source-current) finish first. Edits plan against the latest source export.

```mermaid
flowchart TB
    goal["Request + project brief"] --> planner["Astra xhigh · inspect source and define contracts"]
    planner --> validate["Rust · validate tasks, scopes and dependencies"]
    validate --> schedule["Rust · choose the least loaded suitable provider"]
    schedule --> route["Jev + Rust policy · Codex model and effort"]
    route --> execute["Codex executors · task worktrees inside the VM"]
    schedule --> muse["Muse Spark 1.3 high · own VM task worktree"]
    muse --> checks
    execute --> checks["Rust · combine changes and rerun checks"]
```

The optional [independent reviewer](review.md) uses Sol 6.1 xhigh. It keeps a fresh conversation separate from the Astra planner and executor routing.

## Planning

The planner inspects relevant source, manifests, tests, docs and project rules. Its starting brief includes up to 120 file paths across three levels, short root rules and manifest excerpts, and up to three documentation excerpts. Dependencies, hidden files and external symlinks are excluded from that brief. The brief guides inspection; the planner still reads the source.

Describe the outcome in ordinary language. The planner chooses task splits, useful concurrency and coordination. Small changes can stay in one task.

Shared contracts define interfaces, data shapes and error behavior before work is split. Separate file ownership allows concurrent work against those contracts. An interface consumer can check its component with a stub; a dependent task then checks the real integration. Shared files and genuine implementation prerequisites require dependencies.

Each task can link peers around a concrete interface, assumption or handoff. Rust exposes links in both directions with peer ownership and live assignments. These links do not block scheduling. Unrelated tasks need no link or messages. Material assumptions and non-goals stay explicit.

```mermaid
flowchart TB
    request["Ordinary feature request"] --> contract["Planner · define shared behavior"]
    contract --> api["API task · independent checks"]
    contract --> ui["Interface task · independent checks"]
    api <-->|"Relevant updates or questions"| ui
    api --> together["Integration task · verify both together"]
    ui --> together
```

## Provider assignment

Startup checks installed CLIs and local account logins. Astra defines tasks, contracts and dependencies; Rust assigns each task when it becomes ready.

- Tasks use `worker: auto` by default. Backend, interface, tests, demanding implementation and integration can use either provider.
- A specific provider needs a brief `provider_reason` naming a required capability. Complexity alone does not restrict the provider. Rust validates the structure; it does not prove the claimed capability is real.
- Rust counts active executors across codemods in this harness and chooses the least loaded available provider. Ties alternate using saved assignment order. These counts cover work, not subscription quotas.
- Each codemod keeps two active slots. Connecting, running and checking occupy a slot. Dependencies still gate readiness.
- Assignment saves the provider, worker identity and reason before delivery. Retries, steering, recovery and repairs keep that owner. A missing provider pauses its saved work.

Existing plans keep their fixed provider assignments. New plans use automatic assignment. Muse runs Spark 1.3 high; Jev continues to choose Codex effort. **Ctrl+O** shows the assignment reason beside each task's model details.

## Jev task routing

[Jev](https://docs.typesafe.ai/introduction) is a hosted decision model. It evaluates four independent questions: difficulty, uncertainty, cross-component impact and security or data-integrity risk. Rust maps those answers to profiles:

| Task | Model | Effort |
| --- | --- | --- |
| Routine, clear, isolated | `gpt-6.1-sol` | medium |
| Involved or cross-component | `gpt-6.1-sol` | high |
| Hard, uncertain or high risk | `gpt-6.1-sol` | xhigh |
| Missing configuration, timeout or invalid response | `gpt-6.1-sol` | xhigh |

Each Codex task routes before its turn starts, using the project brief, saved plan, contracts, assigned scope and checks. Steering stays in the current turn. Planning is pinned to `gpt-6-astra` xhigh, so it needs no routing call.

Requests time out after eight seconds. The initial confidence floor is **0.80 for every question**. Rust validates options, probabilities and confidence before lowering effort. These thresholds are experimental; confidence measures distribution concentration, not guaranteed correctness. Test them on real codemods before changing them. See [Jev confidence](https://docs.typesafe.ai/confidence).

Codex's model catalog validates the chosen model and effort. An unavailable model falls back to Sol 6.1 or the catalog default, preferring xhigh, then high, then medium. Actual account access is established by inference. A rejected turn remains retryable.

Each task stores its resolved model, effort and routing reason beside its checks. **Ctrl+O** names the decision that triggered a fallback, for example **risk confidence 0.47 < 0.80**. Older saved decisions use their recorded evidence for the same explanation when available; the policy stays unchanged.

Jev calls also record the policy version, routing input and request duration, plus the revision and answers when available. The full evidence stays in SQLite. Retries replace the current decision; this is diagnostic data, not a calibrated benchmark or automatic architecture repair.

## Configure Jev

Copy `.env.example` to `.env.local` in the harness repository and add `TYPESAFE_API_KEY`. Then run `sprowt-harness setup` from that folder. Setup copies only that key into `router.env` in the [private local data directory](local-state.md), with owner-only file permissions. Installed harnesses use this configuration from any project.

`.env.local` is ignored by Git. The key is never loaded into the process environment, model prompts or worker configuration. Codex commands cannot read the local key file or `router.env`; executor tools stay inside the VM. Host Rust calls only TypeSafe's fixed HTTPS endpoint, with redirects and environment proxies disabled. Routing sends relevant project context to TypeSafe and uses separate API billing.

## What a plan contains

A summary, shared contracts, material assumptions, non-goals and tasks with IDs, titles, outcomes, file scopes, dependencies, coordination topics, automatic or capability-specific provider choices and completion checks. Existing saved plans without context or coordination fields still load.

Rust checks that:

- Context notes are bounded and nonempty when supplied.
- There are 1–32 tasks with unique IDs and nonempty outcomes and checks.
- Dependencies exist and contain no cycles.
- Coordination links name existing peers, exclude the task itself and duplicate peers, and carry brief topics.
- File scopes use exact project-relative paths, with no globs or parent traversal.
- Tasks sharing file or directory scopes have a dependency between them.
- Automatic tasks have an empty provider reason; a newly specified provider has a short capability reason. Legacy fixed assignments still load.
- The planner's schema allows automatic assignment and only the detected providers.

These checks validate structure and declared scopes. They do not prove the plan will solve the request.

## Review the plan

While work runs, the conversation shows a numbered outline: task titles, outcomes and dependencies such as **after task 1**. Finished versions show compact task rows and the final check count. The numbering matches the displayed order, even when the saved task IDs are words.

Press **Ctrl+O** for outcomes, dependencies, contracts, assumptions, file scopes, peer topics, completion checks and model details. Press it again to collapse them. **Ctrl+T** opens worker history separately. Display controls preserve the plan, draft and queue.

Once a valid plan is saved, execution begins automatically. **Ctrl+R** stops work or retries; **Ctrl+D** reviews source when workers are idle.

## Stop and retry

Ctrl+R stops unfinished planning or retries it after interruption or failure. An untouched worktree checks the remote again, refreshes its source and starts a fresh planner conversation. Confirmed starting snapshots and source already used for execution stay fixed. History, draft and queue are retained.

A valid plan is saved and displayed in the conversation. New plans proceed directly to execution. After an interruption, Ctrl+R explicitly resumes work.

Reopening restores progress; unfinished work waits for retry. Finished versions can update automatically from merged work, using an integration plan. Sending edits creates a new plan against the latest source, with checks for the change and regressions. The conversation stays; the current plan is replaced.
