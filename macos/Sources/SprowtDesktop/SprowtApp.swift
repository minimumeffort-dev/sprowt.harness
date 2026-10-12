import AppKit
import SwiftUI

@main
struct SprowtApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @State private var session = Session()

    var body: some Scene {
        Window("Sprowt", id: "main") {
            ContentView(session: session)
                .frame(minWidth: 840, minHeight: 600)
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
        .defaultSize(width: 1100, height: 780)
        .commands {
            CommandGroup(replacing: .newItem) {
                Button("New codemod") { session.showingNew = true }
                    .keyboardShortcut("n")
                    .disabled(!session.editable)
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
