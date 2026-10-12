[Docs](README.md) / Get started

# Your first codemod

Install the harness, make a small change, and publish it after checks and review pass.

## Before you begin

| Requirement | Why it is needed |
| --- | --- |
| Apple silicon and macOS 26+ | Apple Container runs the Linux workers |
| [Rust](https://rustup.rs) | Build and install the harness |
| [Codex CLI](https://github.com/openai/codex), supported version | Planning, execution and independent review |
| A ChatGPT account login in Codex | Authenticate the host connection |
| [GitHub CLI](https://cli.github.com/) | Publish a PR when ready |

Check the [supported CLI versions](workers.md#supported-versions) before starting. Muse is optional: without an installed or managed copy, Codex handles execution. Sprowt prepares its own tested Muse release automatically; a signed-out account still needs login.

## 1. Prepare the workers

Configure Codex to use a file-backed login. The harness keeps these credentials on the Mac.

```sh
codex -c 'cli_auth_credentials_store="file"' login
```

Install and start [Apple Container](https://github.com/apple/container):

```sh
brew install container
container system start --enable-kernel-install
```

**Expected:** Codex is signed in and the container service is running. The harness creates the codemod's Linux environment when execution starts; the first setup can take longer.

<details>
<summary>Add Muse to the worker pool</summary>

Install Muse using [Meta's distribution instructions](https://dev.meta.ai/docs/muse-code), then sign in:

```sh
muse login
```

Muse joins automatically. There is no `--muse` flag to enable it. The adapter needs host `python3`; first startup can download the tested release. Your standalone CLI stays unchanged. See [automatic Muse setup](workers.md#automatic-muse-setup) and [Muse recovery](workers.md#muse-recovery).

</details>

## 2. Install the harness

From your checkout of this repository:

```sh
cargo install --path . --locked --force
sprowt-harness --help
```

**Expected:** help lists `setup`, `--no-motion` and `--closed-worktree-days`. If the command is not found, make sure Cargo's binary directory is on your shell's `PATH`.

To use the native interface, follow [Build and open the macOS app](macos.md#build-and-open) after worker setup.

## 3. Open your project

Change to the project you want to work on, then launch:

Replace `/path/to/your/project` with your project directory.

```sh
cd /path/to/your/project
sprowt-harness
```

Use one harness instance per project. Empty folders, existing projects and Git repositories are supported.

- **No commits yet:** review and confirm the initial files before Git setup.
- **Existing repository:** the harness checks the current branch's upstream and safely fast-forwards before creating a worktree. Overlapping edits or divergent history stop setup.
- **Local uncommitted edits:** they stay in your project. A new codemod starts from committed source; commit changes first if they must be included.

## 4. Describe a small outcome

Open **Ctrl+P → New codemod**, or use the creation view shown on first launch. For example:

```text
Add a --version option to this CLI.
Use the existing package version and test the output.
Keep the default command behavior unchanged.
```

Press **Ctrl+J** for a newline and **Enter** to submit. The codemod appears as **New codemod** until the planner generates its short title. Your complete request stays in the conversation. See [codemods and messages](codemods.md#create-and-manage-work).

**Expected:** a numbered plan appears and execution starts automatically. The timeline follows tasks, combined checks and review. You do not need to choose worker counts or assign providers.

| To… | Use |
| --- | --- |
| See the current step and its evidence | **Ctrl+L** |
| Explore tasks, checks, review and activity | **Ctrl+T** |
| Read the plan's scopes and contracts | **Ctrl+O** |
| Pause or resume unfinished work | **Ctrl+R** |
| Find actions available now | **Ctrl+G** |

Workers may ask a question or request a download domain. Answer in the composer when it shows **Answer #ID**, or use **Ctrl+N** to inspect network access.

## 5. Check the result

After task checks pass, the harness combines the changes and reruns checks. A fresh reviewer then inspects that exact version.

- **Clean review:** inspect the diff with **Ctrl+D** and proceed to publication.
- **Confirmed findings:** choose **Fix review issues** with **Ctrl+G → x**. Repairs and fresh review run within a bounded budget.
- **Uncertain findings:** choose **Investigate findings** before making source changes.
- **Failed checks:** open **Ctrl+T → 2 · Checks** and follow the suggested action.

Read [Review and repairs](review.md) for closure rules and [Troubleshooting](troubleshooting.md) if work stops.

## 6. Publish the PR

Before your first publication, set up GitHub CLI in a separate terminal:

```sh
brew install gh
gh auth login
gh auth setup-git
```

When the dock says **Ready to publish**, press **Ctrl+S**. Review the generated commit title, PR title, summary and verification, then confirm publication. If needed, the harness offers to connect a repository or create a private one.

**Expected:** the PR URL is saved and the timeline shows publication. Checks and a clean current review are required for every new or updated PR. The harness does not merge PRs automatically.

## Keep working

Send an edit through the composer to start another pass and update the same open PR. While work is running, messages wait for the next pass; [steering](codemods.md#steer-an-active-turn) can deliver an instruction sooner.

To finish locally, use **Ctrl+P → c** and confirm **Close**. Source is checkpointed and the VM is removed. Reopening restores saved work; unfinished execution waits for you to resume.

**Next:** [Learn the terminal](terminal.md) · [Understand PR updates](git-workflow.md#publish-a-pr) · [Configure optional Jev routing](planning.md#configure-jev)
