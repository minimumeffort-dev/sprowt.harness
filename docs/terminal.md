[Docs](README.md) / Use the harness

# Terminal guide

The Evidence Workbench puts stages on the left and their evidence on the right. The dock keeps your draft and next action together.

[Navigation](#navigate-the-timeline) · [Details](#details) · [Next action](#understand-the-dock) · [Keyboard reference](#keyboard-reference) · [CLI](#commands)

## Read the screen

![Evidence Workbench with a compact stage rail, evidence pane and message dock.](assets/terminal-overview.svg)

*Rendered terminal layout with sample tasks and results.*

| Area | Purpose |
| --- | --- |
| **Stage rail** | Request, plan, environment, implementation, checks, review and publication |
| **Evidence pane** | Summary, checks, changes and activity for the selected step |
| **Dock** | Current state, the next useful action and your message |

Wide terminals show stages and evidence side by side. Smaller terminals show one pane at a time: select a stage, then press **Enter** to read it. **← Stages** returns to the list.

Environment appears when the plan includes a preparation recipe. Its summary lists setup and checks; task details contain the observed preparation receipts.

The header's **◇** selector opens codemods with **Ctrl+P**. The planner supplies each new codemod's short title; its complete request stays in the conversation.

The header shows **‹ Back Alt←** and **› Forward Alt→** when a destination is available. Click an arrow or use its shortcut to revisit a codemod or unsent creation. At narrow widths, the controls show the glyph and shortcut. [Navigation history](codemods.md#create-and-manage-work) preserves both drafts and resets when you open a project. Finish an open dialog or inspection before navigating.

The header's local-save status is separate from checks, review and GitHub publication. **Work saved locally** does not mean the code has passed checks or reached GitHub.

## Read the evidence

Select a stage, then click a tab or press **1–4** while exploring.

| Tab | Shows |
| --- | --- |
| **1 · Summary** | The selected stage's outcome and next action; review findings with evidence and proposed fixes |
| **2 · Checks** | Saved setup failures and observed task results, with failed checks first |
| **3 · Changes** | Access to the current diff when available |
| **4 · Activity** | Saved updates for a selected task, or codemod history for a stage |

Use **[ / ]** to move between review findings. **e** expands command evidence. To read the complete workflow history, choose **Timeline overview** or press **v** while exploring.

Letters and numbers type normally when the composer is focused.

## Navigate the timeline

1. Press **Tab** to focus the stage rail. A **▸** beside a heading marks the focused pane.
2. Use **↑/↓** to select a row and **Space** to expand it.
3. Press **Enter** to inspect. In a wide terminal, **→** focuses the side inspector and **←** returns.
4. Press **Esc** to return to your draft.

Clicking a row selects it. Enter on a stage opens evidence; it never starts a repair or publishes.

### Scroll and return to current work

Use the wheel or trackpad over the pane you want to scroll. The timeline and inspector scroll independently. Lists move selection; the composer scrolls its own draft. Keyboard scrolling uses **Page Up/Down** or **Fn+↑/↓** on Mac.

Exploring holds your position as new activity arrives, even when the selected task moves off screen.

```text
Following current work ── scroll or select ──> Exploring saved steps
Following current work <───── Ctrl+L ──────── Exploring saved steps
```

**Ctrl+L**, or clicking **Current**, selects the current step, resets its details to the top and resumes following. **End** also jumps when the timeline is focused. Your draft stays intact.

### Read task progress

The stage rail stays compact. Expand Implementation with **Space** to inspect individual tasks, or use **Timeline overview**. Task evidence shows the worker, model, latest update and dependencies. A repair can have a separate **Previous failure** row containing its saved evidence.

Keep a repair selected as it finishes: its evidence changes to the completed result while your selected tab, focus and draft stay in place.

**Latest update** belongs to the current attempt. Earlier narration remains in Activity. Worker elapsed time measures the current connected turn; last activity measures observed events. Neither is a completion estimate or proof of progress.

## Details

Choose **Inspect** or press **Ctrl+T** to open full Details. These sections include all tasks and results, beyond the selected workbench stage. Use **1–4**, **Tab** or **Shift+Tab** to switch sections.

| Section | What to look for |
| --- | --- |
| **1 · Tasks** | Status, owner, model and dependencies; Enter opens one task |
| **2 · Checks** | Failed checks first, then passing groups and commands not run |
| **3 · Review** | Finding IDs, severity, owner, evidence, proposed fix and decision history |
| **4 · Activity** | Time by phase, failed attempts, worker updates and handoffs |

Details opens Review when findings need attention, Checks after a combined failure, and otherwise Tasks. Before tasks exist, it opens Activity. Opening evidence does not start work.

| Evidence label | Read it as |
| --- | --- |
| **✓ passed** / **! failed** / **· not run** | Observed command result; a grouped check passes only when every command passes |
| **Earlier task report · before combined checks** | The worker's success predates the combined result |
| **Saved results** after a source change | Historical evidence; the current version still needs verification |
| **Observed worker time** | Time by phase across restarts/plans; concurrent workers count separately and offline gaps are excluded |

Press **e** to expand commands, full preparation output, elapsed time and limits. Saved setup failures remain visible in the dock and Checks after restart. Choose **Inspect failure** to open the affected task. Worker narration and elapsed time do not prove progress.

Failed tasks include recovery guidance when the cause is known. Three attempts with identical saved failure evidence recommend **Revise repair plan**, carrying that history forward. New checks and review are still required.

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

The first row names the current state. Supporting rows distinguish implementation, checks, review and publication. When you need to act, one recommended button appears beside the state. Click it or use its displayed shortcut.

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

When creating a codemod, **F2 · Format** opens Markdown helpers for heading, bold, italic, bulleted list, checklist, inline code and code block. A `/` on an empty line opens the same menu. **↑**, **↓** and **Enter** choose a format; **Esc** cancels and leaves the slash intact. Typing a path such as `/Users/...` closes the menu without changing the path. **Ctrl+Z** undoes creation edits; **Ctrl+Shift+Z** or **Ctrl+R** redoes them.

Use the terminal application's system dictation if it supports it, or paste a dictated request. Multiline paste inserts the whole request and preserves the draft; it never acts as Enter. Sprowt does not capture microphone audio in the terminal.

**Enter** performs the action named beside the composer: create, answer, request edits or queue for the next pass. It never activates the dock's recommendation. **Ctrl+J** inserts a newline. See [messages and steering](codemods.md#send-an-instruction).

The submit and newline controls also work by click. **Inspect** opens full Details; **More** opens all available actions. Your draft stays intact while navigating.

## Keyboard reference

Shortcuts are contextual. **Ctrl+G · More** lists actions available in the current state, grouped into Work, Inspect, Codemod and Remove.

| Key | Action |
| --- | --- |
| Tab / Shift+Tab | Cycle composer, timeline and side inspector |
| ↑↓ / Space / Enter | Select / expand / inspect in the timeline |
| 1–4 while exploring | Summary / Checks / Changes / Activity |
| v while exploring | Toggle Timeline overview |
| [ / ] in review evidence | Previous / next finding |
| e while exploring | Expand command evidence |
| Ctrl+L | Jump to current from any of those three panes |
| Home / End | Top / bottom of inspector; End jumps to current in timeline |
| Ctrl+J | Newline in composer |
| F2 | Format menu in new codemod composer |
| Ctrl+Z | Undo in new codemod composer |
| Ctrl+Shift+Z / Ctrl+R | Redo in new codemod composer |
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
| Alt+← / Alt+→ | Back / forward through codemods and creation |
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

The workbench splits when the content area has at least 100 columns and 12 rows. The stage rail uses 28% of the width, bounded to 25–43 cells; evidence gets the remaining space. Smaller terminals show the focused pane at full width. Resizing preserves selection and draft and clamps scroll positions.

Headings, spacing, contrast and padded buttons improve readability within the terminal grid. To enlarge the font itself, use your terminal app's font or zoom settings.

Messages stay left aligned, with one blank row between them. User messages use `>` and a subtle background; agent messages identify provider and role. Menus and confirmations use a consistent compact layout. Empty lists are labelled explicitly.

The Sprowt companion uses an 8-column, 4-row area of terminal cells. It is curious while idle, thoughtful while planning, focused during work, happy when ready and still when blocked. Its silhouette stays fixed. `--no-motion` also disables entrance animation.

On exit, normal input, mouse, paste handling and the previous terminal screen are restored.

The [native macOS workspace](macos.md#find-the-same-work-in-the-terminal) exposes the same task evidence, checks, changes, queue and contextual actions. Close the terminal interface before opening the same project in the app.

**Next:** [Manage codemods](codemods.md) · [Read review findings](review.md) · [Troubleshoot a failure](troubleshooting.md)
