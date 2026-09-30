# Local Linux sandbox

Each executing code mod gets its own Apple Container VM. Codex chooses and installs the project’s runtime there. Your login stays on the Mac.

## Commands and checks

Read from top to bottom. Solid arrows send work; dashed arrows return results.

```mermaid
sequenceDiagram
    box Your Mac · login stays here
        participant Harness as Sprowt harness
        participant Agent as Codex agent<br/>(app-server)
    end
    box Linux VM · one per mod
        participant Tools as VM tool runner<br/>(Codex exec-server)
    end
    Harness->>Agent: Send the next task
    Agent->>Tools: Prepare runtime, edit and test
    Tools-->>Agent: Tool results
    Agent-->>Harness: Task summary and check commands
    Harness->>Tools: Rerun the checks independently
    Tools-->>Harness: Check results
```

Both VM connections use standard input/output. The harness reruns checks in the same VM that Codex used.

## Source files

Files move in one direction: copy, work, review, apply. The VM gets a copy of your source; the original project stays on the Mac.

```mermaid
flowchart TB
    source["1. Your project<br/>Mac · original source"]
    workspace["2. /workspace<br/>Linux VM · code and dependencies"]
    review["3. Diff and check results<br/>Mac · review with Ctrl+D"]
    applied["4. Your project<br/>Mac · reviewed changes applied"]
    source -->|"Copy source into the VM"| workspace
    workspace -->|"Export source only"| review
    review -->|"Checks pass and you confirm apply"| applied
```

## Use it

You need Apple silicon, macOS 26+, Apple Container and Codex CLI **0.159.2**. Sign in with a file-backed ChatGPT login and start the container service:

```sh
brew install container
container system start --enable-kernel-install
codex -c 'cli_auth_credentials_store="file"' login
```

Create a mod, review its plan and press **Ctrl+R**. The first execution builds the shared development image; later runs reuse it. The terminal shows setup progress. **Ctrl+D** keeps the same diff and apply workflow.

The image contains general build tools, Bubblewrap, Codex and mise. No project language is selected in advance. Codex reads your manifests, installs a compatible runtime under `/home/sprowt`, then installs project dependencies under `/workspace`. System files stay read-only; extra OS packages require changing the image.

## The boundary

- Commands, edits and independent checks use the same VM. A disconnected guest stops work; it never falls back to host execution.
- No host folders, login files, SSH agent, service sockets or ports are mounted or forwarded.
- Codex’s guest sandbox keeps system tools read-only and forces network traffic through its domain proxy. Direct connections and private network destinations are blocked. Guest loopback is available for local app checks.
- Source exports preserve regular files, modes and deletions. Dependency folders and common credential files are excluded. Links and special files are rejected. Exports are limited to 512 MiB, with 64 MiB per source file.

The planner still inspects the project through a read-only host OS sandbox. Host MCPs, apps, plugins and hooks stay disabled. VM execution currently needs a file-backed ChatGPT login; Keychain-only login is not supported by this connection.

## Downloads

```mermaid
flowchart TB
    command["Command in the Linux VM"]
    proxy["Codex network proxy<br/>Uses the Mac's domain allowlist"]
    packages["Public runtime and package sources"]
    command -->|"Request a download"| proxy
    proxy -->|"Allowed domain"| packages
```

Other domains, direct connections and private network destinations are blocked.

The default allowlist covers common Python, Node, Rust, Go and other package sources, plus Playwright browser downloads. It allows `cdn.playwright.dev`, its Microsoft mirror and redirects to `storage.googleapis.com`. Browser system libraries still need to be available in the image.

Edit the host-only file to add required domains:

```text
~/Library/Application Support/sprowt-harness/network.json
```

It is a JSON list of hostnames; `*.example.org` allows that domain’s subdomains. Existing local policies are kept when upgrading; new default domains must also be added to that file. Quit and reopen the harness, then press **Ctrl+R** to reconnect with the updated policy. A model cannot edit this host policy. Allowed services can receive data; domain filtering is not a guarantee against data leaving the VM.

## VM lifecycle

| Mod state | VM |
| --- | --- |
| Running | Running; reused across tasks |
| Paused or awaiting review | Retained with its runtime and dependencies |
| Successfully applied | Deleted; history and source exports stay on the Mac |
| Mod deleted | Deleted along with its host workspace and history |

Quitting stops VMs and retains unfinished mods’ disks. Reopening and **Ctrl+R** reconnect without losing dependencies. Reconnecting clears guest processes left by a crash. Shared images and the container service remain for reuse.

VM deletion happens after files are applied and the applied state is saved. If cleanup fails, the project keeps the changes. The VM marker stays until deletion succeeds; **Ctrl+R** retries cleanup, including after reopening. A failed or cancelled apply keeps the VM.

Unfinished mods created before VM support keep their working files. Their first explicit run starts a fresh executor conversation and reruns tasks in Linux; old host check results are cleared. Applied mods remain applied. A missing or altered VM blocks reconnection and preserves the last exported source for review.

This backend runs Linux. iOS and macOS builds need a later macOS VM backend. One executor per mod; different mods can run in parallel. Uses experimental [Codex executor interfaces](https://github.com/openai/codex/tree/rust-v0.159.2/codex-rs/exec-server) and [Codex managed networking](https://learn.chatgpt.com/docs/permissions).
