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

    var workerSettingsDraft: WorkerSettingsDraft?
    private(set) var snapshot: Snapshot?
    private(set) var catalog: ProjectCatalog?
    private var catalogExpandedProject: Int64?
    private var loadingCatalog = false
    private var executable: URL?
    private var initialCommand: Command?
    private(set) var connection = Connection.disconnected
    private(set) var error: String?
    private var creationDrafts = Drafts()
    var newDescription: String {
        get { snapshot.map { creationDrafts.text(for: $0.projectId, fallback: $0.creationDraft) } ?? "" }
        set {
            guard let id = snapshot?.projectId else { return }
            let revision = creationDrafts.edit(newValue, for: id)
            enqueue(Command(type: "set_creation_draft", text: newValue, projectId: id), revision: revision)
        }
    }
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
    var projects: [ProjectSummary] { snapshot?.projects ?? catalog?.projects ?? [] }
    var expandedProject: Int64? { snapshot.map { $0.expandedProject } ?? catalogExpandedProject }
    var hasUnlistedInspection: Bool {
        guard let dialog, dialog.isInspection else { return false }
        return snapshot?.workspaceViews.contains(dialog.workspaceTab) == false
    }
    var appearance: Appearance { snapshot?.appearance ?? catalog?.appearance ?? .system }
    var canBrowseProjects: Bool { !busy && (interactive || (snapshot == nil && catalog != nil && connection == .disconnected)) }
    var interactive: Bool { editable && (dialog == nil || dialog?.isInspection == true) }
    var busy: Bool {
        current.map { !["set_draft", "set_creation_draft"].contains($0.request.command.type) } == true ||
        pending.contains { !["set_draft", "set_creation_draft"].contains($0.request.command.type) }
    }
    // Inspections stay open for everything except starting a new codemod;
    // creation replaces the whole workspace, so finish the inspection first.
    var canBeginCreation: Bool { interactive && !busy && dialog == nil }
    private var changingDestination: Bool {
        ([current].compactMap { $0 } + pending).contains {
            ["select_mod", "begin_creation", "go_back", "go_forward", "select_project", "open_project", "begin_project_creation"].contains($0.request.command.type)
        }
    }
    var canCompose: Bool { interactive && !showingNew && active?.closed == false && !changingDestination }
    var canGoBack: Bool { interactive && !busy && snapshot?.navigation.canGoBack == true }
    var canGoForward: Bool { interactive && !busy && snapshot?.navigation.canGoForward == true }

    var draft: String { active.map { drafts.text(for: $0.id, fallback: $0.draft) } ?? "" }
    var visibleMods: [CodemodSummary] { snapshot?.mods.filter { $0.closed == showClosed } ?? [] }

    func loadCatalog(dataDirectory: String? = nil, removing project: Int64? = nil, executable: URL? = nil) {
        guard connection == .disconnected else { return }
        self.dataDirectory = dataDirectory
        self.executable = executable
        loadingCatalog = true
        connection = .starting
        error = nil
        stderr = ""
        let generation = UUID()
        self.generation = generation
        let transport = Sidecar()
        self.transport = transport
        do {
            let stream = try transport.catalog(dataDirectory: dataDirectory, removing: project, executable: executable)
            Task { [weak self] in
                for await event in stream {
                    guard self?.generation == generation else { break }
                    self?.receive(event)
                }
            }
        } catch {
            self.error = "Could not load saved projects: \(error.localizedDescription)"
            loadingCatalog = false
            connection = .disconnected
            self.transport = nil
        }
    }

    func start(project: URL, dataDirectory: String? = nil, executable: URL? = nil, initialCommand: Command? = nil) {
        let generation = UUID()
        self.generation = generation
        self.project = project
        self.dataDirectory = dataDirectory
        self.executable = executable
        self.initialCommand = initialCommand
        loadingCatalog = false
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
        if editable { openProject(folder) }
        else if connection == .disconnected { start(project: folder, dataDirectory: dataDirectory, executable: executable) }
    }

    func openProject(_ folder: URL) {
        guard interactive && !busy else { return }
        enqueue(Command(type: "open_project", path: folder.path))
    }

    func toggleProject(_ id: Int64) {
        guard canBrowseProjects else { return }
        if editable { enqueue(Command(type: "toggle_project", projectId: id)) }
        else { catalogExpandedProject = catalogExpandedProject == id ? nil : id }
    }

    func selectProject(_ id: Int64) {
        guard canBrowseProjects else { return }
        if editable { enqueue(Command(type: "select_project", projectId: id)) }
        else { openSavedProject(id) }
    }

    func removeProject(_ id: Int64) {
        guard canBrowseProjects else { return }
        if editable { enqueue(Command(type: "remove_project", projectId: id)) }
        else { loadCatalog(dataDirectory: dataDirectory, removing: id, executable: executable) }
    }

    private func openSavedProject(_ id: Int64, command: Command? = nil) {
        guard let project = projects.first(where: { $0.id == id }) else { return }
        start(project: URL(fileURLWithPath: project.path), dataDirectory: dataDirectory, executable: executable, initialCommand: command)
    }

    func beginCreation(in project: Int64) {
        guard canBrowseProjects, dialog == nil else { return }
        showClosed = false
        if editable { enqueue(Command(type: "begin_project_creation", projectId: project)) }
        else { openSavedProject(project, command: Command(type: "begin_creation")) }
    }

    func close(completion: @escaping () -> Void) {
        guard transport != nil, connection != .disconnected else { completion(); return }
        connection = .stopping
        finishClosing = completion
        sendNext()
    }

    func openWorkerSettings() {
        guard interactive && !busy, let snapshot else { return }
        workerSettingsDraft = WorkerSettingsDraft(modId: showingNew ? nil : active?.id, settings: snapshot.workerSettings)
        enqueue(Command(type: "refresh_models"))
    }

    func saveWorkerSettings(_ settings: WorkerSettings, modId: Int64?) {
        enqueue(Command(type: "set_worker_settings", modId: modId, settings: settings))
    }

    func setAppearance(_ appearance: Appearance) {
        enqueue(Command(type: "set_appearance", appearance: appearance))
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
        guard canBrowseProjects else { return }
        if editable { enqueue(Command(type: "select_mod", modId: id)) }
        else if let project = projects.first(where: { $0.mods.contains(where: { $0.id == id }) }) {
            openSavedProject(project.id, command: Command(type: "select_mod", modId: id))
        }
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
        else if ["set_draft", "set_creation_draft"].contains(command.type), let last = pending.last,
           last.request.command.type == command.type, last.request.command.modId == command.modId,
           last.request.command.projectId == command.projectId {
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
            if connection != .stopping && !(loadingCatalog && status == 0 && catalog != nil) {
                error = stderr.trimmingCharacters(in: .whitespacesAndNewlines)
                if error?.isEmpty != false { error = loadingCatalog ? "Could not load saved projects (exit \(status))." : "The Rust engine stopped (exit \(status)). Reopen the project to continue." }
            }
            loadingCatalog = false
            initialCommand = nil
            connection = .disconnected
            transport = nil
            pending.removeAll()
            current = nil
            let completion = finishClosing
            finishClosing = nil
            completion?()
        case .line(let data):
            do {
                if loadingCatalog {
                    catalog = try ProjectCatalog.decode(data)
                    if !projects.contains(where: { $0.id == catalogExpandedProject }) { catalogExpandedProject = nil }
                } else { apply(try Envelope.decode(data)) }
            }
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
            if request.request.command.type == "set_worker_settings", response.error == nil { workerSettingsDraft = nil }
            navigated = response.error == nil && ["go_back", "go_forward", "select_mod", "select_project", "open_project", "begin_project_creation"].contains(request.request.command.type)
            if response.error == nil, let revision = request.draftRevision {
                if let id = request.request.command.modId { drafts.acknowledge(id: id, revision: revision) }
                if let id = request.request.command.projectId { creationDrafts.acknowledge(id: id, revision: revision) }
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
            if !["set_draft", "set_creation_draft"].contains(request.request.command.type) || response.error != nil { error = response.error }
        } else if let message = response.error { error = message }
        snapshot = response.snapshot
        project = URL(fileURLWithPath: response.snapshot.project)
        creationDrafts.receive(id: response.snapshot.projectId, text: response.snapshot.creationDraft)
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
        if let command = initialCommand {
            initialCommand = nil
            enqueue(command)
        }
        sendNext()
    }
}
