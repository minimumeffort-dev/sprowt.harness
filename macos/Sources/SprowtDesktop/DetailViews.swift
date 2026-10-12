import SwiftUI

struct OverviewView: View {
    @Bindable var session: Session
    let mod: CodemodDetail

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            VStack(alignment: .leading, spacing: 8) {
                let summary = mod.plan?.summary ?? ""
                Text(summary.isEmpty ? mod.description : summary)
                    .font(Theme.font(21, weight: .semibold)).textSelection(.enabled)
                if let detail = session.snapshot?.detail, !detail.isEmpty {
                    Text(detail).font(Theme.font(12)).foregroundStyle(Theme.muted).textSelection(.enabled)
                }
                if let evidence = session.snapshot?.evidence, !evidence.isEmpty {
                    Text(evidence).font(Theme.font(11)).foregroundStyle(Theme.muted).textSelection(.enabled)
                }
            }
            HStack(spacing: 3) {
                ForEach(WorkStage.allCases, id: \.self) { stage in
                    Button(stage.rawValue) { session.workspace.stage = stage }
                        .buttonStyle(WorkspaceButtonStyle(selected: session.workspace.stage == stage))
                        .accessibilityAddTraits(session.workspace.stage == stage ? .isSelected : [])
                }
                Spacer(minLength: 0)
            }
            WorkspaceDivider()
            switch session.workspace.stage {
            case .plan, .build:
                if let plan = mod.plan {
                    TaskListView(tasks: plan.tasks, selected: session.workspace.target) { task in
                        session.workspace.show(.progress, target: .task(task.id))
                    }
                } else {
                    Text("The plan will appear here when it is saved.").foregroundStyle(Theme.muted)
                }
                if session.workspace.stage == .build {
                    ForEach(mod.plan?.tasks.filter { $0.latest != nil } ?? []) { task in
                        if let message = task.latest {
                            VStack(alignment: .leading, spacing: 10) {
                                MessageView(message: message)
                                Button { session.workspace.show(.activity, target: .task(task.id)) } label: {
                                    Label("Inspect task activity", systemImage: "arrow.up.right")
                                }.buttonStyle(WorkspaceButtonStyle())
                            }.padding(.top, 8)
                        }
                    }
                }
            case .checks:
                ChecksView(checks: mod.checks)
            case .review:
                FindingsView(findings: mod.findings)
                if let action = session.snapshot?.actions.first(where: { $0.id == "review" || $0.id == "findings" }) {
                    Button(action.label) { session.perform(action) }
                        .buttonStyle(WorkspaceButtonStyle()).disabled(!session.interactive || session.busy)
                }
            }
        }
    }
}

struct TaskListView: View {
    let tasks: [PlanTask]
    let selected: WorkspaceSelection.Target?
    let inspect: (PlanTask) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if tasks.isEmpty { Text("No tasks in the current plan.").foregroundStyle(Theme.muted) }
            ForEach(Array(tasks.enumerated()), id: \.element.id) { index, task in
                Button { inspect(task) } label: {
                    HStack(alignment: .top, spacing: 10) {
                        Text(String(index + 1)).font(Theme.font(10, weight: .medium)).foregroundStyle(Theme.muted)
                            .frame(width: 21, height: 21).background(Theme.sidebar, in: Circle())
                        VStack(alignment: .leading, spacing: 6) {
                            HStack(alignment: .firstTextBaseline, spacing: 8) {
                                Text(task.title).font(Theme.font(13, weight: .semibold))
                                Spacer(minLength: 8)
                                Label(task.status, systemImage: task.hasCurrentFailure ? "exclamationmark.circle" : task.completed ? "checkmark" : "circle")
                                    .font(Theme.font(10)).foregroundStyle(task.hasCurrentFailure ? Theme.red : task.completed ? Theme.green : Theme.muted)
                            }
                            Text(task.outcome).font(Theme.font(12)).foregroundStyle(Theme.muted)
                            if !task.dependsOn.isEmpty {
                                Text("After: \(task.dependsOn.map { id in tasks.first(where: { $0.id == id })?.title ?? id }.joined(separator: ", "))")
                                    .font(Theme.font(10)).foregroundStyle(Theme.muted)
                            }
                        }
                    }.frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 8)
                }.buttonStyle(WorkspaceButtonStyle(selected: selected == .task(task.id)))
                    .accessibilityLabel("Inspect task \(index + 1): \(task.title)")
                WorkspaceDivider()
            }
        }
    }
}

