import SwiftUI

struct AttachedQueueView: View {
    @Bindable var session: Session
    let mod: CodemodDetail
    @State private var editor = QueueEditor()

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Queued instructions").font(Theme.font(11, weight: .semibold)).foregroundStyle(Theme.muted)
                Spacer()
                GlyphButton(symbol: "xmark", label: "Collapse queue") { session.workspace.queue = .closed }
                    .keyboardShortcut(session.dialog?.isInspection == true ? nil : .cancelAction)
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    VStack(alignment: .leading, spacing: 3) {
                        ForEach(Array(mod.queue.enumerated()), id: \.element.id) { index, item in
                            Button { editor.select(item.id, in: mod.queue) } label: {
                                HStack(alignment: .top, spacing: 8) {
                                    Text(String(index + 1)).font(Theme.font(10)).foregroundStyle(Theme.muted).frame(width: 12)
                                    Text(item.body).font(Theme.font(12)).lineLimit(2).frame(maxWidth: .infinity, alignment: .leading)
                                }
                            }.buttonStyle(WorkspaceButtonStyle(selected: editor.selected == item.id))
                        }
                    }
                    if let id = editor.selected {
                        TextEditor(text: $editor.text).font(Theme.font(12)).frame(height: 54)
                            .scrollContentBackground(.hidden).padding(6).background(Theme.paper)
                            .overlay(RoundedRectangle(cornerRadius: 5).stroke(Theme.line))
                            .accessibilityLabel("Edit queued instruction")
                        HStack(spacing: 3) {
                            Button("Save edit") { session.enqueue(Command(type: "edit_queue", modId: mod.id, text: editor.text, messageId: id)) }
                                .buttonStyle(WorkspaceButtonStyle())
                                .disabled(editor.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                            GlyphButton(symbol: "arrow.up", label: "Move instruction up") { move(id, "up") }
                                .disabled(mod.queue.first?.id == id)
                            GlyphButton(symbol: "arrow.down", label: "Move instruction down") { move(id, "down") }
                                .disabled(mod.queue.last?.id == id)
                            GlyphButton(symbol: "xmark", label: "Remove queued instruction") {
                                session.enqueue(Command(type: "remove_queue", modId: mod.id, messageId: id))
                            }
                            Spacer(minLength: 3)
                            Button { session.enqueue(Command(type: "steer_queue", modId: mod.id, messageIds: [id])) } label: {
                                Label("Steer", systemImage: "arrow.up.right")
                            }.buttonStyle(WorkspaceButtonStyle()).help("Steer with the selected instruction; wait for workers when none are connected.")
                                .disabled(editor.text != mod.queue.first(where: { $0.id == id })?.body)
                        }.disabled(!session.interactive || session.busy)
                        if editor.text != mod.queue.first(where: { $0.id == id })?.body {
                            Text("Save the edit before steering.").font(Theme.font(10)).foregroundStyle(Theme.muted)
                        }
                    }
                }.frame(maxWidth: .infinity, alignment: .leading)
            }.frame(height: 180)
        }.padding(10).background(Theme.panel, in: RoundedRectangle(cornerRadius: Theme.radius))
            .overlay(RoundedRectangle(cornerRadius: Theme.radius).stroke(Theme.line))
            .onAppear { editor.reconcile(mod.queue) }
            .onChange(of: mod.queue.map(\.id)) { _, _ in editor.reconcile(mod.queue) }
    }

    private func move(_ id: Int64, _ direction: String) {
        session.enqueue(Command(type: "move_queue", modId: mod.id, messageId: id, direction: direction))
    }
}
