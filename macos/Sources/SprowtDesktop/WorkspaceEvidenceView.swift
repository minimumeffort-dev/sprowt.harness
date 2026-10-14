import SwiftUI

struct WorkspaceEvidenceView: View {
    @Bindable var session: Session
    let mod: CodemodDetail

    private var selectedWorker: WorkerSummary? {
        guard case .worker(let id) = session.workspace.target else { return nil }
        return mod.workerDetails.first(where: { $0.id == id })
    }
    private var selectedTask: PlanTask? { mod.inspectedTask(session.workspace.target) }
    private var inspection: EngineDialog? { session.dialog.flatMap { $0.isInspection ? $0 : nil } }

    var body: some View {
        let selectedTab = session.workspace.tab
        let currentInspection = inspection
        return VStack(alignment: .leading, spacing: 18) {
            if session.workspace.target != nil {
                Button("Back to \(selectedTab.label)") { session.workspace.show(selectedTab) }
                    .buttonStyle(WorkspaceButtonStyle())
            }
            if let inspection = currentInspection, (selectedTab == inspection.workspaceTab || selectedTab == .conversation && session.hasUnlistedInspection) {
                Text(inspection.title).font(Theme.font(16, weight: .semibold))
                if inspection.kind == "diff" {
                    CodeText(text: inspection.body)
                } else {
                    MessageText(source: inspection.body)
                }
            } else {
                panel(selectedTab)
            }
        }.padding(Theme.inset).frame(maxWidth: 790, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .leading)
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
            ForEach(Array(mod.checkActivity.filter { selectedTask == nil || $0.taskId == selectedTask?.id }.enumerated()), id: \.offset) { _, activity in
                Text(activity.label).font(Theme.font(12)).foregroundStyle(Theme.accent)
            }
            if let task = selectedTask {
                SectionHeading(title: task.checksLabel)
                Text(task.title).font(Theme.font(16, weight: .semibold))
                if !task.results.isEmpty { ChecksView(checks: task.results) }
                PreparationView(preparation: task.preparation)
            } else {
                SectionHeading(title: "Checks", detail: mod.checks.isEmpty ? "" : "\(mod.checks.count) saved")
                if !mod.checks.isEmpty { ChecksView(checks: mod.checks) }
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
                    CodeText(text: mod.timings).padding(.top, 8)
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
            if let taskTitle { Text(taskTitle).font(Theme.font(14)).foregroundStyle(Theme.ink) }
            Text(worker.state).font(Theme.font(11)).foregroundStyle(Theme.statusColor(worker.state))
            if !worker.activity.isEmpty { MessageText(source: worker.activity) }
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
        VStack(alignment: .leading, spacing: 20) {
            Text(task.title).font(Theme.font(18, weight: .semibold)).textSelection(.enabled)
            Label(task.status, systemImage: task.hasCurrentFailure ? "exclamationmark.circle" : task.completed ? "checkmark" : "circle")
                .font(Theme.font(12)).foregroundStyle(task.hasCurrentFailure ? Theme.red : task.completed ? Theme.green : Theme.statusColor(task.status))
            MessageText(source: task.outcome)
            if !task.detail.isEmpty && task.detail != task.error { MessageText(source: task.detail) }
            if let error = task.error, !task.preparationOwnsError { EvidenceNotice(text: error) }
            if let latest = task.latest { MessageView(message: latest, toggles: $session.workspace.messageToggles) }
            DisclosureGroup {
                VStack(alignment: .leading, spacing: 18) {
                    if !task.identity.isEmpty {
                        Text(task.identity).font(Theme.font(13, weight: .medium)).foregroundStyle(Theme.ink)
                    }
                    if let source = task.source { SourceReference(source: source) }
                    if !task.files.isEmpty {
                        VStack(alignment: .leading, spacing: 10) {
                            evidenceHeading("File scope", count: task.files.count == 1 ? "1 file" : "\(task.files.count) files")
                            FileScopeView(files: task.files)
                        }
                    }
                    if !task.checks.isEmpty {
                        VStack(alignment: .leading, spacing: 12) {
                            evidenceHeading("Required checks", count: task.checks.count == 1 ? "1 check" : "\(task.checks.count) checks")
                            ForEach(Array(task.checks.enumerated()), id: \.offset) { _, check in
                                HStack(alignment: .firstTextBaseline, spacing: 9) {
                                    Text("•").font(Theme.font(12)).foregroundStyle(Theme.muted)
                                    MessageText(source: check)
                                }
                            }
                        }
                    }
                }.frame(maxWidth: .infinity, alignment: .leading).padding(.top, 16)
            } label: {
                Text("Files, checks and worker").font(Theme.font(14, weight: .semibold)).foregroundStyle(Theme.ink)
            }
            if !task.results.isEmpty {
                Button { session.workspace.show(.checks, target: .task(task.id)) } label: { Label("View check output", systemImage: "arrow.up.right") }
                    .buttonStyle(WorkspaceButtonStyle())
            }
            PreparationView(preparation: task.preparation)
            ForEach(task.actions, id: \.identity) { action in
                Button { session.perform(action) } label: { Label(action.label, systemImage: action.symbol) }
                    .buttonStyle(WorkspaceButtonStyle()).disabled(!session.interactive || session.busy)
            }
        }.frame(maxWidth: .infinity, alignment: .leading)
    }

    private func evidenceHeading(_ title: String, count: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(title).font(Theme.font(13, weight: .semibold)).foregroundStyle(Theme.ink)
            Text(count).font(Theme.font(12)).foregroundStyle(Theme.muted)
        }
    }
}

