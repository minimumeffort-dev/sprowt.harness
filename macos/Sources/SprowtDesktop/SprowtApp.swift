import AppKit
import SwiftUI

@main
struct SprowtApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @State private var session = Session()
    @State private var sidebarVisible = true

    init() { Theme.registerFonts() }

    var body: some Scene {
        Window("Sprowt", id: "main") {
            ContentView(session: session, sidebarVisible: $sidebarVisible)
                .frame(minWidth: 900, minHeight: 650)
                .task {
                    delegate.session = session
                    let arguments = ProcessInfo.processInfo.arguments
                    func argument(_ flag: String) -> String? {
                        guard let index = arguments.firstIndex(of: flag), arguments.indices.contains(index + 1) else { return nil }
                        return arguments[index + 1]
                    }
                    if let project = argument("--project") {
                        session.start(project: URL(fileURLWithPath: project), dataDirectory: argument("--data-dir"))
                    }
                }
        }
        .defaultSize(width: 1280, height: 860)
        .windowToolbarStyle(.unifiedCompact(showsTitle: false))
        .commands {
            CommandGroup(replacing: .sidebar) {
                Button(sidebarVisible ? "Hide Sidebar" : "Show Sidebar") { sidebarVisible.toggle() }
                    .keyboardShortcut("s", modifiers: [.control, .command])
            }
            CommandGroup(after: .sidebar) {
                Button("Back") { session.navigate(back: true) }
                    .keyboardShortcut("[").disabled(!session.canGoBack)
                Button("Forward") { session.navigate(back: false) }
                    .keyboardShortcut("]").disabled(!session.canGoForward)
                Divider()
                ForEach(Array(WorkspaceTab.allCases.enumerated()), id: \.element.rawValue) { index, tab in
                    Button(tab.label) { session.workspace.show(tab) }
                        .keyboardShortcut(KeyEquivalent(Character("\(index + 1)")), modifiers: .command)
                        .disabled(session.active == nil)
                }
            }
            CommandGroup(replacing: .newItem) {
                Button("New codemod") { session.beginCreation() }
                    .keyboardShortcut("n")
                    .disabled(!session.canBeginCreation)
                Button("Open project…") { session.chooseProject() }
                    .keyboardShortcut("o")
                    .disabled(session.connection == .stopping)
            }
        }
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    var session: Session?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        NSApp.activate()
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard let session, session.connection != .disconnected else { return .terminateNow }
        session.close { sender.reply(toApplicationShouldTerminate: true) }
        return .terminateLater
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}
