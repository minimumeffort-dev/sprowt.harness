# Working on Sprowt Harness

## Learn in small steps

- This is a learning project. Make each step small and easy to understand.
- Before coding, briefly explain what the step adds and why.
- Group related code and setup changes into small, coherent batches. Explain the important pieces together; avoid separate approvals for trivial steps.
- Wait for the user's "approve" or "ok" before implementing a proposed batch. That approval includes its routine setup and verification.
- Afterward, explain how to run or verify it and the main concept introduced.
- Avoid large implementation jumps or scaffolding the whole architecture at once.

## Build from the outside in

- Start with the user-facing CLI and high-level concepts.
- Define component responsibilities and interfaces before filling in internals.
- Develop one layer at a time, adding detail as it becomes necessary.

## Keep it lean

- Write short, clear code. Prefer readability over clever compression.
- Build only what the current step needs; avoid speculative abstractions and dependencies.
- Keep comments sparse and concise. Explain non-obvious decisions, not obvious code.
- Keep explanations concise too.
