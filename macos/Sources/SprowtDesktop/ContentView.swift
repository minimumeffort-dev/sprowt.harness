import SwiftUI

struct ContentView: View {
    @Bindable var session: Session
    @Binding var sidebarVisible: Bool

    var body: some View {
        HStack(spacing: 0) {
            NavigationRail(session: session, sidebarVisible: $sidebarVisible)
            HSplitView {
                // Keep both panes mounted to preserve native editor focus and undo.
                SidebarView(session: session)
                    .frame(minWidth: sidebarVisible ? 210 : 0,
                           idealWidth: sidebarVisible ? 242 : 0,
                           maxWidth: sidebarVisible ? 320 : 0)
                    .clipped().allowsHitTesting(sidebarVisible).disabled(!sidebarVisible)
                    .accessibilityHidden(!sidebarVisible)
                VStack(spacing: 0) {
                    if let error = session.error ?? session.snapshot?.error {
                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                            Image(systemName: "exclamationmark.triangle").symbolBaseline(textSize: 12)
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
        }
        .toolbar {
            ToolbarItemGroup(placement: .navigation) {
                Button { session.navigate(back: true) } label: { Image(systemName: "chevron.left") }
                    .disabled(!session.canGoBack).help("Back (⌘[)").accessibilityLabel("Back")
                Button { session.navigate(back: false) } label: { Image(systemName: "chevron.right") }
                    .disabled(!session.canGoForward).help("Forward (⌘])").accessibilityLabel("Forward")
                Button { sidebarVisible.toggle() } label: {
                    Image(systemName: "sidebar.left")
                }.help("Toggle sidebar (⌃⌘S)").accessibilityLabel("Toggle sidebar")
            }
        }
        .toolbarBackground(Theme.sidebar, for: .windowToolbar)
        .toolbarBackground(.visible, for: .windowToolbar)
        .font(Theme.font()).foregroundStyle(Theme.ink).tint(Theme.accent)
        .background(Theme.paper)
        .sheet(item: Binding(get: { session.dialog.flatMap { $0.isInspection ? nil : $0 } }, set: { _ in })) { dialog in
            EngineDialogView(session: session, dialog: dialog).interactiveDismissDisabled()
        }
    }
}
