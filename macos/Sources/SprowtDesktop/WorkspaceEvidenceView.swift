import SwiftUI

struct WorkspaceEvidenceView: View {
    @Bindable var session: Session
    let mod: CodemodDetail

    private var selectedWorker: WorkerSummary? {
        guard case .worker(let id) = session.workspace.target else { return nil }
        return mod.workerDetails.first(where: { $0.id == id })
    }
    private var selectedTask: PlanTask? {
        let id: String?
        switch session.workspace.target {
        case .task(let task): id = task
        case .worker: id = selectedWorker?.taskId
        case nil: id = nil
        }
        return mod.plan?.tasks.first(where: { $0.id == id })
    }
    private var inspection: EngineDialog? { session.dialog.flatMap { $0.isInspection ? $0 : nil } }

    var body: some View {
        let selectedTab = session.workspace.tab
        let currentInspection = inspection
        return VStack(alignment: .leading, spacing: 18) {
            if session.workspace.target != nil {
                Button("Back to \(selectedTab.label)") { session.workspace.show(selectedTab) }
                    .buttonStyle(WorkspaceButtonStyle())
            }
            if let inspection = currentInspection, selectedTab == inspection.workspaceTab {
                Text(inspection.title).font(Theme.font(16, weight: .semibold))
                Text(inspection.body).font(inspection.kind == "diff" ? .system(size: 11, design: .monospaced) : Theme.font(12))
                    .textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
            } else {
                panel(selectedTab)
            }
        }.padding(Theme.inset).frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder private func panel(_ tab: WorkspaceTab) -> some View {
        switch tab {
        case .changes: changes
        case .checks: checks
        case .findings: findings
        case .agents: agents
        case .conversation:
            if let task = selectedTask { TaskInspectorView(session: session, task: task) }
        }
    }

    private var findings: some View {
        VStack(alignment: .leading, spacing: 18) {
            SectionHeading(title: "Review findings", detail: mod.findings.isEmpty ? "" : "\(mod.findings.count) saved")
            FindingsView(findings: mod.findings)
            if let action = session.snapshot?.actions.first(where: { $0.id == "review" || $0.id == "findings" || $0.id == "fix_issues" }) {
                Button(action.label) { session.perform(action) }
                    .buttonStyle(WorkspaceButtonStyle()).disabled(!session.interactive || session.busy)
            }
            if !mod.prFeedback.isEmpty || mod.prFeedbackError != nil {
                PrFeedbackView(session: session, mod: mod)
            }
        }
    }

    private var changes: some View {
        VStack(alignment: .leading, spacing: 14) {
            SectionHeading(title: "Source changes")
            if let action = session.snapshot?.actions.first(where: { $0.id == "diff" }) {
                Button { session.perform(action) } label: { Label("Inspect current changes", systemImage: "doc.text.magnifyingglass") }
                    .buttonStyle(WorkspaceButtonStyle()).disabled(!session.interactive || session.busy)
            } else {
                Text(mod.plan?.tasks.contains(where: { $0.runId != nil }) == true ? "The diff is available when the current work stops." : "No execution changes to inspect.")
                    .font(Theme.font(12)).foregroundStyle(Theme.muted)
            }
            if let task = selectedTask {
                TaskInspectorView(session: session, task: task)
            }
        }
    }

    private var checks: some View {
        VStack(alignment: .leading, spacing: 18) {
            if let task = selectedTask {
                SectionHeading(title: task.checksLabel)
                Text(task.title).font(Theme.font(16, weight: .semibold))
                ChecksView(checks: task.results)
                PreparationView(preparation: task.preparation)
            } else {
                SectionHeading(title: "Saved checks", detail: "\(mod.checks.count) saved")
                ChecksView(checks: mod.checks)
                ForEach(mod.plan?.tasks.filter { $0.preparation != nil } ?? []) { task in
                    VStack(alignment: .leading, spacing: 8) {
                        Text(task.title).font(Theme.font(12, weight: .medium))
                        PreparationView(preparation: task.preparation)
                    }
                }
            }
        }
    }

    private var agents: some View {
        VStack(alignment: .leading, spacing: 18) {
            if !mod.timings.isEmpty {
                DisclosureGroup("Recorded timings") {
                    Text(mod.timings).font(.system(size: 11, design: .monospaced)).textSelection(.enabled)
                }.font(Theme.font(11)).foregroundStyle(Theme.muted)
            }
            if let worker = selectedWorker {
                WorkerActivityView(worker: worker, showDetails: true)
                if let task = selectedTask { TaskInspectorView(session: session, task: task) }
            } else {
                if mod.workerDetails.isEmpty {
                    Text("No agents yet.").font(Theme.font(12)).foregroundStyle(Theme.muted)
                }
                ForEach(mod.workerDetails) { worker in
                    Button { session.workspace.show(.agents, target: .worker(worker.id)) } label: {
                        WorkerActivityView(worker: worker, taskTitle: mod.plan?.tasks.first(where: { $0.id == worker.taskId })?.title)
                    }.buttonStyle(WorkspaceButtonStyle())
                }
            }
        }
    }

}

private struct WorkerActivityView: View {
    let worker: WorkerSummary
    var taskTitle: String?
    var showDetails = false
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label("\(worker.provider.capitalized) · \(worker.role)", systemImage: "arrow.turn.down.right").font(Theme.font(12, weight: .medium))
            if let taskTitle { Text(taskTitle).font(Theme.font(12)) }
            Text(worker.state).font(Theme.font(11)).foregroundStyle(Theme.statusColor(worker.state))
            if !worker.activity.isEmpty { Text(worker.activity).font(Theme.font(12)).foregroundStyle(Theme.muted).textSelection(.enabled) }
            if showDetails {
                DisclosureGroup("Worker details") {
                    Text(worker.identity).font(Theme.font(12)).textSelection(.enabled).padding(.top, 8)
                }.font(Theme.font(11)).foregroundStyle(Theme.muted)
            }
        }.frame(maxWidth: .infinity, alignment: .leading)
    }
}

