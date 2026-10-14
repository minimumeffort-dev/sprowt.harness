import Foundation

enum InspectorTab: String, CaseIterable { case changes = "Changes", checks = "Checks", findings = "Findings", activity = "Activity" }

struct WorkspaceSelection: Equatable {
    struct Context: Equatable { let project: String; let modId: Int64 }
    enum Target: Equatable { case task(String), worker(Int64) }
    enum Inspector: Equatable { case hidden, visible(InspectorTab, Target?) }
    enum Queue: Equatable { case closed, open }

    private(set) var context: Context?
    private var inspectionId: String?
    var inspector = Inspector.visible(.activity, nil)
    var queue = Queue.closed

    var inspectorTab: InspectorTab? {
        if case .visible(let tab, _) = inspector { return tab }
        return nil
    }
    var target: Target? {
        if case .visible(_, let target) = inspector { return target }
        return nil
    }

    mutating func show(_ tab: InspectorTab, target: Target? = nil) { inspector = .visible(tab, target) }
    mutating func toggleInspector() { inspector = inspectorTab == nil ? .visible(.activity, nil) : .hidden }
    mutating func toggleQueue() { queue = queue == .closed ? .open : .closed }

    mutating func reconcile(_ snapshot: Snapshot) {
        let next = snapshot.activeMod.map { Context(project: snapshot.project, modId: $0.id) }
        if context != next {
            self = WorkspaceSelection()
            context = next
        }
        guard let mod = snapshot.activeMod else { return }
        switch target {
        case .task(let id) where mod.plan?.tasks.contains(where: { $0.id == id }) != true:
            inspector = .visible(inspectorTab ?? .activity, nil)
        case .worker(let id) where !mod.workerDetails.contains(where: { $0.id == id }):
            inspector = .visible(inspectorTab ?? .activity, nil)
        default: break
        }
        if mod.queue.isEmpty { queue = .closed }
        if let dialog = snapshot.dialog, dialog.isInspection, inspectionId != dialog.id {
            show(dialog.kind == "diff" ? .changes : dialog.kind == "findings" ? .findings : .activity)
        }
        inspectionId = snapshot.dialog.flatMap { $0.isInspection ? $0.id : nil }
    }
}

extension EngineDialog {
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
