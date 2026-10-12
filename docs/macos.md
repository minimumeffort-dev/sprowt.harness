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

**Expected:** Sprowt opens a native window. Choose a project folder to load its codemods. The build includes the Rust executable inside the app, so the interface and engine come from the same checkout.

The matching terminal executable is `target/release/sprowt-harness`. To update a separately installed CLI, run `cargo install --path . --locked --force` from this checkout as well.

The bundle is signed locally for development. It is not a notarized distribution release.

## Work on a project

Choose a codemod in the sidebar to read its conversation, plan, checks and findings. The available actions come from Rust and change with the current workflow state. Starting a new codemod starts the same planning and execution flow as the terminal app.

Use the composer to answer a question or queue an instruction. Queue edits, reordering and steering preserve the composer draft. Setup, network access, publication, closing and deletion retain their explicit confirmation steps.

The app is a macOS interface; workers still build and check source in Linux. See [execution boundaries](architecture.md#boundaries-today).

## Switch between interfaces

Close the current interface before opening the same project in the other. Both use the [same local state](local-state.md). A project lock prevents two controllers from running its workers at once.

Saved plans, messages, drafts and results remain available. Reopening unfinished work does not resume workers automatically; choose the offered resume action.

Live simultaneous control from both interfaces is not implemented. The shared Rust engine keeps workflow behavior in one place, while terminal and SwiftUI presentation remain separate.

## Develop the interface

Run the native tests from the harness repository:

```sh
swift test --package-path macos
```

The native app communicates with its bundled engine over local stdin/stdout. Typed commands carry a request ID; responses contain the protocol version, an acknowledgement or error, and the current snapshot. Swift owns rendering and editing focus. Rust owns scheduling, permissions, persistence and workflow transitions.

For an isolated development session, replace both paths below with disposable directories:

```sh
open -n target/macos/Sprowt.app --args \
  --project /path/to/test-project \
  --data-dir /path/to/test-state
```

An alternate data directory separates harness records; it does not make project operations read-only or replace worker credentials. Use a disposable project when testing actions.

**Next:** [Codemods and messages](codemods.md) · [Development checks](development.md)
