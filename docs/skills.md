[Docs](README.md) / Coding skills

# Coding skills

Harness development and harness agents use pinned Ponytail, pstack and Thermo-Nuclear review instructions. The upstream files stay unchanged. Sprowt's [compatibility policy](../skills/POLICY.md) defines how they work with our roles, permissions and reports.

## Use them in development

Start with [AGENTS.md](../AGENTS.md#use-the-pinned-coding-skills), then read the relevant role instructions. Use this repository's `skills` directory wherever a role says `{{SKILLS_ROOT}}`.

| Work | Required guidance |
| --- | --- |
| Planning | Ponytail full; pstack decomposition, domain design, boundaries and verification |
| Implementation or debugging | Ponytail full; relevant pstack workflow; Thermo-Nuclear quality standards |
| Verification | Prove It Works and Test Behavior; the project's actual checks |
| Review | Correctness and Thermo-Nuclear structural review together |

The [skill index](../skills/INDEX.md) links the specific files. Read only the pstack instructions and references needed for the task. Retaining the full plugin preserves local references; it does not enable every workflow.

## Harness agents

Each agent connection receives the shared policy and its role instructions. The role lists mandatory skill files to read first, followed by relevant pstack workflows. The full plugin stays out of the initial prompt.

| Agent | Delivery |
| --- | --- |
| Planner | Policy and planner instructions; pinned files readable in the host sandbox |
| Codex executor | Policy and executor instructions in thread start, resume and fresh fallback |
| Muse executor | Policy and executor instructions in each task turn, including resumed sessions |
| Reviewer | Policy and reviewer instructions with correctness and structural review in the same report |
| Publication writer | Shared policy with its existing evidence-only, no-tools JSON contract |

The compiled binary contains the bundle. No upstream downloads or global plugin installation happen at agent startup.

```text
Repository skills/ -> compiled harness
                         |
                         +-- planner: private host copy, read-only
                         +-- executor/reviewer: /opt/sprowt-skills/<version>
```

The bundle version changes when its contents change. VM copies sit outside project source and task worktrees. Workers receive read-only access; these copies are excluded from task source snapshots.

To activate an updated bundle, install from the harness repository:

```sh
cargo install --path . --locked --force
```

Restart the harness before creating or resuming work. Already-running connections retain the instructions from their current harness process. Resumed work preserves accepted user instructions and receives the current build's policy.

The harness verifies bundle delivery and read permissions. Applying the instructions remains model behavior. Existing checks, independent review and publication gates still decide whether the work is ready.

## What is pinned

[The manifest](../skills/manifest.json) records full commits, original paths and SHA-256 hashes for every upstream file.

| Source | Commit | Retained content | License |
| --- | --- | --- | --- |
| [Ponytail](https://github.com/dietrichgebert/ponytail/tree/9b58c1ffb790c075ca32e70a89cf1d80588f4abf) | `9b58c1f` | Main skill, full mode by default | [MIT](../skills/upstream/ponytail/LICENSE) |
| [pstack](https://github.com/cursor/plugins/tree/11713c683fe4add4fd403ea880580abbc9e0875f/pstack) | `11713c6` | Complete plugin, including skills, references, playbooks and helpers | [MIT](../skills/upstream/pstack/LICENSE) |
| [Thermo-Nuclear review](https://github.com/cursor/plugins/blob/11713c683fe4add4fd403ea880580abbc9e0875f/cursor-team-kit/skills/thermo-nuclear-code-quality-review/SKILL.md) | `11713c6` | Review skill | [MIT](../skills/upstream/cursor-team-kit/LICENSE) |

From the harness repository:

```sh
python3 examples/check_skills.py
```

**Expected:** `Verified 169 pinned upstream files.` A changed, missing or extra file fails verification.

## How compatibility works

```text
Sprowt policy + role instructions
              |
              +-- required skill files
              +-- relevant pstack workflow and references
              |
              v
Existing plan, task or review report
```

- Sprowt controls task ownership, models, worker coordination, checks and publication.
- Cursor-specific delegation and control steps use available Sprowt tools. Missing tools are reported as limitations.
- Upstream autonomy instructions do not grant permissions. Bundled setup scripts, hooks and PR watchers are inactive.
- Structural defects need concrete source evidence and acceptance criteria. Unrelated redesigns stay informational. Correctness checks and review finding identity remain intact.
- External skill references are documented in [the compatibility policy](../skills/POLICY.md#adapt-upstream-workflows). They are not silently installed.

## Update a pin

1. Check out the intended upstream commit in a separate clone. Read the changed instructions and license.
2. Replace the retained upstream files byte for byte, including required references. Keep adaptations in `skills/POLICY.md`, `skills/INDEX.md` or `skills/roles`.
3. Update the package commit, source paths and SHA-256 hashes in `skills/manifest.json`. Update this page's pin table and verified file count.
4. Compare the bundle against the pinned clones, then run the development checks.

With the clone paths substituted:

```sh
python3 examples/check_skills.py --sources /path/to/ponytail /path/to/cursor-plugins
python3 -m unittest discover -s examples -p 'test_skills.py'
```

**Expected:** the clone commits, file contents, licenses and manifest agree. Review the diff before committing. Do not regenerate hashes merely to accept a local upstream edit.

**Next:** [Development checks](development.md#local-checks) · [Review and repairs](review.md)
