[Docs](README.md) / Use the macOS app

# Run Sprowt on macOS

Use the native SwiftUI interface with the same Rust engine and saved work as the terminal app.

## Build and open

Install Xcode 26 or newer and Rust, then complete the [worker setup](quickstart.md#1-prepare-the-workers). The app uses the same supported Codex login, optional Muse account and Apple Container service as the CLI.

From the harness repository:

```sh
./macos/build.sh
open target/macos/Sprowt.app
```

**Expected:** Sprowt opens a native window with its yellow seedling icon. Saved projects appear immediately in the sidebar. Expand a project to choose a codemod, or choose a folder to add another project. The build includes the Rust executable inside the app, so the interface and engine come from the same checkout.

The matching terminal executable is `target/release/sprowt-harness`. To update a separately installed CLI, run `cargo install --path . --locked --force` from this checkout as well.

The bundle is signed locally for development. It is not a notarized distribution release.

## Work on a project

Choose a codemod in the sidebar to open **Conversation**. Your request appears once, followed by current messages and numbered plan tasks with outcomes and dependencies. **Saved history** contains earlier updates that are no longer in the feed.

The main area switches between **Conversation**, **Changes**, **Checks**, **Findings** and **Agents**. The composer and queue stay available across these views. **Agents** shows the agent count and each worker's state. The sidebar contains codemod navigation.

Select a plan task or an agent to inspect its current attempt, model, files and results. Use **Back to Conversation** or **Back to Agents** to return. Selecting a main tab shows that view for the whole codemod.

Expand **Files, checks and worker** for the file scope and individual required checks. File names stand out from muted directories; the full paths stay intact. Evidence uses a bounded, left-aligned reading width. **Source reference** reveals the complete saved identifier. Preparation stays aligned with the task details; pending network access is amber, and failures are red. A current preparation failure opens its evidence automatically. The same failure is shown once in task details, with the full saved output available under each check.

Messages distinguish headings, emphasis, lists, inline code and fenced code. Unsupported Markdown remains literal. Paths and saved command output use a monospace background; output preserves its original text and indentation.

[Long messages](codemods.md#read-long-messages) show compact prose and separate source disclosures. **Show full message** and **Show source** expand independently without changing the saved message or composer draft. The Rust snapshot supplies the same prose/code segments, stable message identities and JSON syntax tokens used by the CLI; the app keeps expansion state locally.

In **Changes**, choose the offered inspection action to read the actual diff. Keep typing, switch tabs or codemods, and choose **Done** when you finish the inspection. Starting a new codemod asks you to finish an inspection first, since it replaces the workspace. Opening evidence never starts a worker.

The header button shows Rust's recommended action; **More** holds the other available actions. A partial review with confirmed findings offers **Fix review issues**, while publication waits for a complete, clean review. See [review and repairs](review.md).

Click the compose icon beside **…** in a project header, or press **⌘N** for the current project, to describe the change in the compact creation editor. **Create codemod** or **⌘Return** starts the same planning and execution flow as the terminal app. The planner supplies a short title while the complete request stays in the conversation. Setup, network access, publication, closing and deletion retain their explicit confirmation steps.

![Native creation editor](assets/macos-sidebar-light.jpg)

Actual native project accordion and creation editor in a seeded disposable project session. No plan was started.

The creation editor shows headings, emphasis, lists, checklists and code as formatted text. Choose **Format**, or type `/` at the start of an empty line, to select a format. Use **↑**, **↓** and **Return**, or click a choice. **Esc** closes the menu and preserves its slash; continuing a path such as `/Users/...` closes the menu and preserves the path. **⌘B**, **⌘I** and **⌘E** toggle bold, italic and inline code. **⌘Z** and **⇧⌘Z** undo and redo edits.

**Dictate** starts macOS Dictation at the editor's caret. Enable it in **System Settings → Keyboard → Dictation** if needed. You can also choose **Edit → Start Dictation**. System controls show dictation feedback. Dictation only inserts text; choose **Create codemod** or **⌘Return** explicitly when the request is ready.

The editor sends Markdown through the existing Rust command. Supported formatting appears in the app; unsupported Markdown remains literal, with its source preserved. Code blocks contain literal text, so inline emphasis does not apply inside them. The full request is stored in both interfaces.

The left icon rail contains **Projects**, **Open project** and **Closed codemods**, with the local profile at the bottom. **⌘O** opens a project folder directly. Click a project name to expand or collapse its indented codemods; there is no disclosure caret. Only one project is expanded at a time. Folding a project preserves the workspace, editor and draft.

The **…** menu opens that project, switches between active and closed codemods, or removes it from the sidebar. [Removing a project](codemods.md#remove-a-project-from-the-sidebar) keeps its files and saved work. The adjacent compose icon creates a codemod in that project. Choosing another project's codemod switches the active project after pending drafts are saved. Agents and project operations continue in the background; returning keeps their existing sessions. A small dot marks background activity, and an attention icon marks a request or failure to inspect. Failed or locked destinations leave the current workspace intact. See [projects and codemods](codemods.md#create-and-manage-work).

**Back**, **Forward**, and the sidebar toggle appear directly on the title bar as plain icon controls.

Use the title bar's sidebar button or the sidebar command in **View** to hide or show the sidebar. **⌃⌘S** works while the sidebar is hidden. The icon rail remains visible. Your codemod selection and composer drafts stay in place.

Use the title bar's **Back** and **Forward** arrows, **View → Back / Forward**, or **⌘[** and **⌘]** to revisit codemods and unfinished creation. [Navigation history](codemods.md#create-and-manage-work) is retained separately for each project until you quit. Inspections do not block navigation; confirmations still do.

Press **⌘1** through **⌘5** for **Conversation**, **Changes**, **Checks**, **Findings** and **Agents**, respectively. These shortcuts preserve the composer draft.

Choose **System**, **Light** or **Dark** from **Appearance** in the bottom profile menu. The preference is shared with the terminal. See [Appearance](appearance.md) for controls and status colors.

Use the composer to answer a question or queue an instruction. Expand the attached queue to edit, reorder, remove or steer pending instructions. Save an edit before choosing **Steer**. These actions preserve the composer draft. Removing the last queued instruction closes the queue and returns focus to the composer. Steering without connected workers saves instructions as waiting for workers; it does not claim delivery.

The app is a macOS interface; workers still build and check source in Linux. See [execution boundaries](architecture.md#boundaries-today).

## Choose workers

Open **More → Workers**, or **Workers** beside the creation editor controls. Choose a model and effort for each role and counts for execution rows. **Save team** persists through the shared engine; Cancel preserves the saved values and composer. See [worker teams and quota recovery](workers.md#choose-a-worker-team).

## Find the same work in the terminal

Both interfaces use the same plans, task inspection, check results and action rules. Their layouts and shortcuts fit their host interface.

| In the macOS app | In the terminal |
| --- | --- |
| Conversation and task checklist | **1 · Conversation** in the [workspace](terminal.md#read-the-screen) |
| Task or worker inspection | Select a task and press Enter, or open **Ctrl+T · Details** |
| Changes, Checks, Findings and Agents | **2** through **5** while exploring |
| Attached queue | **Ctrl+Q · Queue** |
| Contextual actions | Action dock or **Ctrl+G · More** |
| Project row / compose icon / **…** | **Ctrl+P**, then **Enter / N / .** |
| Open project / **⌘O** | **Ctrl+P → +**, then enter the folder path |
| Back / Forward or **⌘[ / ⌘]** | Header arrows or **Alt+← / Alt+→** |
| Creation editor's Format menu | **F2 · Format** in the new codemod composer |
| Dictate | Host terminal's dictation or paste, when supported |

## Switch between interfaces

Close the current interface before opening the same project in the other. Both use the [same local state](local-state.md). A project lock prevents two controllers from running its workers at once.

Saved plans, messages, drafts and results remain available. Reopening unfinished work does not resume workers automatically; choose the offered resume action.

Live simultaneous control from both interfaces is not implemented. The shared Rust engine keeps workflow behavior in one place, while terminal and SwiftUI presentation remain separate.

## Develop the interface

Run the native tests from the harness repository:

```sh
swift test --package-path macos
```

At launch, the app calls `projects` on its bundled Rust executable to read the saved catalog and appearance without opening a workspace or starting workers. Selecting a project starts the desktop session.

The native app communicates with its bundled engine over local stdin/stdout. Typed commands carry a request ID; responses contain the protocol version, an acknowledgement or error, and the current snapshot. The snapshot includes the project catalog, active and expanded project IDs, per-project activity, creation draft, navigation destination, available directions and typed appearance preference. `remove_project` saves sidebar visibility without stopping its runtime. `toggle_project` changes only expansion; `open_project`, `select_project` and `begin_project_creation` use Rust's shared session to retain project runtimes and ownership locks. `set_creation_draft` saves the identified project's unsent description. Draft writes drain before project selection. `set_appearance` accepts `system`, `light` or `dark`; Rust acknowledges the new value only after saving it. Swift owns rendering and editing focus. Rust owns navigation history, scheduling, permissions, persistence and workflow transitions. The session polls every opened project; background dialogs remain with their project and never take over the visible editor. Quitting closes all owned runtimes.

`refresh_models` starts bounded asynchronous catalog discovery. `set_worker_settings` carries `{mod_id, settings}`; a null `mod_id` edits the current project creation defaults. Snapshots include typed `worker_settings` and `model_catalog`. Rust validates models, efforts, rows and total count before acknowledging a save. Swift keeps the editable draft until success and never changes active worker pins.

Native system typography and evergreen colors cover the workspace, editors, dialogs and window chrome. No font files or downloads are required.

Row icons align with the center of their label's first line, even when the label wraps.

For an isolated development session, replace both paths below with disposable directories:

```sh
open -n target/macos/Sprowt.app --args \
  --project /path/to/test-project \
  --data-dir /path/to/test-state
```

An alternate data directory separates harness records; it does not make project operations read-only or replace worker credentials. Use a disposable project when testing actions.

**Next:** [Codemods and messages](codemods.md) · [Development checks](development.md)
