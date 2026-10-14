import AppKit
import Foundation
import SwiftUI
import Testing
@testable import SprowtDesktop

@Test @MainActor func readOnlyMessagesKeepParagraphsListsAndLiteralCode() {
    let source = "# Restore attachments\n\nKeep **task data** and `taskId`.\n- Save café 👩🏽‍💻\n- [ ] Reopen\n```js\nconst text = \"**literal**\";\n  save(text);\n```\n```\nsecond block\n```\nLinks [stay literal](https://example.com)."
    let blocks = MessageBlock.parse(source)
    #expect(blocks.map(\.text.string) == [
        "Restore attachments", "", "Keep task data and taskId.", "Save café 👩🏽‍💻", "Reopen",
        "const text = \"**literal**\";\n  save(text);", "second block", "Links [stay literal](https://example.com)."
    ])
    #expect(blocks.map(\.kind) == ["#", "", "", "-", "- [ ]", "code:js", "code:", ""])
    #expect(blocks.compactMap(\.marker) == ["• ", "☐ "])
    let runs = Array(blocks[2].styled.runs)
    #expect(runs.map { String(blocks[2].styled[$0.range].characters) } == ["Keep ", "task data", " and ", "taskId", "."])
    #expect(runs[3].foregroundColor == Theme.accent)
    #expect(runs[3].backgroundColor == Theme.codeBackground)
    #expect(runs[1].font != runs[0].font)
    #expect(ComposerMarkdown.encode(ComposerMarkdown.decode(source)) == source)
}

@Test @MainActor func readOnlyMessagesPreserveUnsupportedAndUnclosedSyntax() {
    let blocks = MessageBlock.parse("Use ~~raw~~ and [a link](url).\n```swift\nlet x = 1\n\n")
    #expect(blocks.map(\.text.string) == ["Use ~~raw~~ and [a link](url).", "```swift", "let x = 1", ""])
    #expect(blocks.map(\.kind) == ["", "", "", ""])
}

@Test func onlyTheMatchingCurrentPreparationOwnsTheTaskFailure() {
    func task(error: String, receipt: String, current: Bool) -> PlanTask {
        PlanTask(id: "storage", title: "Save files", outcome: "Keep data", dependsOn: [], status: "blocked", detail: error,
                 files: [], checks: [], runId: 1, identity: "Codex", source: "source", error: error, latest: nil,
                 results: [], checksLabel: "Checks", completed: false,
                 preparation: PreparationEvidence(source: "source", current: current, stopped: false, error: receipt, results: []), actions: [])
    }
    let receipt = "Capability unavailable: browser storage\nError: OPFS round-trip failed"
    #expect(task(error: "Environment preflight failed: \(receipt)", receipt: receipt, current: true).preparationOwnsError)
    #expect(task(error: receipt, receipt: receipt, current: true).preparationOwnsError)
    #expect(!task(error: "Environment preflight failed: \(receipt)", receipt: receipt, current: false).preparationOwnsError)
    #expect(!task(error: "Worker stopped unexpectedly", receipt: receipt, current: true).preparationOwnsError)
}
