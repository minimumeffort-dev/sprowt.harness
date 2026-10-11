[Docs](README.md) / Understand the system

# Project preparation

Before a task starts editing, the controller prepares its Linux environment and records the starting test results. Codex and Muse use the same preparation path.

```mermaid
flowchart LR
    recipe["Recipe + image + setup inputs"] --> copy["Copy matching prepared assets"]
    copy --> setup["Run setup, unless locked dependencies are proven"]
    setup --> probes["Prove current capabilities"]
    probes -->|"Pass"| baseline["Run current baseline"] --> worker["Start worker"]
    probes -->|"Fail"| pause["Pause with setup evidence"]
```

## Where the recipe comes from

The planner reads project manifests and supplies setup commands, capability probes and baseline checks.

| Available recipe | Used by the controller |
| --- | --- |
| Explicit recipe in the accepted plan | The plan's recipe, including corrections made during replanning |
| No plan recipe, but `.sprowt/environment.json` exists | The project file |
| Neither | Worker-managed setup |

## Add a project recipe

This Node example needs a committed `package-lock.json` and an existing `npm test` script. Choose the pinned Node version supported by your project.

From the **project root**, create the configuration directory:

```sh
mkdir -p .sprowt
```

Save the following as **`.sprowt/environment.json`**:

```json
{
  "inputs": ["package.json", "package-lock.json"],
  "system_packages": [],
  "reusable_paths": [],
  "setup": [{
    "name": "Install Node",
    "argv": ["/usr/local/bin/mise", "install", "node@22.16.0"],
    "timeout_seconds": 180
  }, {
    "name": "Install locked dependencies",
    "argv": ["/usr/local/bin/mise", "exec", "node@22.16.0", "--", "npm", "ci"],
    "timeout_seconds": 180
  }],
  "capabilities": [{
    "name": "Node runtime",
    "argv": ["/usr/local/bin/mise", "exec", "node@22.16.0", "--", "node", "--version"],
    "timeout_seconds": 30
  }],
  "baseline": [{
    "name": "Existing tests",
    "argv": ["/usr/local/bin/mise", "exec", "node@22.16.0", "--", "npm", "test"],
    "timeout_seconds": 180
  }]
}
```

Check the file's JSON syntax on your Mac:

```sh
python3 -m json.tool .sprowt/environment.json > /dev/null
```

**Expected:** exit status 0 with no output. This checks JSON syntax only. The harness validates the recipe and runs its Linux commands when a task starts.

Commit the recipe with the project. Keep it free of credentials. If a revised plan corrects setup, update this file for future codemods too.

Browser projects also need a real browser startup probe and the required Debian libraries. Start the server and browser inside one probe. See [browser checks](sandbox.md#browser-checks).

### Recipe fields

| Field | Use | Limit |
| --- | --- | --- |
| `inputs` | Exact project-relative files that affect setup reuse | Up to 32 paths |
| `system_packages` | Required signed Debian package names | [Package policy](sandbox.md#system-packages) |
| `reusable_paths` | Optional locked dependency snapshot; defaults to `[]` | Only `["node_modules"]` |
| `setup` | Install the pinned runtime and locked dependencies | Up to 12 commands |
| `capabilities` | Prove the runtime or browser actually starts | 1–12 commands |
| `baseline` | Record tests before editing | Up to 12 commands |

Each command needs `name`, `argv` and `timeout_seconds` (1–180). Begin `argv` with an absolute executable. Use project-relative file arguments; temporary task and worker paths are rejected.

## What the results mean

| Phase | Failure behavior |
| --- | --- |
| System packages or setup | Pause before sending an editing turn |
| Capability probes | Pause and retain the failed command and output |
| Baseline tests | Save the failure and let the worker repair the starting source |
| Source changed during preparation | Preserve the edits and pause; fix the recipe |

Open **Task Details** for preparation results; **e** includes passed commands. On setup failure, **Revise repair plan** carries the recipe and evidence into planning. Blocked downloads still require [network approval](sandbox.md#downloads).

Preparation evidence also reports copied bytes, dependency reuse and cache rejection. Results belong to that attempt's source and worker HOME.

Baseline results never satisfy completion checks, clear review findings or unlock publication. Final checks still run on the combined source.

## Reuse across tasks

Each codemod VM holds an immutable cache of prepared assets. Tasks receive private copies, so changing one worker's installation cannot change another worker's files or the saved cache.

| Asset | Default behavior |
| --- | --- |
| Mise runtime installations | Copy verified files into a missing private installation directory |
| npm content downloads | Copy content blobs; omit index, configuration and authentication files |
| pip wheels | Copy wheel files; omit download-origin metadata and HTTP caches |
| `node_modules` | Copy only when explicitly declared reproducible; otherwise rerun setup |
| HOME, provider state, credentials, history, source and `.venv` | Never shared or cached as environment roots |

The key includes the **validated image digest and architecture**, recipe, declared setup inputs, common manifests/lockfiles and cache policy. Ordinary source edits leave this preparation key unchanged. A changed lockfile, image, recipe or declared input starts fresh preparation.

Runtime and download reuse still runs setup in every new task. Setup can reuse local downloads. The cache never overwrites an existing destination.

### Opt in to locked Node dependencies

For a deterministic `npm ci` project, change the recipe field to:

```json
"reusable_paths": ["node_modules"]
```

**Prerequisites:** declare both `package.json` and `package-lock.json` in `inputs`, pin Node, and include every additional setup input. For example, declare `scripts/install.js` when an install script reads it. Do not opt in when installation embeds a task path, consumes undeclared source files or depends on secrets or mutable external state.

**Expected:** the first successful setup saves a verified snapshot. A matching new task can skip setup only after the complete snapshot is copied and its capability probes pass. An absent snapshot, existing dependency directory, changed input or failed probe runs setup again. Private configuration or history inside `node_modules` rejects the snapshot, rather than silently omitting required files.

- Every reuse reruns capability probes in the current VM and worker HOME. Failed probes trigger one setup retry.
- Repeating an attempt can retain preparation only in the same worker HOME with the same key and no saved preparation error.
- Baselines run on every editing attempt against current source. Final checks remain separate.

### Cache failure and limits

Missing, corrupt or unsupported assets fall back to fresh setup. An interrupted operation stops the attempt. Copies check content, file modes and relative links; absolute links, escaping link chains, hard links and special files are rejected.

The cache retains up to **3 environments per codemod**, each at most **512 MiB / 100,000 entries**. Publication is serialized; restores create protected staged copies, then move them into private paths. A copy or publication has a 90-second budget. The next operation removes interrupted staging copies while preserving active copies and worker source. These limits may prevent reuse for large dependency trees; they do not prevent normal setup.

Setup and baseline commands must leave source and lockfiles unchanged. Put generated dependencies and caches in their usual excluded locations. Commands retain normal worker permissions.

Private `environments/` receipts preserve commands, output, source fingerprints, worker HOME, cache decisions and errors across restart. Receipt files use mode `0600`. New task IDs get their own preparation records. The cache stays inside the codemod VM; deleting that VM also removes its prepared assets.

Older plans without a recipe continue with worker-managed setup. They gain controller preparation when replanned or when the project supplies the recipe file. Existing recipes without `reusable_paths` retain the default `[]`. Reuse does not reproduce external package registries; declared inputs, immutable assets and current probes bound what is reused.

**Next:** [Execution and final checks](execution.md) · [Troubleshooting](troubleshooting.md)
