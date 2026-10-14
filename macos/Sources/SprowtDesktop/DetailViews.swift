import SwiftUI

struct FeedView: View {
    @Bindable var session: Session
    let mod: CodemodDetail

    var body: some View {
        VStack(alignment: .leading, spacing: 24) {
            ForEach(Array(mod.messages.enumerated()), id: \.offset) { _, message in MessageView(message: message) }
            if let plan = mod.plan, !plan.tasks.isEmpty {
                TaskListView(tasks: plan.tasks, selected: session.workspace.target) { task in
                    session.workspace.show(.conversation, target: .task(task.id))
                }
            }
            SavedHistoryView(mod: mod)
        }
    }
}

struct TaskListView: View {
    let tasks: [PlanTask]
    let selected: WorkspaceSelection.Target?
    let inspect: (PlanTask) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
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
                                    .font(Theme.font(10)).foregroundStyle(task.hasCurrentFailure ? Theme.red : task.completed ? Theme.green : Theme.statusColor(task.status))
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

struct SavedHistoryView: View {
    let mod: CodemodDetail
    var body: some View {
        VStack(alignment: .leading, spacing: 24) {
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
                        .background(Theme.accent.opacity(0.16), in: RoundedRectangle(cornerRadius: 5))
                        .symbolBaseline(textSize: 11)
                    Text(message.author)
                        .font(Theme.font(11, weight: .medium)).foregroundStyle(Theme.muted)
                }
            }
            Text(message.role == "user" ? "> \(message.body)" : message.body)
                .font(Theme.font(14)).foregroundStyle(Theme.ink)
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

struct PrFeedbackView: View {
    @Bindable var session: Session
    let mod: CodemodDetail
    @State private var selected: Set<String> = []

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("GitHub PR feedback").font(Theme.font(13, weight: .semibold)).padding(.top, 8)
            if let error = mod.prFeedbackError {
                Text("GitHub feedback unavailable · \(error)").font(Theme.font(12)).foregroundStyle(Theme.muted).textSelection(.enabled)
            }
            ForEach(mod.prFeedback) { item in
                HStack(alignment: .top, spacing: 8) {
                    Toggle("", isOn: Binding(
                        get: { selected.contains(item.id) },
                        set: { on in if on { selected.insert(item.id) } else { selected.remove(item.id) } }
                    )).toggleStyle(.checkbox).labelsHidden().disabled(item.imported || !session.interactive || session.busy)
                    DisclosureGroup {
                        Text(item.detail).font(Theme.font(12)).textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading).padding(.top, 8)
                    } label: {
                        HStack(alignment: .firstTextBaseline, spacing: 7) {
                            Text(item.title).font(Theme.font(12))
                            Spacer(minLength: 8)
                            Text(item.imported ? "in review · \(item.status)" : item.status)
                                .font(Theme.font(10)).foregroundStyle(Theme.muted)
                        }
                    }
                }
                WorkspaceDivider()
            }
            Button("Send selected to review") {
                let ids = mod.prFeedback.map(\.id).filter { selected.contains($0) }
                session.enqueue(Command(type: "fix_pr_feedback", modId: mod.id, feedbackIds: ids))
                selected.removeAll()
            }.buttonStyle(WorkspaceButtonStyle())
                .disabled(selected.isEmpty || !session.interactive || session.busy)
                .help("Add the selected feedback to the review ledger; a fresh review triages it before any repair.")
        }.onChange(of: mod.prFeedback.map(\.id)) { _, ids in
            selected.formIntersection(Set(ids))
            selected.subtract(mod.prFeedback.filter(\.imported).map(\.id))
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
