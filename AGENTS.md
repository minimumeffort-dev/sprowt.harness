# Working on Sprowt Harness

## Learn in small steps

- This is a learning project. Make each step small and easy to understand.
- Group related code and setup changes into small, coherent batches; avoid separate approvals for trivial steps.
- Before each batch, briefly explain the intended behavior, why it matters and the main design choices. Use a small diagram when it helps.
- Do not show code snippets or give file-by-file walkthroughs unless the user asks.
- Let the user give feedback or approve the proposal. Incorporate feedback and wait for approval before implementing the batch. "Approve", "ok" or equivalent agreement counts; that approval includes routine setup and verification.
- Afterward, briefly summarize what works, how to try it, what was verified and any relevant limitations.
- Avoid large implementation jumps or scaffolding the whole architecture at once.

## Build from the outside in

- Start with the user-facing CLI and high-level concepts.
- Define component responsibilities and interfaces before filling in internals.
- Develop one layer at a time, adding detail as it becomes necessary.

## Keep it lean

- Write short, clear code. Prefer readability over clever compression.
- Build only features the user requested or explicitly approved in a batch; avoid speculative abstractions and dependencies.
- Users describe outcomes. The harness chooses useful task splits, parallel work and necessary worker coordination.
- Keep comments sparse and concise. Explain non-obvious decisions, not obvious code.
- Keep explanations concise too.
- Structure explanations and documentation with clear headings, short paragraphs and lists where useful.
- Keep real credentials, local environment values and session data out of Git. Use placeholders in examples.

## Keep docs current

- Documentation is part of finishing every behavior, CLI, configuration or architecture change. Update affected guides, examples and diagrams in the same commit; update the README when its overview or setup changes.
- Verify docs against the current source and CLI. Describe implemented behavior; label proposals and illustrations explicitly. Replace stale content and keep history in Git.
- Follow the concise, example-led style of Stripe's docs: lead with the outcome, show the smallest usable example, state the expected result, then give the next action or recovery step.
- Prefer copyable commands and code examples to long explanations. State prerequisites and the working directory, label placeholders, and distinguish shell commands from internal agent payloads. Use fenced blocks with a language.
- Use small diagrams, tables and labelled visuals when they make a workflow or boundary easier to understand. Keep examples beside the instructions they explain; avoid decorative assets and filler.
- Keep paragraphs short and concrete, in our voice. Put optional internals below everyday instructions or in a collapsed reference section.
- Keep one canonical page per topic. Link to it instead of duplicating instructions, version pins or limits. Keep the docs index and incoming links current.
- Before committing, check changed commands, examples, links, keyboard hints and visuals. Report what was verified and any live behavior that was not exercised.

## Use the pinned coding skills

- Read [skills/POLICY.md](skills/POLICY.md) before applying upstream instructions. It adapts their tools, autonomy, reporting and review rules to Sprowt. Keep all compatibility changes separate from `skills/upstream`.
- For implementation and debugging, read [Ponytail](skills/upstream/ponytail/skills/ponytail/SKILL.md) and use its default **full** mode. Read the [executor instructions](skills/roles/executor.md), then select the relevant pstack workflow from [skills/INDEX.md](skills/INDEX.md).
- For planning, use the [planner instructions](skills/roles/planner.md) for decomposition, domain design, boundaries and verification. For verification, load pstack's Prove It Works and Test Behavior instructions from the index.
- Before completing a code change, apply correctness review and [Thermo-Nuclear structural review](skills/upstream/cursor-team-kit/skills/thermo-nuclear-code-quality-review/SKILL.md) together using the [reviewer instructions](skills/roles/reviewer.md). These same standards guide implementation.
- Keep initial context small. Read mandatory role files, then only the pstack leaves and references needed for the task. Do not load the whole plugin or enable its orchestration, hooks or setup scripts.
- Treat `skills` as `{{SKILLS_ROOT}}` during repository development. Upstream files must match [skills/manifest.json](skills/manifest.json), including licenses and required local references. Run `python3 examples/check_skills.py` after changing the bundle; follow [the update procedure](docs/skills.md#update-a-pin).

## Finish each batch

- Start every feature batch from the latest `origin/main`. Fetch before starting, and fetch again before pushing. Rebase or merge intervening changes, resolve conflicts and verify the combined result before publishing.
- When other chats share a checkout, use a separate worktree from the latest `origin/main`. Preserve their uncommitted work; never reset, stash, commit or overwrite another chat's changes.
- After implementing and verifying an approved batch, commit its changes and push to `main`. Include documentation changes.
- After each push, start the next batch from the updated `origin/main`. Use ordinary pushes; never force-push `main`.
- Committing and pushing are part of finishing the batch; no separate approval is needed.

## Keep both interfaces in sync

- Put workflow rules, permissions and persistence in the shared Rust engine. Terminal and SwiftUI code handle presentation and input.
- Every new feature must work in both the terminal and the macOS app in the same batch. Keep behavior and available actions consistent, while using each interface's native presentation. Keep the desktop protocol and its Rust/Swift contract checks current.
- Verify the user-facing flow in both interfaces before completing a feature. A feature available in only one interface is unfinished.
- Build the native bundle from the same checkout as its Rust executable. Run the Rust and Swift checks before completing a change to their shared boundary.

## Keep the UX focused

- Every visible element and interaction must serve a requested or approved feature.
- Avoid filler text, duplicate instructions, and status labels for features that do not exist yet.
- Empty space is fine. Do not add copy or controls just to fill it.
- Label empty codemod lists clearly. Keep repeated runtime and isolation guidance in the docs, not the conversation.
- Place necessary guidance beside the control it explains; avoid disconnected text floating in the layout.
- Use purposeful terminal glyphs for controls and keyboard hints, with clear labels and no emoji rendering.
- Write “codemod” as one word. Use `<codemod/>` as the section label and a glyph beside each mod name. Add agent status and counts when agents exist.
- Keep conversations left aligned, with one blank row between messages. Do not indent the transcript or composer to match the pet or header.
- Mark user messages with a visible `>` prefix and a subtle background. Label agent messages with their provider and role.
- Show plans as numbered tasks with short outcomes and dependencies. Put file scopes, checks and model information behind one details toggle.
- Highlight progress, errors and the next useful action. Hide stale planning chatter after a plan is ready; keep the saved history intact.
- Show keyboard hints when their actions are relevant. Keep each hint beside its control.
- Keep pending instructions separate from conversation history. Queue edits must preserve the composer draft.
- Close list dialogs when no actionable items remain. Removing the last queued instruction returns to the composer with its draft intact.
- Steering delivers one or more instructions to relevant running workers. Reordering the queue is not steering; do not claim delivery without a connected worker.
- Until workers are connected, persist steering requests separately and show them as waiting for workers. Preserve message order and move them from the queue atomically.
- Use one dialog style for codemods and queues: matching alignment, spacing, selection and keyboard hints.
- The Sprowt pet is an original character with personality, not a rendering of the logo. Review it at its actual terminal size.
- Keep the pet's silhouette consistent. Prefer facial expressions and subtle motion over skewing its body.
