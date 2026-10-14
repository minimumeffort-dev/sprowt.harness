import SwiftUI

struct WorkspaceView: View {
    @Bindable var session: Session
    let mod: CodemodDetail
    @FocusState private var composerFocused: Bool

    private var primaryAction: EngineAction? { session.snapshot?.actions.first }

    var body: some View {
        VStack(spacing: 0) {
            header
            WorkspaceDivider()
            HSplitView {
                VStack(spacing: 0) {
                    ScrollView {
                        FeedView(session: session, mod: mod)
                            .padding(Theme.inset).frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .id(mod.id)
                    composer
                }.frame(minWidth: 350, maxWidth: .infinity)
                if session.workspace.inspectorTab != nil {
                    InspectorView(session: session, mod: mod)
                        .frame(minWidth: 290, idealWidth: 330, maxWidth: 350)
                }
            }
        }
        .onChange(of: session.workspace.queue) { _, state in if state == .closed { composerFocused = true } }
        .onChange(of: mod.id) { _, _ in composerFocused = true }
        .onChange(of: session.dialog?.id) { _, id in if id == nil { composerFocused = true } }
    }

    private var header: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 5) {
                Text(mod.name).font(Theme.font(15, weight: .semibold)).lineLimit(2).textSelection(.enabled)
                HStack(spacing: 6) {
                    Circle().fill(session.snapshot?.phase.color ?? Theme.muted).frame(width: 5, height: 5)
                    Text(session.snapshot?.status ?? "").lineLimit(1)
                    if !mod.workerDetails.isEmpty { Text("· \(mod.workerDetails.count) agents") }
                }.font(Theme.font(11)).foregroundStyle(Theme.muted)
            }
            Spacer(minLength: 8)
            if let action = primaryAction {
                Button { session.perform(action) } label: { Label(action.label, systemImage: action.symbol) }
                    .buttonStyle(WorkspaceButtonStyle()).overlay(RoundedRectangle(cornerRadius: 5).stroke(Theme.line))
                    .disabled(!session.interactive || session.busy)
            }
            GlyphButton(symbol: "sidebar.right", label: session.workspace.inspectorTab == nil ? "Show workspace" : "Hide workspace", selected: session.workspace.inspectorTab != nil) {
                session.workspace.toggleInspector()
            }
            if let actions = session.snapshot?.actions, actions.count > 1 {
                Menu {
                    ForEach(Array(actions.dropFirst()), id: \.identity) { action in
                        Button { session.perform(action) } label: { Label(action.label, systemImage: action.symbol) }
                    }
                } label: { Image(systemName: "ellipsis").font(.system(size: 13)).frame(width: 24, height: 24) }
                    .menuStyle(.borderlessButton).fixedSize().accessibilityLabel("More codemod actions")
                    .disabled(!session.interactive || session.busy)
            }
        }.padding(.horizontal, Theme.inset).padding(.vertical, 16)
    }

    private var composer: some View {
        VStack(alignment: .leading, spacing: 10) {
            if !mod.steering.isEmpty {
                DisclosureGroup("Steering: waiting for workers (\(mod.steering.count))") {
                    ForEach(Array(mod.steering.enumerated()), id: \.offset) { _, text in
                        Text(text).frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled)
                    }
                }.font(Theme.font(11)).foregroundStyle(Theme.muted)
            }
            if session.workspace.queue == .open {
                AttachedQueueView(session: session, mod: mod)
            }
            VStack(alignment: .leading, spacing: 8) {
                TextEditor(text: Binding(get: { session.draft }, set: { session.editDraft($0) }))
                    .font(Theme.font(13)).frame(height: 62).scrollContentBackground(.hidden)
                    .accessibilityLabel("Instruction").focused($composerFocused).disabled(!session.canCompose)
                HStack(spacing: 8) {
                    if !mod.queue.isEmpty {
                        Button { session.workspace.toggleQueue() } label: {
                            Label("\(mod.queue.count) queued", systemImage: "list.number")
                        }.buttonStyle(WorkspaceButtonStyle(selected: session.workspace.queue == .open))
                            .disabled(!session.interactive).accessibilityLabel("Queued instructions (\(mod.queue.count))")
                    }
                    Spacer()
                    Text("⌘↩").font(Theme.font(11)).foregroundStyle(Theme.muted)
                    Button { session.submit(); composerFocused = true } label: {
                        Image(systemName: "arrow.up").font(.system(size: 13, weight: .semibold)).frame(width: 15, height: 15)
                    }.buttonStyle(WorkspaceButtonStyle(prominent: true))
                        .keyboardShortcut(.return, modifiers: .command).accessibilityLabel("Send instruction").help("Send instruction (⌘↩)")
                        .disabled(!session.canCompose || session.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }.padding(11).background(Theme.paper, in: RoundedRectangle(cornerRadius: Theme.radius))
                .overlay(RoundedRectangle(cornerRadius: Theme.radius).stroke(Theme.line))
        }.padding(.horizontal, Theme.inset).padding(.bottom, 16)
    }
}

extension EngineAction {
    var symbol: String {
        switch id {
        case "run", "reopen": "play"
        case "stop": "pause"
        case "retry", "retry_git", "retry_task", "revise_repair": "arrow.clockwise"
        case "fix_check", "fix_issues": "wrench.and.screwdriver"
        case "network": "network"
        case "diff": "doc.text.magnifyingglass"
        case "review", "findings": "checkmark.bubble"
        case "publish": "arrow.up.right"
        case "update": "arrow.triangle.2.circlepath"
        case "close": "archivebox"
        case "delete": "trash"
        default: "info.circle"
        }
    }
}
