import AppKit
import Foundation
import Observation

@MainActor
@Observable
final class Session {
    enum Connection { case disconnected, starting, ready, stopping }
    struct Pending {
        let request: Request
        let draftRevision: UInt64?
    }

    private(set) var snapshot: Snapshot?
    private(set) var connection = Connection.disconnected
    private(set) var error: String?
    var newDescription = ""
    var showingNew: Bool { snapshot?.navigation.destination == .newCodemod }
    var showClosed = false
    var workspace = WorkspaceSelection()
    private(set) var drafts = Drafts()
    private var transport: Sidecar?
    private var nextId: UInt64 = 0
    private var pending: [Pending] = []
    private var current: Pending?
    private var stderr = ""
    private var finishClosing: (() -> Void)?
    private var dataDirectory: String?
    private var project: URL?
    private var generation = UUID()
    private var creation: (description: String, previousMod: Int64?)?

    var active: CodemodDetail? { snapshot?.activeMod }
    var dialog: EngineDialog? { connection == .ready ? snapshot?.dialog : nil }
    var editable: Bool { connection == .ready }
    var interactive: Bool { editable && (dialog == nil || dialog?.isInspection == true) }
    var busy: Bool {
        current.map { $0.request.command.type != "set_draft" } == true ||
        pending.contains { $0.request.command.type != "set_draft" }
    }
    // Inspections stay open for everything except starting a new codemod;
    // creation replaces the whole workspace, so finish the inspection first.
    var canBeginCreation: Bool { interactive && !busy && dialog == nil }
    private var changingDestination: Bool {
        ([current].compactMap { $0 } + pending).contains {
            ["select_mod", "begin_creation", "go_back", "go_forward"].contains($0.request.command.type)
        }
    }
    var canCompose: Bool { interactive && !showingNew && active?.closed == false && !changingDestination }
    var canGoBack: Bool { interactive && !busy && snapshot?.navigation.canGoBack == true }
    var canGoForward: Bool { interactive && !busy && snapshot?.navigation.canGoForward == true }

    var draft: String { active.map { drafts.text(for: $0.id, fallback: $0.draft) } ?? "" }
    var visibleMods: [CodemodSummary] { snapshot?.mods.filter { $0.closed == showClosed } ?? [] }

    func start(project: URL, dataDirectory: String? = nil, executable: URL? = nil) {
        let generation = UUID()
        self.generation = generation
        self.project = project
        self.dataDirectory = dataDirectory
        connection = .starting
        error = nil
        stderr = ""
        let transport = Sidecar()
        self.transport = transport
        do {
            let stream = try transport.start(project: project, dataDirectory: dataDirectory, executable: executable)
            Task { [weak self] in
                for await event in stream {
                    guard self?.generation == generation else { break }
                    self?.receive(event)
                }
            }
        } catch {
            self.error = "Could not start the Rust engine: \(error.localizedDescription)"
            connection = .disconnected
            self.transport = nil
        }
    }

