import SwiftUI

struct EngineDialogView: View {
    @Bindable var session: Session
    let dialog: EngineDialog
    @State private var repositorySlug = ""
    @State private var repositoryCreate = false

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(dialog.title).font(Theme.font(20, weight: .semibold))
            if let error = session.error ?? session.snapshot?.error {
                Text(error).foregroundStyle(Theme.red).textSelection(.enabled)
            }
            ScrollView {
                Text(dialog.body).textSelection(.enabled)
                    .font(dialog.kind == "diff" ? .system(.body, design: .monospaced) : Theme.font())
                    .frame(maxWidth: .infinity, alignment: .leading)
            }.frame(maxHeight: 360)
            if !dialog.domains.isEmpty {
                Text(dialog.domains.joined(separator: "\n")).font(.system(.callout, design: .monospaced)).textSelection(.enabled)
            }
            if dialog.kind == "repository" {
                TextField("owner/repository", text: $repositorySlug)
                    .textFieldStyle(.roundedBorder).accessibilityLabel("GitHub repository")
                Toggle("Create repository", isOn: $repositoryCreate)
            }
            HStack {
                Button(dialog.confirmLabel == nil && dialog.kind != "network" ? "Done" : "Cancel") {
                    session.enqueue(Command(type: "cancel"))
                }.keyboardShortcut(.cancelAction)
                Spacer()
                if dialog.kind == "network", let requestId = dialog.requestId {
                    Button("Deny") { network(requestId, allow: false) }
                    Button("Allow") { network(requestId, allow: true) }
                        .buttonStyle(WorkspaceButtonStyle(prominent: true))
                } else if let label = dialog.confirmLabel {
                    Button(label) { confirm() }
                        .buttonStyle(WorkspaceButtonStyle(prominent: true))
                        .keyboardShortcut(.defaultAction)
                }
            }.disabled(!session.editable || session.busy)
        }.padding(24).frame(width: 620).font(Theme.font()).foregroundStyle(Theme.ink).background(Theme.paper)
        .onAppear {
            repositorySlug = dialog.repositorySlug ?? ""
            repositoryCreate = dialog.repositoryCreate
        }
    }

    private func network(_ requestId: Int64, allow: Bool) {
        session.enqueue(Command(type: "network_decision", modId: session.active?.id, requestId: requestId, allow: allow))
    }

    private func confirm() {
        if dialog.kind == "repository" {
            session.enqueue(Command(type: "repository", modId: session.active?.id, slug: repositorySlug, create: repositoryCreate))
        } else {
            session.enqueue(Command(type: "confirm", modId: session.active?.id, kind: dialog.kind))
        }
    }
}
