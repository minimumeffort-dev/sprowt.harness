import AppKit
import SwiftUI
import Testing
@testable import SprowtDesktop

@Test @MainActor func workerSettingsFitSmallTeamsAndBoundLargeTeams() {
    let choice = ModelChoice(provider: "codex", model: "gpt-6.1-sol", effort: "high")
    func size(rows: Int) -> NSSize {
        let settings = WorkerSettings(planner: choice,
                                      executors: Array(repeating: WorkerAllocation(choice: choice, count: 1), count: rows),
                                      reviewer: choice)
        let view = NSHostingView(rootView: WorkerSettingsView(session: Session(),
                                                             draft: WorkerSettingsDraft(modId: nil, settings: settings)))
        return view.fittingSize
    }
    let small = size(rows: 2)
    let large = size(rows: 16)
    #expect(small.width == 640)
    #expect(small.height < 580)
    #expect(large.width == 640)
    #expect(large.height > small.height)
    #expect(large.height < 700)
}