private struct FileScopeView: View {
    let files: [String]

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            ForEach(Array(files.enumerated()), id: \.offset) { _, path in
                Text(styledPath(path)).frame(maxWidth: .infinity, alignment: .leading)
            }
        }.font(.system(size: Theme.scaledSize(12), design: .monospaced))
            .lineSpacing(4).textSelection(.enabled).padding(.vertical, 12).padding(.horizontal, 14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.codeBackground, in: RoundedRectangle(cornerRadius: Theme.radius))
    }

    private func styledPath(_ path: String) -> AttributedString {
        var result = AttributedString(path)
        result.foregroundColor = Theme.ink
        if let slash = path.lastIndex(of: "/"),
           let prefix = result.range(of: String(path[...slash])) {
            result[prefix].foregroundColor = Theme.muted
        }
        return result
    }
}

private struct PreparationView: View {
    let preparation: PreparationEvidence?
    @State private var expanded: Bool

    init(preparation: PreparationEvidence?) {
        self.preparation = preparation
        _expanded = State(initialValue: preparation?.current == true && preparation?.error != nil)
    }

    var body: some View {
        if let preparation {
            VStack(alignment: .leading, spacing: 16) {
                WorkspaceDivider()
                DisclosureGroup(isExpanded: $expanded) {
                    VStack(alignment: .leading, spacing: 16) {
                        SourceReference(source: preparation.source)
                        if preparation.stopped {
                            Text("Preparation stopped.").font(Theme.font(14)).foregroundStyle(Theme.muted)
                        }
                        if let error = preparation.error {
                            EvidenceNotice(text: error, color: error.hasPrefix("Network access pending:") ? Theme.amber : Theme.red)
                        }
                        ChecksView(checks: preparation.results)
                    }.frame(maxWidth: .infinity, alignment: .leading).padding(.top, 16)
                } label: {
                    Text(preparation.current ? "Environment preparation" : "Previous environment preparation")
                        .font(Theme.font(14, weight: .semibold)).foregroundStyle(Theme.ink)
                }
            }.frame(maxWidth: .infinity, alignment: .leading)
                .onChange(of: preparation.error) { _, error in if preparation.current && error != nil { expanded = true } }
                .onChange(of: preparation.current) { _, current in if current && preparation.error != nil { expanded = true } }
        }
    }
}
