import SwiftUI

struct ContentView: View {
    @Bindable var session: Session

    var body: some View {
        HSplitView {
            SidebarView(session: session).frame(minWidth: 190, idealWidth: 220, maxWidth: 240)
            VStack(spacing: 0) {
                if let error = session.error ?? session.snapshot?.error {
                    HStack(alignment: .top, spacing: 8) {
                        Image(systemName: "exclamationmark.triangle")
                        Text(error).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
                        if session.connection == .disconnected {
                            Button("Open project…") { session.chooseProject() }
                        }
                    }.font(Theme.font(12)).foregroundStyle(Theme.red).padding(12)
                        .background(Theme.red.opacity(0.06))
                    WorkspaceDivider()
                }
                if session.connection == .starting || session.connection == .stopping {
                    HStack(spacing: 10) {
                        ProgressView().controlSize(.small)
                        Text(session.connection == .starting ? "Opening project…" : "Saving and stopping workers…")
                        Spacer()
                    }.padding(16).foregroundStyle(Theme.muted)
                }
                if session.showingNew || (session.editable && session.active == nil) {
                    NewCodemodView(session: session)
                } else if let active = session.active {
                    WorkspaceView(session: session, mod: active)
                } else {
                    VStack(spacing: 16) {
                        Image(systemName: "folder").font(.system(size: 28)).foregroundStyle(Theme.muted)
                        Text("Open a project").font(Theme.font(20, weight: .semibold))
                        Button("Choose folder…") { session.chooseProject() }
                            .buttonStyle(WorkspaceButtonStyle(prominent: true))
                    }.frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }.frame(minWidth: 650, maxWidth: .infinity, maxHeight: .infinity).background(Theme.paper)
        }
        .font(Theme.font()).foregroundStyle(Theme.ink).tint(Theme.ink)
        .background(Theme.paper)
        .toolbar {
            ToolbarItem(placement: .principal) {
                HStack(spacing: 9) {
                    Text("sprowt").font(Theme.font(14, weight: .bold))
                    if let project = session.snapshot?.project {
                        Image(systemName: "folder").font(.system(size: 11)).foregroundStyle(Theme.muted)
                        Text(URL(fileURLWithPath: project).lastPathComponent)
                            .font(Theme.font(11)).foregroundStyle(Theme.muted).help(project)
                    }
                }
            }
        }
        .sheet(item: Binding(get: { session.dialog.flatMap { $0.isInspection ? nil : $0 } }, set: { _ in })) { dialog in
            EngineDialogView(session: session, dialog: dialog).interactiveDismissDisabled()
        }
    }
}

private struct NewCodemodView: View {
    @Bindable var session: Session
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            HStack {
                Text("What would you like to change?").font(Theme.font(22, weight: .semibold))
                Spacer()
                if session.active != nil {
                    GlyphButton(symbol: "xmark", label: "Return to codemod") { session.showingNew = false }
                }
            }
            TextEditor(text: $session.newDescription)
                .font(Theme.font(14)).padding(10)
                .scrollContentBackground(.hidden).background(Theme.paper)
                .overlay(RoundedRectangle(cornerRadius: Theme.radius).stroke(Theme.line))
                .frame(minHeight: 140, maxHeight: 220).focused($focused)
                .accessibilityLabel("Describe the codemod")
            HStack {
                Text("Describe the outcome.").font(Theme.font(12)).foregroundStyle(Theme.muted)
                Spacer()
                Button("Create codemod") { session.create() }
                    .buttonStyle(WorkspaceButtonStyle(prominent: true)).keyboardShortcut(.return, modifiers: .command)
                    .disabled(!session.interactive || session.busy || session.newDescription.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                Text("⌘↩").font(Theme.font(11)).foregroundStyle(Theme.muted)
            }
            Spacer()
        }.padding(Theme.inset).onAppear { focused = true }
    }
}