struct ConversationView: View {
    let mod: CodemodDetail
    var body: some View {
        VStack(alignment: .leading, spacing: 24) {
            ForEach(Array(mod.messages.enumerated()), id: \.offset) { _, message in MessageView(message: message) }
            if !mod.history.isEmpty {
                DisclosureGroup("Saved history") {
                    VStack(alignment: .leading, spacing: 24) {
                        ForEach(Array(mod.history.enumerated()), id: \.offset) { _, message in MessageView(message: message) }
                    }.padding(.top, 16)
                }.font(Theme.font(11)).foregroundStyle(Theme.muted)
            }
        }
    }
}

struct MessageView: View {
    let message: ConversationMessage
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if message.role != "user" {
                HStack(alignment: .firstTextBaseline, spacing: 7) {
                    Image(systemName: "leaf").font(.system(size: 12)).frame(width: 21, height: 21)
                        .background(Theme.gold.opacity(0.22), in: RoundedRectangle(cornerRadius: 5))
                        .symbolBaseline(textSize: 11)
                    Text(message.role.replacingOccurrences(of: ":", with: " · "))
                        .font(Theme.font(11, weight: .medium)).foregroundStyle(Theme.muted)
                }
            }
            Text(message.role == "user" ? "> \(message.body)" : message.body)
                .font(Theme.font(13)).foregroundStyle(Theme.ink)
                .frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled)
        }.padding(message.role == "user" ? 12 : 0)
            .background(message.role == "user" ? Theme.panel : .clear,
                        in: RoundedRectangle(cornerRadius: Theme.radius))
    }
}

struct ChecksView: View {
    let checks: [Check]
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if checks.isEmpty { Text("No saved check results.").font(Theme.font(12)).foregroundStyle(Theme.muted) }
            ForEach(checks) { check in
                DisclosureGroup {
                    Text(check.output.isEmpty ? "No output" : check.output)
                        .font(.system(size: 11, design: .monospaced)).foregroundStyle(Theme.ink).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading).padding(.top, 8)
                } label: {
                    HStack(alignment: .firstTextBaseline, spacing: 7) {
                        Image(systemName: check.status == .passed ? "checkmark" : check.status == .failed ? "xmark.circle" : "minus.circle")
                            .font(.system(size: 11)).foregroundStyle(check.status == .passed ? Theme.green : check.status == .failed ? Theme.red : Theme.muted)
                            .symbolBaseline(textSize: 12)
                        VStack(alignment: .leading, spacing: 4) {
                            Text(check.label).font(Theme.font(12)).foregroundStyle(Theme.ink)
                            Text(check.kind.caption).font(Theme.font(10)).foregroundStyle(Theme.muted)
                        }
                        Spacer(minLength: 8)
                        Text(check.status.rawValue).font(Theme.font(10)).foregroundStyle(Theme.muted)
                    }
                }
                WorkspaceDivider()
            }
        }
    }
}

struct FindingsView: View {
    let findings: [Finding]
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if findings.isEmpty { Text("No saved findings.").font(Theme.font(12)).foregroundStyle(Theme.muted) }
            ForEach(findings) { finding in
                DisclosureGroup {
                    Text(finding.detail).font(Theme.font(12)).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading).padding(.top, 8)
                } label: {
                    HStack(alignment: .firstTextBaseline, spacing: 7) {
                        Image(systemName: "checkmark.bubble").font(.system(size: 11)).foregroundStyle(Theme.muted).symbolBaseline(textSize: 12)
                        Text(finding.title).font(Theme.font(12))
                        Spacer(minLength: 8)
                        Text(finding.status).font(Theme.font(10)).foregroundStyle(Theme.muted)
                    }
                }
                WorkspaceDivider()
            }
        }
    }
}

extension EvidenceKind {
    var caption: String {
        switch self {
        case .check: "Task verification · before combined checks"
        case .finalCheck: "Combined version checks"
        case .previousCheck: "Previous failed checks"
        case .observedCheck: "Observed task checks"
        case .preparationSetup: "Environment setup"
        case .preparationCapabilities: "Capability checks"
        case .preparationBaseline: "Starting-source baseline"
        }
    }
}
