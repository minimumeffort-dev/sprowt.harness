import SwiftUI

struct AgentsView: View {
    let agents: AgentsEvidence
    let rawTimings: String
    @Binding var selection: WorkspaceSelection
    var selectedWorker: Int64?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(agents.workers.filter { selectedWorker == nil || $0.id == selectedWorker }.enumerated()), id: \.element.id) { index, agent in
                if index > 0 { WorkspaceDivider().padding(.vertical, 22) }
                worker(agent)
            }
            if !agents.timings.isEmpty {
                WorkspaceDivider().padding(.top, 26).padding(.bottom, 18)
                timings
            }
        }.frame(maxWidth: .infinity, alignment: .leading)
    }

    private func worker(_ agent: AgentEvidence) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .top, spacing: 11) {
                Group {
                    if let number = agent.taskNumber { Text("\(number)") }
                    else { Image(systemName: "person") }
                }.font(Theme.font(11, weight: .semibold)).foregroundStyle(Theme.muted)
                    .frame(width: 27, height: 27).background(Theme.panel, in: Circle())
                VStack(alignment: .leading, spacing: 5) {
                    Text(agent.title).font(Theme.font(15, weight: .semibold)).textSelection(.enabled)
                    Text("\(agent.worker.provider.capitalized) · \(agent.worker.role) · w\(agent.id)")
                        .font(Theme.font(11)).foregroundStyle(Theme.muted)
                }
                Spacer(minLength: 10)
                Label(agent.state, systemImage: agent.tone == "attention" ? "exclamationmark.circle" : agent.tone == "complete" ? "checkmark" : "circle.fill")
                    .font(Theme.font(11, weight: .medium))
                    .foregroundStyle(agent.tone == "attention" ? Theme.amber : agent.tone == "working" ? Theme.blue : Theme.muted)
            }
            VStack(alignment: .leading, spacing: 12) {
                if let failure = agent.failure {
                    Text(failure.summary).font(Theme.font(13)).textSelection(.enabled)
                } else if !agent.activityPreview.isEmpty {
                    Text(agent.activityPreview).font(Theme.font(13)).foregroundStyle(Theme.muted).lineLimit(2)
                }
                HStack(spacing: 18) {
                    if agent.failure != nil { disclosure("Failure details", key: "failure:\(agent.id)") }
                    else if !agent.activity.isEmpty { disclosure("Activity details", key: "activity:\(agent.id)") }
                    disclosure("Worker details", key: "worker:\(agent.id)")
                }
                if let failure = agent.failure, selection.agentDetails.contains("failure:\(agent.id)") {
                    CodeText(text: failure.detail, label: "Failure details")
                }
                if selection.agentDetails.contains("activity:\(agent.id)") && !agent.activity.isEmpty {
                    MessageText(source: agent.activity)
                }
                if selection.agentDetails.contains("worker:\(agent.id)") {
                    Text(agent.worker.identity).font(Theme.font(12)).foregroundStyle(Theme.muted).textSelection(.enabled)
                    if let diagnostic = agent.previousDiagnostic {
                        CodeText(text: diagnostic, label: "Previous worker diagnostic")
                    }
                }
            }.padding(.leading, 38).frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var timings: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack {
                disclosure("Setup timings", key: "timings")
                Spacer()
                if !selection.agentDetails.contains("timings"), let largest = agents.timings.first {
                    Text("\(largest.label)  \(largest.duration)").font(Theme.font(11)).foregroundStyle(Theme.muted).monospacedDigit()
                }
            }
            if selection.agentDetails.contains("timings") {
                VStack(spacing: 12) {
                    ForEach(agents.timings, id: \.label) { timing in
                        HStack {
                            Text(timing.label).font(Theme.font(12)).foregroundStyle(Theme.muted)
                            Spacer()
                            Text(timing.duration).font(Theme.font(12, weight: .medium)).monospacedDigit()
                        }
                    }
                    HStack { disclosure("Exact timing evidence", key: "timing-source"); Spacer() }
                }.frame(maxWidth: 460).padding(.leading, 17)
                if selection.agentDetails.contains("timing-source") { CodeText(text: rawTimings) }
            }
        }
    }

    private func disclosure(_ label: String, key: String) -> some View {
        let expanded = selection.agentDetails.contains(key)
        return Button {
            if expanded { selection.agentDetails.remove(key) } else { selection.agentDetails.insert(key) }
        } label: {
            HStack(spacing: 6) {
                Image(systemName: expanded ? "chevron.down" : "chevron.right").font(.system(size: 8, weight: .semibold))
                Text(expanded ? "Hide \(label.lowercased())" : label).font(Theme.font(11, weight: .medium))
            }.foregroundStyle(Theme.muted)
        }.buttonStyle(.plain).accessibilityLabel(label).accessibilityValue(expanded ? "Expanded" : "Collapsed")
    }
}