struct TaskInspectorView: View {
    @Bindable var session: Session
    let task: PlanTask

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(task.title).font(Theme.font(16, weight: .semibold)).textSelection(.enabled)
            Label(task.status, systemImage: task.hasCurrentFailure ? "exclamationmark.circle" : task.completed ? "checkmark" : "circle")
                .font(Theme.font(11)).foregroundStyle(task.hasCurrentFailure ? Theme.red : task.completed ? Theme.green : Theme.statusColor(task.status))
            Text(task.outcome).font(Theme.font(12)).textSelection(.enabled)
            if !task.detail.isEmpty { Text(task.detail).font(Theme.font(12)).textSelection(.enabled) }
            if let error = task.error { Text(error).font(Theme.font(12)).foregroundStyle(Theme.red).textSelection(.enabled) }
            if let latest = task.latest { MessageView(message: latest) }
            DisclosureGroup("Files, checks and worker") {
                VStack(alignment: .leading, spacing: 10) {
                    if !task.identity.isEmpty { Text(task.identity) }
                    if let source = task.source { Text("Source: \(source)").font(.system(size: 11, design: .monospaced)) }
                    if !task.files.isEmpty {
                        Text("File scope").font(Theme.font(11, weight: .semibold)).foregroundStyle(Theme.muted)
                        Text(task.files.joined(separator: "\n")).font(.system(size: 11, design: .monospaced))
                    }
                    if !task.checks.isEmpty {
                        Text("Required checks").font(Theme.font(11, weight: .semibold)).foregroundStyle(Theme.muted)
                        Text(task.checks.joined(separator: "\n"))
                    }
                }.font(Theme.font(12)).textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading).padding(.top, 10)
            }.font(Theme.font(11)).foregroundStyle(Theme.muted)
            if !task.results.isEmpty {
                Button { session.workspace.show(.checks, target: .task(task.id)) } label: { Label("View check output", systemImage: "arrow.up.right") }
                    .buttonStyle(WorkspaceButtonStyle())
            }
            PreparationView(preparation: task.preparation)
            ForEach(task.actions, id: \.identity) { action in
                Button { session.perform(action) } label: { Label(action.label, systemImage: action.symbol) }
                    .buttonStyle(WorkspaceButtonStyle()).disabled(!session.interactive || session.busy)
            }
        }
    }
}

private struct PreparationView: View {
    let preparation: PreparationEvidence?
    var body: some View {
        if let preparation {
            DisclosureGroup(preparation.current ? "Environment preparation" : "Previous environment preparation") {
                VStack(alignment: .leading, spacing: 10) {
                    Text("Source: \(preparation.source)").font(.system(size: 11, design: .monospaced)).textSelection(.enabled)
                    if preparation.stopped { Text("Preparation stopped.") }
                    if let error = preparation.error { Text(error).foregroundStyle(Theme.red).textSelection(.enabled) }
                    ChecksView(checks: preparation.results)
                }.font(Theme.font(12)).padding(.top, 10)
            }.font(Theme.font(11)).foregroundStyle(Theme.muted)
        }
    }
}
