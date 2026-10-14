import AppKit
import SwiftUI
import Testing
@testable import SprowtDesktop

private let diagnostic = "Environment preflight failed.\nTraceback (most recent call last):\nValueError: Source file exceeds 64 MiB"

private func agentsSnapshot(previous: Bool = false, inspection: Bool = false) throws -> Snapshot {
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
                 "activity": "", "activity_preview": "", "previous_diagnostic": previous && index == 0 ? diagnostic : NSNull(),
                 "failure": index == 0 && !previous ? ["summary": "A source file exceeds the export limit.", "detail": diagnostic] : NSNull()]
            },
            "timings": [["label": "Dependency setup", "duration_ms": 316000, "duration": "5m 16s"],
                        ["label": "Worker slot wait", "duration_ms": 103000, "duration": "1m 43s"]]
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
    selection.agentDetails = ["failure:1", "worker:1", "timings", "timing-source"]
    selection.queue = .open
    selection.messageToggles = ["saved-prose"]
    selection.reconcile(try agentsSnapshot(previous: true))
    #expect(selection.tab == .agents)
    #expect(selection.target == .worker(1))
    #expect(selection.agentDetails == ["failure:1", "worker:1", "timings", "timing-source"])
    #expect(selection.queue == .open)
    #expect(selection.messageToggles == ["saved-prose"])
    #expect(snapshot.activeMod?.draft == "Saved draft")
    selection.reconcile(try decodeSnapshot { $0["project"] = "/another-project" })
    #expect(selection.agentDetails.isEmpty)
    #expect(selection.tab == .conversation)
}

@Test @MainActor func agentsFoldDiagnosticsAndTimingsWithoutTruncatingTheirEvidence() throws {
    let snapshot = try agentsSnapshot()
    let agents = try #require(snapshot.activeMod?.agents)
    func height(_ details: Set<String>) -> CGFloat {
        var selection = WorkspaceSelection()
        selection.agentDetails = details
        return NSHostingView(rootView: AgentsView(agents: agents, rawTimings: "Exact original timing evidence",
                                                  selection: .constant(selection)).frame(width: 800)).fittingSize.height
    }
    let folded = height([])
    #expect(height(["failure:1"]) > folded)
    #expect(height(["timings"]) > folded)
    #expect(height(["timings", "timing-source"]) > height(["timings"]))
    #expect(agents.workers[0].failure?.detail == diagnostic)
    #expect(agents.workers[0].worker.state == "ready")
    #expect(agents.workers[0].state == "Needs attention")
    #expect(agents.timings[0].durationMs == 316000)
    #expect(agents.timings[0].duration == "5m 16s")
}
