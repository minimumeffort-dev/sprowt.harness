import SwiftUI

struct PlanView: View {
    let plan: Plan
    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Text("Plan").font(.headline)
                Spacer()
                Text(plan.status).foregroundStyle(.secondary)
            }
            if !plan.summary.isEmpty { Text(plan.summary).textSelection(.enabled) }
            ForEach(Array(plan.tasks.enumerated()), id: \.element.id) { index, task in
                VStack(alignment: .leading, spacing: 4) {
                    HStack(alignment: .firstTextBaseline) {
                        Text("\(index + 1). \(task.title)").fontWeight(.medium)
                        Spacer()
                        Text(task.status).font(.caption).foregroundStyle(.secondary)
                    }
                    Text(task.outcome).textSelection(.enabled)
                    if !task.dependsOn.isEmpty {
                        Text("After: \(task.dependsOn.joined(separator: ", "))").font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
            DisclosureGroup("Details") {
                VStack(alignment: .leading, spacing: 14) {
                    ForEach(plan.tasks) { task in
                        VStack(alignment: .leading, spacing: 5) {
                            Text(task.title).fontWeight(.medium)
                            if !task.detail.isEmpty { Text(task.detail) }
                            if !task.files.isEmpty { Text("Files: \(task.files.joined(separator: ", "))") }
                            if !task.checks.isEmpty { Text("Checks:\n\(task.checks.joined(separator: "\n"))") }
                        }.textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
                    }
                }.font(.callout).padding(.top, 8)
            }
        }.padding(16).background(.quaternary.opacity(0.3), in: RoundedRectangle(cornerRadius: 8))
    }
}

struct ChecksView: View {
    let checks: [Check]
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Checks").font(.headline)
            ForEach(Array(checks.enumerated()), id: \.offset) { _, check in
                DisclosureGroup {
                    Text(check.output.isEmpty ? "No output" : check.output)
                        .font(.system(.callout, design: .monospaced)).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading).padding(.top, 6)
                } label: {
                    HStack { Text(check.label); Spacer(); Text(check.status).foregroundStyle(.secondary) }
                }
            }
        }
    }
}

struct FindingsView: View {
    let findings: [Finding]
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Findings").font(.headline)
            ForEach(findings) { finding in
                DisclosureGroup {
                    Text(finding.detail).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading).padding(.top, 6)
                } label: {
                    HStack { Text(finding.title); Spacer(); Text(finding.status).foregroundStyle(.secondary) }
                }
            }
        }
    }
}

struct QueueView: View {
    @Bindable var session: Session
    let modId: Int64
    @Environment(\.dismiss) private var dismiss
    @State private var selected: Int64?
    @State private var editText = ""
    private var queue: [QueuedMessage] { session.active?.id == modId ? session.active?.queue ?? [] : [] }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Queued instructions").font(.title2.weight(.semibold))
            if let error = session.error { Text(error).foregroundStyle(.red).textSelection(.enabled) }
            List(selection: $selected) {
                ForEach(queue) { item in Text(item.body).lineLimit(3).tag(item.id) }
            }.frame(minHeight: 160)
            if let selected {
                TextEditor(text: $editText).font(.body).frame(height: 110)
                    .accessibilityLabel("Edit queued instruction")
                HStack {
                    Button("Save edit") { session.enqueue(Command(type: "edit_queue", modId: modId, text: editText, messageId: selected)) }
                        .disabled(editText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    Button { move(selected, direction: "up") } label: { Image(systemName: "arrow.up") }
                        .help("Move up").accessibilityLabel("Move instruction up").disabled(queue.first?.id == selected)
                    Button { move(selected, direction: "down") } label: { Image(systemName: "arrow.down") }
                        .help("Move down").accessibilityLabel("Move instruction down").disabled(queue.last?.id == selected)
                    Button("Remove", role: .destructive) {
                        session.enqueue(Command(type: "remove_queue", modId: modId, messageId: selected))
                    }
                    Spacer()
                    Button("Steer") { session.enqueue(Command(type: "steer_queue", modId: modId, messageIds: [selected])) }
                }.disabled(!session.editable || session.busy)
            }
            HStack {
                Spacer()
                Button("Done") { dismiss() }.keyboardShortcut(.cancelAction)
            }
        }.padding(24).frame(width: 580, height: 440)
        .onChange(of: selected) { _, id in editText = queue.first(where: { $0.id == id })?.body ?? "" }
        .onChange(of: queue.map(\.id)) { _, ids in
            if ids.isEmpty { dismiss() }
            else if selected == nil || !ids.contains(selected!) { selected = ids.first }
        }
        .onAppear { selected = queue.first?.id }
    }

    private func move(_ id: Int64, direction: String) {
        session.enqueue(Command(type: "move_queue", modId: modId, messageId: id, direction: direction))
    }
}
