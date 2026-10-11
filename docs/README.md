# Documentation

Build a change, understand its progress, and publish a checked PR.

**New here? [Start with your first codemod →](quickstart.md)**

## Use the harness

| Guide | What you will learn |
| --- | --- |
| [Quickstart](quickstart.md) | Install, sign in, describe a change and publish |
| [Terminal](terminal.md) | Navigate the timeline, inspect evidence and use shortcuts |
| [Codemods and messages](codemods.md) | Manage work, queue edits and steer active workers |
| [Review and repairs](review.md) | Read findings, start fixes and understand repair limits |
| [Git workflow](git-workflow.md) | Sync branches, publish, update, close and reopen |
| [Troubleshooting](troubleshooting.md) | Find the next action for a specific failure |

## Understand the system

Start with the [architecture overview](architecture.md), then follow the part you need.

| Topic | Responsibility |
| --- | --- |
| [Planning and routing](planning.md) | Task contracts, dependencies, provider choice and reasoning effort |
| [Execution and checks](execution.md) | Task isolation, source integration, repeatable checks and recovery |
| [Worker communication](coordination.md) | Peer questions, answers and saved delivery |
| [Workers](workers.md) | Provider connections, supported versions and Muse recovery |
| [Sandbox](sandbox.md) | Linux environments, runtimes, packages and network access |

## Reference and contribution

| Reference | Contents |
| --- | --- |
| [CLI and keyboard reference](terminal.md#keyboard-reference) | Commands, shortcuts and context-specific actions |
| [Harness tools](tools.md) | Tool callers, payload examples and validation boundaries |
| [Local state](local-state.md) | Storage paths, retention, credentials and restart behavior |
| [Development](development.md) | Local checks, live diagnostics and the source map |

## A few terms

| Term | Meaning |
| --- | --- |
| **Codemod** | One requested outcome, with its own plan, messages, branch and working environment |
| **Task** | One scoped piece of a codemod's plan |
| **Worker** | An agent connection assigned to planning, execution or review |
| **Combined source** | The tasks' integrated changes, checked together before review |
| **Current evidence** | Checks or review tied to the exact source being considered for publication |

[Back to the project](../README.md)
