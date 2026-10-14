import Foundation

struct WorkspaceSelection: Equatable {
    struct Context: Equatable { let project: String; let modId: Int64 }
    enum Target: Hashable { case task(String), worker(Int64) }
    enum Queue: Equatable { case closed, open }

    private(set) var context: Context?
    private var inspectionId: String?
    private(set) var tab = WorkspaceTab.conversation
    private(set) var target: Target?
    var queue = Queue.closed
    var messageToggles: Set<String> = []
    var agentDetails: Set<String> = []

    mutating func show(_ tab: WorkspaceTab, target: Target? = nil) {
        self.tab = tab
        self.target = target
    }
    mutating func toggleQueue() { queue = queue == .closed ? .open : .closed }

    func ownsAgentError(_ error: String, in snapshot: Snapshot) -> Bool {
        guard tab == .agents, snapshot.dialog?.isInspection != true else { return false }
        let worker: Int64? = if case .worker(let id) = target { id } else { nil }
        return snapshot.activeMod?.agents.owns(error, worker: worker) == true
    }

    mutating func reconcile(_ snapshot: Snapshot) {
        let next = snapshot.activeMod.map { Context(project: snapshot.project, modId: $0.id) }
        if context != next {
            self = WorkspaceSelection()
            context = next
        }
        guard let mod = snapshot.activeMod else { return }
        switch target {
        case .task(let id) where mod.plan?.tasks.contains(where: { $0.id == id }) != true:
            target = nil
        case .worker(let id) where !mod.workerDetails.contains(where: { $0.id == id }):
            target = nil
        default: break
        }
        if mod.queue.isEmpty { queue = .closed }
        if let dialog = snapshot.dialog, dialog.isInspection, inspectionId != dialog.id {
            show(dialog.workspaceTab)
        }
        if !snapshot.workspaceViews.contains(tab) { show(.conversation) }
        inspectionId = snapshot.dialog.flatMap { $0.isInspection ? $0.id : nil }
    }
}

extension EngineDialog {
    var workspaceTab: WorkspaceTab { kind == "diff" ? .changes : kind == "findings" ? .findings : .agents }
    var isInspection: Bool { ["diff", "failure", "sync_details", "findings"].contains(kind) && confirmLabel == nil }
}

struct QueueEditor {
    private(set) var selected: Int64?
    var text = ""

    mutating func select(_ id: Int64?, in queue: [QueuedMessage]) {
        selected = id
        text = queue.first(where: { $0.id == id })?.body ?? ""
    }
    mutating func reconcile(_ queue: [QueuedMessage]) {
        if selected == nil || !queue.contains(where: { $0.id == selected }) { select(queue.first?.id, in: queue) }
    }
}
