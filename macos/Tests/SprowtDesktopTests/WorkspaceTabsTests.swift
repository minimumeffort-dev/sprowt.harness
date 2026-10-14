import Foundation
import Testing
@testable import SprowtDesktop

@Test func disappearingEvidenceReturnsToConversationWithoutClearingLocalDraftState() throws {
    let original = try decodeSnapshot()
    let empty = try decodeSnapshot { root in
        root["workspace_views"] = ["conversation"]
        var mod = root["active_mod"] as! [String: Any]
        mod["checks"] = []
        mod["findings"] = []
        mod["pr_feedback"] = []
        mod["worker_details"] = []
        root["active_mod"] = mod
    }
    for tab in WorkspaceTab.allCases.dropFirst() {
        var selection = WorkspaceSelection()
        selection.reconcile(original)
        selection.show(tab, target: .task("implement_cli"))
        selection.queue = .open
        selection.messageToggles = ["expanded-message"]
        selection.reconcile(empty)
        #expect(selection.tab == .conversation)
        #expect(selection.target == nil)
        #expect(selection.queue == .open)
        #expect(selection.messageToggles == ["expanded-message"])
        #expect(empty.activeMod?.draft == "Saved draft")
        selection.reconcile(original)
        #expect(selection.tab == .conversation)
        selection.show(tab)
        selection.reconcile(original)
        #expect(selection.tab == tab)
    }
}

@Test func explicitEmptyDiffKeepsInspectionAndConversationWithoutAnEmptyTab() throws {
    let snapshot = try decodeSnapshot { root in
        root["workspace_views"] = ["conversation"]
        root["dialog"] = ["kind": "diff", "title": "Review changes", "body": "", "confirm_label": NSNull(), "request_id": NSNull(), "domains": [], "repository_slug": NSNull(), "repository_create": false] as [String: Any]
    }
    var selection = WorkspaceSelection()
    selection.reconcile(snapshot)
    #expect(selection.tab == .conversation)
    #expect(snapshot.workspaceViews == [.conversation])
    #expect(snapshot.dialog?.isInspection == true)
    #expect(snapshot.activeMod?.draft == "Saved draft")
}

@Test func runningCheckActivityDecodesWithoutSavedResults() throws {
    let snapshot = try decodeSnapshot { root in
        root["workspace_views"] = ["conversation", "checks"]
        var mod = root["active_mod"] as! [String: Any]
        mod["checks"] = []
        mod["check_activity"] = [["task_id": "implement_cli", "label": "Task 1 checks are running."]]
        root["active_mod"] = mod
    }
    #expect(snapshot.activeMod?.checkActivity.first?.taskId == "implement_cli")
    #expect(snapshot.activeMod?.checkActivity.first?.label == "Task 1 checks are running.")
    var selection = WorkspaceSelection()
    selection.reconcile(snapshot)
    selection.show(.checks, target: .task("implement_cli"))
    selection.reconcile(snapshot)
    #expect(selection.tab == .checks)
    #expect(selection.target == .task("implement_cli"))
}
