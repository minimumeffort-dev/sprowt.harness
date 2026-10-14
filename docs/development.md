[Docs](README.md) / Contribute

# Development

Build one small, coherent change at a time. Start with the user-facing behavior, keep the implementation and docs together, and follow [AGENTS.md](../AGENTS.md).

Read the [pinned coding skills](skills.md#use-them-in-development) for your role before planning, implementation or review. Ponytail uses full mode; load relevant pstack instructions as needed.

## Local checks

From the harness repository:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
python3 -m unittest discover -s examples -p 'test_*.py'
python3 examples/check_skills.py
swift test --package-path macos
```

The default Rust suite omits live tests marked `ignored`. The Python suite exercises broker and transport behavior without live provider sessions. Passing these suites does not establish live provider or VM compatibility.

The creation editor tests exercise Markdown source preservation, Unicode selection, native formatting and undo, slash menu cancellation, list and heading editing, marked text, and the unchanged description command. Terminal tests exercise the same formatting actions through keyboard and click controls, including literal paths, multiline paste and menu rendering. Automated tests do not record audio. Verify the native Dictate control and Edit menu's system action in a disposable app session without treating action dispatch as proof of speech recognition.

Navigation checks exercise engine commands, terminal keys and clicks, and native request acknowledgements. They cover both drafts, forward-path replacement, closed and deleted destinations, dialogs, and delayed inspections. To export the actual terminal navigation buffers at 40, 80 and 160 columns:

```sh
SPROWT_UI_CAPTURE_DIR=/tmp/sprowt-navigation-captures \
  cargo test --locked terminal_navigation_controls_render_and_click
```

**Expected:** the directory contains text and JSON cell buffers with working Back and Forward controls. Native verification also checks title-bar arrows and keyboard commands with the sidebar hidden.

Appearance tests use public commands and terminal keys to check persistence across projects, rejection of invalid values, save failures and draft/caret preservation. A shared fixture compares the actual resolved native and terminal colors and checks text contrast. Export all five terminal views and the appearance picker in both themes:

```sh
SPROWT_APPEARANCE_BUFFERS=/tmp/sprowt-appearance-captures \
  cargo test --locked appearance_palettes_paint_all_workspace_and_dialog_cells_in_both_themes
```

**Expected:** light and dark text and JSON cell buffers at 40, 80 and 160 columns, with explicit foreground and background colors.

Project navigation tests exercise the real engine commands, native draft bridge and terminal keys/clicks. They cover accordion expansion without navigation, cross-project draft restoration, retained worker sessions, background event/job/preparation progress, scoped network approvals, failed switches and cleanup of every runtime on exit. Shared protocol fixtures include project activity; terminal and native tests preserve the visible draft while background status changes.

Worker settings tests cover catalog validation, model pins, configured counts, safe quota replacement, uncertain receipts, shared cooldowns, restart and legacy migration. Export the actual terminal dialog after key interactions:

```sh
SPROWT_WORKERS_PREVIEW=/tmp/sprowt-workers-captures \
  cargo test --locked app::tests::worker_settings
```

For native interface changes, [build the macOS bundle](macos.md#build-and-open) and exercise the changed flow in the app. The Rust and Swift protocol checks must agree before a shared boundary change is complete.

To inspect CLI help without starting the interactive app:

```sh
cargo run --locked -- --help
```

To try the UI from the repository in a real terminal:

```sh
cargo run --locked -- --no-motion
```

Creating a codemod from that UI starts planning and execution against this repository. For product work, install the binary and launch it from the intended project instead.

### Check workspace navigation

Run the focused interaction tests, then export terminal buffers at desktop and narrow sizes:

```sh
cargo test --locked workbench
SPROWT_WORKSPACE_BUFFERS=/tmp/sprowt-workspace-captures \
  cargo test --locked export_workspace_buffers -- --ignored --nocapture
```

**Expected:** interaction tests pass; the output directory contains JSON cell buffers with symbols, colors and positions. The captures use sample tasks and saved results, including all five workspace views. Render and inspect these buffers before updating the documentation visual. Verify tab changes, task drilldown, saved history and queue dismissal preserve the composer draft and caret.

## Live checks

Use the [supported provider versions](workers.md#supported-versions), sign in and start Apple Container first. Run the check relevant to the boundary you changed.

| Area | Focused command |
| --- | --- |
| Managed Muse preparation and local account discovery (no VM or model inference; can download release binaries) | `cargo test --locked installed_subscription_accounts_are_available -- --ignored --nocapture` |
| Empty preparation, accepted source binding, explicit memory and retained allocation (no inference) | `cargo test --locked early_vm_is_empty_then_binds_accepted_source_and_preserves_bound_allocation -- --ignored --nocapture` |
| Pinned skill delivery and read-only access (no inference) | `cargo test --locked pinned_skills_are_read_only_for_host_and_vm_agents -- --ignored --nocapture` |
| Muse helper reuse and corruption recovery (no inference) | `cargo test --locked muse_helpers_reuse_verified_content_and_recopy_corruption -- --ignored --nocapture` |
| Retained v1 VM setup migration (no inference; local v1 image required) | `cargo test --locked retained_v1_vm_migrates_stable_helpers_and_keeps_source_and_runtime -- --ignored --nocapture` |
| Selected Muse planner, read-only VM and generated plan | `cargo test selected_muse_planner_reads_source_and_returns_a_plan -- --ignored --nocapture` |
| Selected Muse reviewer, read-only VM and known defect | `cargo test selected_muse_reviewer_finds_defect_without_editing_source -- --ignored --nocapture` |
| Commit and PR text (Codex only; no push) | `cargo test live_publication_writer -- --ignored --nocapture` |
| Preparation download approvals, blocked baselines and both provider sessions (no inference or downloads) | `cargo test --locked live_preparation_network_requests_pause_commands_and_resume_scoped_attempts -- --ignored --nocapture` |
| Preparation reuse, tracked test caches and genuine source/lockfile changes (no inference) | `cargo test --locked live_preflight_retains_evidence_and_reuses_only_proven_setup -- --ignored --nocapture` |
| Tracked generated metadata and parallel source integration (no inference) | `cargo test --locked tracked_generated_metadata_does_not_conflict_with_parallel_source_edits -- --ignored --nocapture` |
| Prepared assets, private HOMEs and locked dependencies (no inference or downloads) | `cargo test --locked live_prepared_assets_are_private_and_locked_dependencies_reuse_only_exact_inputs -- --ignored --nocapture` |
| Independent checks, private paths, package waits and stale receipt rejection (no inference or downloads; cached image required) | `cargo test --locked shared_dispatcher_overlaps_isolated_checks_and_rejects_stale_receipts -- --ignored --nocapture` |
| Saved-check repair, peer source and fresh combined verification (no inference or downloads; cached image required) | `cargo test --locked requested_check_repair_preserves_combined_source_and_saved_scripts -- --ignored --nocapture` |
| Dependent source handoff, retained edits/scripts, retry and restart (no inference or downloads; cached image required) | `cargo test --locked dependent_delivery_refreshes_upfront_branch_and_preserves_draft_across_retry_and_restart -- --ignored --nocapture` |
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
| CLI and terminal loop | [`main.rs`](../src/main.rs), [`app.rs`](../src/app.rs) |
| Shared session and project runtimes | [`session.rs`](../src/session.rs), [`engine.rs`](../src/engine.rs), [`engine`](../src/engine) |
| State and actions | [`engine/state.rs`](../src/engine/state.rs), [`engine/actions.rs`](../src/engine/actions.rs) |
| Native command protocol | [`desktop.rs`](../src/desktop.rs), [`engine/api.rs`](../src/engine/api.rs), [`engine/snapshot.rs`](../src/engine/snapshot.rs) |
| Native macOS interface | [`macos/Sources`](../macos/Sources), [build and development guide](macos.md) |
| Workspace views and navigation | [`engine/workspace_view.rs`](../src/engine/workspace_view.rs), [`app/workspace_navigation.rs`](../src/app/workspace_navigation.rs), [`app/controls.rs`](../src/app/controls.rs), [`ui/workbench.rs`](../src/ui/workbench.rs) |
| Dock and evidence views | [`ui/dock.rs`](../src/ui/dock.rs), [`ui/workbench/evidence.rs`](../src/ui/workbench/evidence.rs), [`ui/inspector.rs`](../src/ui/inspector.rs) |
| Terminal rendering and companion | [`ui.rs`](../src/ui.rs), [`sprout.rs`](../src/sprout.rs) |
| Plans, assignment and routing | [`plan.rs`](../src/plan.rs), [`scheduler.rs`](../src/scheduler.rs), [`router.rs`](../src/router.rs) |
| Project preparation, bounded asset reuse and baseline receipts | [`environment.rs`](../src/environment.rs), [`environment/cache.rs`](../src/environment/cache.rs), [`environment_cache.py`](../sandbox/environment_cache.py) |
| Worker orchestration | [`worker.rs`](../src/worker.rs), [`agents.rs`](../src/agents.rs) |
| Startup ownership and measurements | [`engine/preparation.rs`](../src/engine/preparation.rs), [`codex/shared_vm.rs`](../src/codex/shared_vm.rs), [`diagnostics.rs`](../src/diagnostics.rs), [`diagnostics/timing.rs`](../src/diagnostics/timing.rs) |
| Provider connections | [`codex.rs`](../src/codex.rs), [`muse.rs`](../src/muse.rs), [`muse_bridge.py`](../src/muse_bridge.py), [`muse_transport.py`](../src/muse_transport.py) |
| Tested Muse releases, downloads and private binary cache | [`muse_release.py`](../src/muse_release.py) |
| Pinned skill delivery | [`skills.rs`](../src/skills.rs), [`build.rs`](../build.rs), [`skills`](../skills/INDEX.md) |
| Checks and recovery | [`checks.rs`](../src/checks.rs), [`execution.rs`](../src/execution.rs), [`repair.rs`](../src/repair.rs), [`conflict.rs`](../src/conflict.rs) |
| Review and findings | [`review.rs`](../src/review.rs), [`review/ledger.rs`](../src/review/ledger.rs) |
| Tools, messaging and permissions | [`tools.rs`](../src/tools.rs), [`mailbox.rs`](../src/mailbox.rs), [`network.rs`](../src/network.rs), [`packages.rs`](../src/packages.rs) |
| Immutable task commands and verified changed-file exports | [`sandbox/session.rs`](../src/sandbox/session.rs), [`sandbox/environment_gate.rs`](../src/sandbox/environment_gate.rs), [`sandbox/transfer.rs`](../src/sandbox/transfer.rs), [`source_transfer.py`](../sandbox/source_transfer.py) |
| VM setup, admission and task worktrees | [`sandbox.rs`](../src/sandbox.rs), [`sandbox/preparation.rs`](../src/sandbox/preparation.rs), [`sandbox/resources.rs`](../src/sandbox/resources.rs), [`task_worktree.rs`](../src/task_worktree.rs) |
| Git and checkpoints | [`git_mod.rs`](../src/git_mod.rs), [`git_sync.rs`](../src/git_sync.rs), [`mod_sync.rs`](../src/mod_sync.rs), [`workspace.rs`](../src/workspace.rs) |
| Commit and PR text | [`publication.rs`](../src/publication.rs) |
| Persistence | [`store.rs`](../src/store.rs) |

## Keep documentation usable

Follow the [documentation rules in AGENTS.md](../AGENTS.md#keep-docs-current). Use [docs/README.md](README.md) as the navigation hub and link to the canonical guide for each topic.

- **Quickstarts:** outcome, prerequisites, runnable steps, expected results, next action.
- **Task guides:** what to do, what happens, and how to recover.
- **References:** exact fields, limits, defaults and ownership.
- **Architecture:** responsibilities and boundaries, with a small diagram when helpful.

### Check a documentation change

From the harness repository, inspect the public CLI and patch:

```sh
cargo run --locked -- --help
git diff --check
git diff -- README.md AGENTS.md docs/
```

**Expected:** help matches the documented flags; the whitespace check prints nothing; the diff describes current behavior.

| Changed content | Verify before committing |
| --- | --- |
| Command or example | Syntax, prerequisites, working directory and expected result; run safe examples in a disposable fixture |
| Tool payload or recipe | Parse the JSON and compare fields and limits with the current schema |
| Link or heading | Open the destination, including relative paths and anchors |
| Keyboard hint or status | Compare with the action handler and visible label |
| Diagram or illustration | Render it and check labels, wrapping and correspondence with the workflow |

Keep credentials and real session data out of examples. State when a live provider or VM check was not run.

<details>
<summary>Documentation references</summary>

Useful examples of this structure: [ElevenLabs quickstarts](https://elevenlabs.io/docs/eleven-api/quickstart/), [Supabase step-by-step guides](https://supabase.com/docs/guides/getting-started/quickstarts/reactjs), [Neon workflow explanations](https://neon.com/docs/get-started-with-neon/workflow-primer) and [Stripe integration paths](https://docs.stripe.com/payments/checkout/quickstarts).

</details>

**Next:** [Architecture](architecture.md) · [Tool extension points](tools.md#extend-the-dispatcher)