    func chooseProject() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Open project"
        if let project { panel.directoryURL = project }
        guard panel.runModal() == .OK, let folder = panel.url else { return }
        close { [weak self] in
            guard let self else { return }
            self.snapshot = nil
            self.drafts = Drafts()
            self.start(project: folder, dataDirectory: self.dataDirectory)
        }
    }

    func close(completion: @escaping () -> Void) {
        guard transport != nil, connection != .disconnected else { completion(); return }
        connection = .stopping
        finishClosing = completion
        sendNext()
    }

    func editDraft(_ text: String) {
        guard let id = active?.id, canCompose else { return }
        let revision = drafts.edit(text, for: id)
        enqueue(Command(type: "set_draft", modId: id, text: text), revision: revision)
    }

    func submit() {
        guard let id = active?.id, canCompose, !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        let text = draft
        let revision = drafts.edit("", for: id)
        enqueue(Command(type: "submit", modId: id, text: text), revision: revision)
    }

    func create() {
        guard interactive, !busy, !newDescription.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        creation = (newDescription, active?.id)
        enqueue(Command(type: "new_mod", description: newDescription))
    }

    func beginCreation() {
        guard canBeginCreation else { return }
        showClosed = false
        enqueue(Command(type: "begin_creation"))
    }

    func select(_ id: Int64) {
        guard interactive && !busy else { return }
        enqueue(Command(type: "select_mod", modId: id))
    }

    func navigate(back: Bool) {
        guard back ? canGoBack : canGoForward else { return }
        enqueue(Command(type: back ? "go_back" : "go_forward"))
    }

    func perform(_ action: EngineAction) {
        enqueue(Command(type: "action", modId: active?.id, action: action.id, taskId: action.taskId))
    }

    func enqueue(_ command: Command, revision: UInt64? = nil, beforePending: Bool = false) {
        guard editable else { return }
        nextId += 1
        let item = Pending(request: Request(id: nextId, command: command), draftRevision: revision)
        if beforePending { pending.insert(item, at: 0) }
        else if command.type == "set_draft", let last = pending.last,
           last.request.command.type == "set_draft", last.request.command.modId == command.modId {
            pending[pending.count - 1] = item
        } else { pending.append(item) }
        sendNext()
    }

    private func sendNext() {
        guard current == nil else { return }
        guard !pending.isEmpty else {
            if connection == .stopping { transport?.closeInput() }
            return
        }
        current = pending.removeFirst()
        do {
            guard let transport else { throw CocoaError(.fileWriteUnknown) }
            try transport.send(current!.request)
        }
        catch { receive(.failure(error.localizedDescription)) }
    }

    private func receive(_ event: TransportEvent) {
        switch event {
        case .stderr(let text): stderr = String((stderr + text).suffix(8192))
        case .failure(let message):
            error = message
            restoreInterruptedSubmission()
            pending.removeAll()
            current = nil
            connection = .stopping
            transport?.closeInput()
        case .ended(let status):
            restoreInterruptedSubmission()
            if connection != .stopping {
                error = stderr.trimmingCharacters(in: .whitespacesAndNewlines)
                if error?.isEmpty != false { error = "The Rust engine stopped (exit \(status)). Reopen the project to continue." }
            }
            connection = .disconnected
            transport = nil
            pending.removeAll()
            current = nil
            let completion = finishClosing
            finishClosing = nil
            completion?()
        case .line(let data):
            do { apply(try Envelope.decode(data)) }
            catch { receive(.failure("Could not read the Rust engine response: \(error.localizedDescription)")) }
        }
    }

    private func restoreInterruptedSubmission() {
        for item in ([current].compactMap { $0 } + pending).reversed() {
            guard item.request.command.type == "submit", let revision = item.draftRevision,
                  let id = item.request.command.modId, let text = item.request.command.text else { continue }
            _ = drafts.restoreSubmission(id: id, revision: revision, text: text)
        }
    }

    private func apply(_ response: Envelope) {
        if response.requestId == nil, let error = response.error {
            snapshot = response.snapshot
            receive(.failure(error))
            return
        }
        if connection == .starting { connection = .ready }
        var restored: Pending?
        var navigated = false
        if let request = current, response.requestId == request.request.id {
            navigated = response.error == nil && ["go_back", "go_forward", "select_mod"].contains(request.request.command.type)
            if response.error == nil, let revision = request.draftRevision, let id = request.request.command.modId {
                drafts.acknowledge(id: id, revision: revision)
            }
            if response.error != nil, request.request.command.type == "submit",
               let revision = request.draftRevision, let id = request.request.command.modId,
               let text = request.request.command.text,
               drafts.restoreSubmission(id: id, revision: revision, text: text) {
                restored = request
            }
            if request.request.command.type == "cancel" ||
               (request.request.command.type == "new_mod" && response.error != nil) { creation = nil }
            current = nil
            if request.request.command.type != "set_draft" || response.error != nil { error = response.error }
        } else if let message = response.error { error = message }
        snapshot = response.snapshot
        workspace.reconcile(response.snapshot)
        if navigated { showClosed = active?.closed == true && !showingNew }
        if let creation, let active, active.id != creation.previousMod, response.snapshot.dialog == nil {
            if newDescription == creation.description { newDescription = "" }
            self.creation = nil
        }
        if let active { drafts.receive(id: active.id, text: active.draft) }
        if let restored {
            enqueue(Command(type: "set_draft", modId: restored.request.command.modId, text: restored.request.command.text),
                    revision: restored.draftRevision, beforePending: true)
        }
        sendNext()
    }
}
