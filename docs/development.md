[Docs](README.md) / Contribute

# Development

Build one small, coherent change at a time. Start with the user-facing behavior, keep the implementation and docs together, and follow [AGENTS.md](../AGENTS.md).

## Local checks

From the harness repository:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
python3 -m unittest discover -s examples -p 'test_*.py'
```

The default Rust suite omits live tests marked `ignored`. The Python suite exercises broker and transport behavior without live provider sessions. Passing these suites does not establish live provider or VM compatibility.

To inspect CLI help without starting the interactive app:

```sh
cargo run --locked -- --help
```

To try the UI from the repository in a real terminal:

```sh
cargo run --locked -- --no-motion
```

Creating a codemod from that UI starts planning and execution against this repository. For product work, install the binary and launch it from the intended project instead.

## Live checks

Use the [supported provider versions](workers.md#supported-versions), sign in and start Apple Container first. Run the check relevant to the boundary you changed.

| Area | Focused command |
| --- | --- |
| Saved check scripts and restart | `cargo test shared_check_runner_recovers_and_survives_restart -- --ignored` |
| Task worktree isolation and cleanup | `cargo test task_worktrees_isolate_combine_restore_and_clean -- --ignored` |
| Network and source export | `cargo test persistent_vm_checks_network_exports_source_and_reconnects -- --ignored` |
| Browser execution boundary | `cargo test browser_turn_and_verification_preserve_boundaries -- --ignored` |
| Muse disconnect recovery | `cargo test muse_adapter_reconnects_once -- --ignored` |
| Independent review and repair | `cargo test independent_review_repairs_a_seeded_bug_and_rechecks_in_vm -- --ignored` |

These checks use disposable fixtures and VMs. Provider-driven checks consume the configured account's usage; shared container images remain for reuse. Inspect a test's `#[ignore]` note and setup before running it.

<details>
<summary>Additional integration checks</summary>

```sh
cargo test tools::tests::package_calls -- --ignored
cargo test git_mod::tests::publishing_keeps_the_vm -- --ignored
cargo test codex::tests::task_turn -- --ignored
cargo test codex::tests::two_clients_share_a_vm -- --ignored
cargo test both_workers_resume_after_codemod_network_approval -- --ignored
cargo test repair_handoff_preserves_drafts_and_rechecks_in_vm -- --ignored
cargo test codex_verifier_hands_a_regression_back_to_muse -- --ignored
cargo test verification_feedback_recovers_both_vm_workers -- --ignored
cargo test automatic_provider_assignment_runs_in_vm -- --ignored
cargo test worker_slots_follow_provider_waves_in_vm -- --ignored
```

The publication test uses a local GitHub fixture. The approval test exercises both providers through a blocked download, user approval and retry. The provider-wave tests check automatic/fixed assignments and real overlap.

</details>

To discover available tests without running them:

```sh
cargo test --locked -- --list
```

Standalone compatibility probes live in [examples](../examples). They are separate from production scheduling; see [Workers](workers.md#standalone-diagnostics).

## Source map

| Responsibility | Entry points |
| --- | --- |
| CLI and application loop | [`main.rs`](../src/main.rs), [`app.rs`](../src/app.rs) |
| State and actions | [`app/state.rs`](../src/app/state.rs), [`app/actions.rs`](../src/app/actions.rs) |
| Timeline and inspection | [`app/timeline.rs`](../src/app/timeline.rs), [`ui/timeline.rs`](../src/ui/timeline.rs), [`ui/inspector.rs`](../src/ui/inspector.rs) |
| Terminal rendering and companion | [`ui.rs`](../src/ui.rs), [`sprout.rs`](../src/sprout.rs) |
| Plans, assignment and routing | [`plan.rs`](../src/plan.rs), [`scheduler.rs`](../src/scheduler.rs), [`router.rs`](../src/router.rs) |
| Worker orchestration | [`worker.rs`](../src/worker.rs), [`agents.rs`](../src/agents.rs) |
| Provider connections | [`codex.rs`](../src/codex.rs), [`muse.rs`](../src/muse.rs), [`muse_bridge.py`](../src/muse_bridge.py), [`muse_transport.py`](../src/muse_transport.py) |
| Checks and recovery | [`checks.rs`](../src/checks.rs), [`execution.rs`](../src/execution.rs), [`repair.rs`](../src/repair.rs), [`conflict.rs`](../src/conflict.rs) |
| Review and findings | [`review.rs`](../src/review.rs), [`review/ledger.rs`](../src/review/ledger.rs) |
| Tools, messaging and permissions | [`tools.rs`](../src/tools.rs), [`mailbox.rs`](../src/mailbox.rs), [`network.rs`](../src/network.rs), [`packages.rs`](../src/packages.rs) |
| VM and task worktrees | [`sandbox.rs`](../src/sandbox.rs), [`task_worktree.rs`](../src/task_worktree.rs) |
| Git and checkpoints | [`git_mod.rs`](../src/git_mod.rs), [`git_sync.rs`](../src/git_sync.rs), [`mod_sync.rs`](../src/mod_sync.rs), [`workspace.rs`](../src/workspace.rs) |
| Persistence | [`store.rs`](../src/store.rs) |

## Keep documentation usable

Use [docs/README.md](README.md) as the navigation hub. Keep one canonical page per topic and link to it from related guides.

- **Quickstarts:** outcome, prerequisites, runnable steps, expected results, next action.
- **Task guides:** what to do, what happens, and how to recover.
- **References:** exact fields, limits, defaults and ownership.
- **Architecture:** responsibilities and boundaries, with a small diagram when helpful.

Put implementation detail below everyday instructions. Use tables for comparisons, code blocks with a language for commands/payloads, and labelled illustrations for concepts. Keep real sessions, credentials and local environment values out of examples.

When behavior changes, check linked pages, keyboard hints, version pins, examples and diagrams together. Remove outdated claims rather than appending a history of fixes. Verify links and render visuals before committing.

<details>
<summary>Documentation references</summary>

Useful examples of this structure: [ElevenLabs quickstarts](https://elevenlabs.io/docs/eleven-api/quickstart/), [Supabase step-by-step guides](https://supabase.com/docs/guides/getting-started/quickstarts/reactjs), [Neon workflow explanations](https://neon.com/docs/get-started-with-neon/workflow-primer) and [Stripe integration paths](https://docs.stripe.com/payments/checkout/quickstarts).

</details>

**Next:** [Architecture](architecture.md) · [Tool extension points](tools.md#extend-the-dispatcher)
