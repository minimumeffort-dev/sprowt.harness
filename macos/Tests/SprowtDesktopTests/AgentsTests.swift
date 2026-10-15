import AppKit
import SwiftUI
import Testing
@testable import SprowtDesktop

private let diagnostic = "Environment preflight failed.\nTraceback (most recent call last):\nValueError: Source file exceeds 64 MiB"

private func agentsSnapshot(previous: Bool = false, inspection: Bool = false, output: String = diagnostic) throws -> Snapshot {
    try decodeSnapshot { root in
        root["error"] = diagnostic
        root["workspace_views"] = ["conversation", "checks", "agents"]
        if inspection {
            root["dialog"] = ["kind": "failure", "title": "Inspect failure", "body": "Other task failure", "confirm_label": NSNull(), "request_id": NSNull(), "domains": [], "repository_slug": NSNull(), "repository_create": false] as [String: Any]
        }
        var mod = root["active_mod"] as! [String: Any]
        let workers: [[String: Any]] = (1...3).map { id in
            ["id": id, "provider": "codex", "identity": "codex · executor · gpt-6.1-sol · xhigh", "role": "executor",
             "task_id": "task-\(id)", "state": id == 1 ? "ready" : "running", "activity": ""]
        }
        mod["worker_details"] = workers
        mod["agents"] = [
            "summary": "Task 1 needs attention · 2 workers running",
            "workers": workers.enumerated().map { index, worker -> [String: Any] in
                ["worker": worker, "task_number": index + 1, "title": "Task \(index + 1)",
                 "state": index == 0 ? "Needs attention" : "Working", "tone": index == 0 ? "attention" : "working",
                 "activity": "", "activity_preview": "Checking the existing baseline",
                 "worker_label": "Codex · w\(index + 1)", "model": "gpt-6.1-sol", "reasoning": "Extra high", "output": output,
                 "previous_diagnostic": previous && index == 0 ? diagnostic : NSNull(),
                 "failure": index == 0 && !previous ? ["summary": "A source file exceeds the export limit.", "detail": diagnostic] : NSNull()]
            },
            "timings": [["label": "Dependency setup", "duration_ms": 316000, "duration": "5m 16s", "relative_duration": 1.0],
                        ["label": "Worker slot wait", "duration_ms": 103000, "duration": "1m 43s", "relative_duration": 103.0 / 316],
                        ["label": "Provider connection", "duration_ms": 31000, "duration": "31.0s", "relative_duration": 31.0 / 316],
                        ["label": "VM boot", "duration_ms": 4200, "duration": "4.2s", "relative_duration": 4.2 / 316]]
        ]
        root["active_mod"] = mod
    }
}

@Test func agentsOwnOnlyTheCurrentFailureInTheVisibleWorkerRows() throws {
    let snapshot = try agentsSnapshot()
    var selection = WorkspaceSelection()
    selection.reconcile(snapshot)
    selection.show(.agents)
    #expect(selection.ownsAgentError(diagnostic, in: snapshot))
    #expect(!selection.ownsAgentError("Unable to save the project database.", in: snapshot))
    selection.show(.agents, target: .worker(2))
    #expect(!selection.ownsAgentError(diagnostic, in: snapshot))
    selection.show(.agents, target: .worker(1))
    #expect(selection.ownsAgentError(diagnostic, in: snapshot))
    #expect(!selection.ownsAgentError(diagnostic, in: try agentsSnapshot(previous: true)))
    #expect(!selection.ownsAgentError(diagnostic, in: try agentsSnapshot(inspection: true)))
    selection.show(.checks)
    #expect(!selection.ownsAgentError(diagnostic, in: snapshot))
}

@Test func agentsRefreshPreservesDisclosuresQueueAndDraftUntilContextChanges() throws {
    let snapshot = try agentsSnapshot()
    var selection = WorkspaceSelection()
    selection.reconcile(snapshot)
    selection.show(.agents, target: .worker(1))
    #expect(selection.agentDetails.isEmpty)
    selection.agentDetails = ["activity-hidden:1", "all-timings"]
    selection.queue = .open
    selection.messageToggles = ["saved-prose"]
    selection.reconcile(try agentsSnapshot(previous: true))
    #expect(selection.tab == .agents)
    #expect(selection.target == .worker(1))
    #expect(selection.agentDetails == ["activity-hidden:1", "all-timings"])
    #expect(selection.queue == .open)
    #expect(selection.messageToggles == ["saved-prose"])
    #expect(snapshot.activeMod?.draft == "Saved draft")
    selection.reconcile(try decodeSnapshot { $0["project"] = "/another-project" })
    #expect(selection.agentDetails.isEmpty)
    #expect(selection.tab == .conversation)
}

@Test @MainActor func agentsShowActivityAndLongestTimingsByDefault() throws {
    let snapshot = try agentsSnapshot()
    let agents = try #require(snapshot.activeMod?.agents)
    func size(_ details: Set<String>, width: CGFloat = 780) -> NSSize {
        var selection = WorkspaceSelection()
        selection.agentDetails = details
        return NSHostingView(rootView: AgentsView(agents: agents, selection: .constant(selection), selectedWorker: 1)
            .frame(width: width)).fittingSize
    }
    let initial = size([])
    #expect(size(["activity-hidden:1"]).height < initial.height)
    #expect(size(["all-timings"]).height > initial.height)
    #expect(size([], width: 480).width == 480)
    #expect(agents.workers[0].failure?.detail == diagnostic)
    #expect(agents.workers[0].output == diagnostic)
    #expect(agents.workers[0].worker.state == "ready")
    #expect(agents.workers[0].state == "Needs attention")
    #expect(agents.workers[0].workerLabel == "Codex · w1")
    #expect(agents.workers[0].model == "gpt-6.1-sol")
    #expect(agents.workers[0].reasoning == "Extra high")
    #expect(agents.timings[0].durationMs == 316000)
    #expect(agents.timings[0].duration == "5m 16s")
    #expect(agents.timings[0].relativeDuration == 1)
}

