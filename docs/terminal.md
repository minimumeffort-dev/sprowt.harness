[Docs](README.md) / Use the harness

# Terminal guide

Use the timeline to follow work, Details to read the evidence, and the dock to act. Your draft stays intact while you explore.

[Navigation](#navigate-the-timeline) · [Details](#details) · [Next action](#understand-the-dock) · [Keyboard reference](#keyboard-reference) · [CLI](#commands)

## Read the screen

![Illustrated timeline, details panel and unified message dock.](assets/terminal-overview.svg)

*Layout illustration, with example tasks and results.*

| Area | Purpose |
| --- | --- |
| **Timeline** | Request, plan, tasks, combined checks, review and publication |
| **Details** | Evidence for the selected step, including failures and review findings |
| **Dock** | Current state, the next useful action and your message |

Wide terminals show timeline and details side by side. Smaller terminals use the full width for the timeline; Enter opens the selected step's details.

## Navigate the timeline

1. Press **Tab** to focus the timeline. A **▸** beside a heading marks the focused pane.
2. Use **↑/↓** to select a row and **Space** to expand it.
3. Press **Enter** to inspect. In a wide terminal, **→** focuses the side inspector and **←** returns.
4. Press **Esc** to return to your draft.

Clicking a row selects it. Enter on a timeline row opens evidence; it never starts a repair or publishes.

### Scroll and return to current work

Use the wheel or trackpad over the pane you want to scroll. The timeline and inspector scroll independently. Lists move selection; the composer scrolls its own draft. Keyboard scrolling uses **Page Up/Down** or **Fn+↑/↓** on Mac.

Exploring holds your position as new activity arrives, even when the selected task moves off screen. **Ctrl+L**, or clicking **Jump to current**, focuses the current step, resets its details to the top and resumes following. Scrolling again holds your position. **End** also jumps when the timeline is focused.

### Read task progress

Finished stages collapse. Active tasks show their worker, model, effort and latest update. Waiting tasks show what they depend on. A repair can have a separate **Previous failure** row containing its saved evidence.

**Latest update** belongs to the current attempt. Earlier narration remains in Activity. Worker elapsed time measures the current connected turn; last activity measures observed events. Neither is a completion estimate or proof of progress.

## Details

Press **Ctrl+T**. Use **1–4**, **Tab** or **Shift+Tab** to switch sections.

| Section | What to look for |
| --- | --- |
| **1 · Tasks** | Status, owner, model and dependencies; Enter opens one task |
| **2 · Checks** | Failed checks first, then passing groups and commands not run |
| **3 · Review** | Finding IDs, severity, owner, evidence, proposed fix and decision history |
| **4 · Activity** | Time by phase, failed attempts, worker updates and handoffs |

Details opens Review when findings need attention, Checks after a combined failure, and otherwise Tasks. Before tasks exist, it opens Activity. Opening evidence does not start work.

Check colors have matching glyphs: **✓ passed**, **! failed**, **· not run**. Repeated check names group their commands; a check passes only when all its commands pass. **e** expands command/output evidence, including elapsed time and time limits. Worker reports remain distinct from controller results.

Saved results are labelled when the source has changed. **Earlier task report · before combined checks** means a task's earlier success predates the final combined result.

Activity also retains **observed worker time** by phase across restarts and replacement plans. Concurrent workers count separately. Offline gaps are excluded; older runs have no retroactive timing. This is effort, not an ETA. Repeated messages do not establish verified progress.

Failed tasks show the likely failure category and a corresponding recovery explanation. Unknown check failures remain unclassified until diagnosed. Three attempts with identical saved failure evidence recommend **Revise repair plan**; the attempt history is included in the new plan. Passing checks and fresh review are still required.

### Inspect a task

Select it in Tasks and press Enter. You can read its latest update, checks, dependencies, unanswered questions, network requests and recovery context.

| Key | Action |
| --- | --- |
| `r` in Tasks | Retry the selected failed task |
| Ctrl+R in task details | Retry that task |
| `h` | Open this task's activity |
| `a` | Open all activity |
| `e` | Expand evidence |

A targeted retry preserves completed peers and waits if both worker slots are occupied. Older messages without a saved task identity remain in all activity.

### Inspect the plan

**Ctrl+O** opens contracts, assumptions, task outcomes, dependencies, scopes, coordination topics, checks and routing. Press it again to return to the timeline. Plans use numbered tasks; the numbering follows display order.

## Understand the dock

The first row names the current state. Supporting rows distinguish implementation, checks, review and publication. When you need to act, one recommended action appears beside the state.

| You see | Next step |
| --- | --- |
| Working or recovering | Follow progress; no action is needed |
| Answer needed | Reply when the composer shows **Answer #ID** |
| Network access needed | **Ctrl+N**, inspect the domains, then allow or deny |
| Task needs attention | Inspect its evidence, then retry that task |
| Combined checks need attention | **Fix failed check**, or rerun saved checks from More |
| Confirmed review findings | **Fix review issues** |
| Uncertain review evidence | **Investigate findings** |
| Repairs stalled or budget exhausted | **Revise repair plan** |
| Review interrupted or outdated | Start or retry review when checks are current |
| Ready to publish | Inspect the diff, then **Publish PR** |
| PR open · update pending | Finish checks and review, then **Update PR** |
| Paused by you | Resume when ready |
| Closed | Reopen saved work |

Successful combined checks normally start review automatically in an active session. Reopening or pausing leaves unfinished work idle. The exact [review policy](review.md) determines whether another repair pass can continue.

Background GitHub sync failures have a separate warning. **Ctrl+U** retries sync; **Ctrl+G → s** shows its full error. This warning does not replace the local task or review state.

### Write a message

The composer starts with four rows and grows to eight, then scrolls. Wrapped text and blank lines count. On short terminals it shrinks to keep controls visible.

**Enter** performs the action named beside the composer: create, answer, request edits or queue for the next pass. It never activates the dock's recommendation. **Ctrl+J** inserts a newline. See [messages and steering](codemods.md#send-an-instruction).

## Keyboard reference

Shortcuts are contextual. **Ctrl+G · More** lists actions available in the current state, grouped into Work, Inspect, Codemod and Remove.

| Key | Action |
| --- | --- |
| Tab / Shift+Tab | Cycle composer, timeline and side inspector |
| ↑↓ / Space / Enter | Select / expand / inspect in the timeline |
| Ctrl+L | Jump to current from any of those three panes |
| Home / End | Top / bottom of inspector; End jumps to current in timeline |
| Ctrl+J | Newline in composer |
| Ctrl+T | Open Details / return to conversation |
| Ctrl+O | Toggle plan details |
| Ctrl+G | More actions |
| Ctrl+R | Pause, resume or retry relevant work |
| Ctrl+S | Review and confirm PR publication or update |
| Ctrl+E | Start or retry independent review |
| Ctrl+D | View diff when available |
| Ctrl+Q | Manage pending instructions |
| Ctrl+N | Decide a pending network request |
| Ctrl+P | Switch or manage codemods |
| Ctrl+U | Sync project and check target updates |
| Page Up / Down | Scroll the focused view |
| Esc | Back; from the composer, quit |
| Ctrl+C | Quit |

**Ctrl+G → x** means press Ctrl+G, release it, then press `x` in More.

| Letter in More | Action, when available |
| --- | --- |
| `x` | Fix a failed check or review issues |
| `v` | Revise a stalled repair plan |
| `r` | Retry the failed task |
| `i` / `f` / `s` | Review findings / failure / sync details |
| `n` / `c` / `d` | New / close / delete codemod |

Letters type normally in the composer. Close, delete and publication keep their confirmations. The [queue](codemods.md#manage-the-queue) has its own shortcuts.

## Commands

| Command | Result |
| --- | --- |
| `sprowt-harness` | Open the current project |
| `sprowt-harness setup` | Configure optional host-only Jev routing |
| `sprowt-harness --no-motion` | Disable animation |
| `sprowt-harness --closed-worktree-days 0` | Keep closed worktrees indefinitely; default is 30 days |
| `sprowt-harness --help` | Show CLI help |
| `sprowt-harness --version` | Show the harness version |

## Layout and companion

The side inspector appears at 124 columns or more, with at least 12 rows available above the dock. The timeline receives 45% of that width. Resizing preserves selection and draft, clamps scroll positions and returns focus to the timeline if the inspector disappears.

Messages stay left aligned, with one blank row between them. User messages use `>` and a subtle background; agent messages identify provider and role. Menus and confirmations use a consistent compact layout. Empty lists are labelled explicitly.

The Sprowt companion uses an 8-column, 4-row area of terminal cells. It is curious while idle, thoughtful while planning, focused during work, happy when ready and still when blocked. Its silhouette stays fixed. `--no-motion` also disables entrance animation.

On exit, normal input, mouse, paste handling and the previous terminal screen are restored.

**Next:** [Manage codemods](codemods.md) · [Read review findings](review.md) · [Troubleshoot a failure](troubleshooting.md)
