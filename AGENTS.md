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
- Keep comments sparse and concise. Explain non-obvious decisions, not obvious code.
- Keep explanations concise too.
- Structure explanations and documentation with clear headings, short paragraphs and lists where useful.

## Finish each batch

- After implementing and verifying an approved batch, commit its changes and push to `main`. Include documentation changes.
- Committing and pushing are part of finishing the batch; no separate approval is needed.

## Keep the UX focused

- Every visible element and interaction must serve a requested or approved feature.
- Avoid filler text, duplicate instructions, and status labels for features that do not exist yet.
- Empty space is fine. Do not add copy or controls just to fill it.
- Place necessary guidance beside the control it explains; avoid disconnected text floating in the layout.
- Use purposeful terminal glyphs for controls and keyboard hints, with clear labels and no emoji rendering.
- Use `<code mod/>` as the section label and a glyph beside each mod name. Add agent status and counts when agents exist.
- Keep conversation rows compact and left aligned. Do not indent the transcript or composer to match the pet or header.
- Mark user messages with a visible `>` prefix rather than a "you" label.
- Keep pending instructions separate from conversation history. Queue edits must preserve the composer draft.
- Close list dialogs when no actionable items remain. Removing the last queued instruction returns to the composer with its draft intact.
- Steering delivers one or more instructions to relevant running workers. Reordering the queue is not steering; do not claim delivery without a connected worker.
- Until workers are connected, persist steering requests separately and show them as waiting for workers. Preserve message order and move them from the queue atomically.
- Use one dialog style for code mods and queues: matching alignment, spacing, selection and keyboard hints.
- The Sprowt pet is an original character with personality, not a rendering of the logo. Review it at its actual terminal size.
- Keep the pet's silhouette consistent. Prefer facial expressions and subtle motion over skewing its body.