@Test @MainActor func agentsBoundLongOutputWithoutDiscardingIt() throws {
    let output = Array(repeating: "A long line of complete diagnostic output", count: 200).joined(separator: "\n")
    let snapshot = try agentsSnapshot(output: output)
    let agents = try #require(snapshot.activeMod?.agents)
    func height(hidden: Bool) -> CGFloat {
        var selection = WorkspaceSelection()
        if hidden { selection.agentDetails.insert("activity-hidden:1") }
        return NSHostingView(rootView: AgentsView(agents: agents, selection: .constant(selection), selectedWorker: 1)
            .frame(width: 480)).fittingSize.height
    }
    let consoleHeight = height(hidden: false) - height(hidden: true)
    #expect(consoleHeight == 320)
    #expect(agents.workers[0].output == output)
}

@Test @MainActor func agentsNativeConsoleCopiesRawTextWithoutLineNumbers() {
    let output = "    first café 👩🏽‍💻\n\n\t第二行\n"
    let scroll = AgentConsoleScrollView()
    scroll.outputView.load(output)
    scroll.setFrameSize(scroll.fittingSize(width: 480))
    scroll.layoutSubtreeIfNeeded()
    scroll.outputView.setSelectedRange(NSRange(location: 0, length: output.utf16.count))
    let pasteboard = NSPasteboard.withUniqueName()
    defer { pasteboard.releaseGlobally() }
    #expect(scroll.outputView.writeSelection(to: pasteboard, types: scroll.outputView.writablePasteboardTypes))
    #expect(pasteboard.string(forType: .string) == output)
    #expect(scroll.outputView.accessibilityValue() == output)
}

@Test @MainActor func agentsNativeConsolePreservesUnicodeSelectionAndScrollOnRefresh() {
    let output = Array(repeating: "    café 👩🏽‍💻\n", count: 100).joined() + "終わり🙂"
    let scroll = AgentConsoleScrollView()
    scroll.setFrameSize(NSSize(width: 480, height: 320))
    scroll.outputView.load(output)
    #expect(scroll.fittingSize(width: 480).height == 320)
    scroll.layoutSubtreeIfNeeded()
    let selected = (output as NSString).range(of: "café 👩🏽‍💻")
    scroll.outputView.setSelectedRange(selected)
    scroll.contentView.scroll(to: NSPoint(x: 0, y: 80))
    let offset = scroll.contentView.bounds.origin
    scroll.outputView.load(output)
    _ = scroll.fittingSize(width: 480)
    #expect(scroll.outputView.selectedRange() == selected)
    #expect(scroll.contentView.bounds.origin == offset)
    let pasteboard = NSPasteboard.withUniqueName()
    defer { pasteboard.releaseGlobally() }
    #expect(scroll.outputView.writeSelection(to: pasteboard, types: scroll.outputView.writablePasteboardTypes))
    #expect(pasteboard.string(forType: .string) == "café 👩🏽‍💻")
}

@Test @MainActor func agentsNativeConsoleStartsAtTopAfterInitialSizing() {
    for output in ["Preflight · baseline", Array(repeating: "Diagnostic output", count: 100).joined(separator: "\n")] {
        let scroll = AgentConsoleScrollView()
        scroll.outputView.load(output)
        let size = scroll.fittingSize(width: 480)
        scroll.setFrameSize(size)
        scroll.layoutSubtreeIfNeeded()
        #expect(scroll.contentView.bounds.origin.y == scroll.outputView.frame.origin.y)
        #expect(scroll.outputView.frame.height == scroll.outputView.measuredHeight(width: 480))
        #expect(scroll.outputView.frame.width == 480)
    }
}

@Test @MainActor func agentsNativeConsoleMeasuresWithoutResizingItsViewport() {
    let output = Array(repeating: "    " + String(repeating: "diagnostic output ", count: 20), count: 30).joined(separator: "\n")
    let scroll = AgentConsoleScrollView()
    scroll.outputView.load(output)
    for width: CGFloat in [780, 602, 780] { _ = scroll.fittingSize(width: width) }
    scroll.setFrameSize(NSSize(width: 602, height: 320))
    scroll.layoutSubtreeIfNeeded()
    let documentSize = scroll.outputView.frame.size
    let container = scroll.outputView.textContainer!
    let layout = scroll.outputView.layoutManager!
    layout.ensureLayout(for: container)
    #expect(documentSize.width == 602)
    #expect(container.size.width == 602 - scroll.outputView.textContainerInset.width * 2)
    #expect(layout.usedRect(for: container).maxX <= container.size.width)
    #expect(documentSize.height == scroll.outputView.measuredHeight(width: 602))
    for width: CGFloat in [780, 602, 780] { _ = scroll.fittingSize(width: width) }
    #expect(scroll.outputView.frame.size == documentSize)
    #expect(container.size.width == 602 - scroll.outputView.textContainerInset.width * 2)
}
