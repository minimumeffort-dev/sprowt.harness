import Foundation
import SwiftUI
import Testing
@testable import SprowtDesktop

@Test @MainActor func rustMessageSegmentsDriveNativeSourceAndSyntaxWithoutChangingText() throws {
    var root = URL(fileURLWithPath: #filePath)
    for _ in 0..<4 { root.deleteLastPathComponent() }
    let response = try Envelope.decode(Data(contentsOf: root.appendingPathComponent("tests/fixtures/desktop.json")))
    let message = try #require(response.snapshot.activeMod?.history.first(where: { $0.role == "planner" }))
    #expect(message.content.segments.count == 3)
    guard case .code(let source, let language, let summary, let collapsed, let tokens) = message.content.segments[1].content else {
        Issue.record("Rust JSON block did not decode as source")
        return
    }
    #expect(language == "JSON")
    #expect(summary == "JSON · 1 line · 55 B")
    #expect(collapsed)
    #expect(source == "{\"path\": \"src/engine.rs\", \"checks\": 3, \"ready\": false}\n")
    #expect(tokens.map(\.text).joined() == source)
    let styled = codeText(source, tokens: tokens)
    #expect(String(styled.characters) == source)
    let key = try #require(styled.runs.first(where: { String(styled[$0.range].characters) == "\"path\"" }))
    #expect(key.foregroundColor == Theme.blue)
    let string = try #require(styled.runs.first(where: { String(styled[$0.range].characters) == "\"src/engine.rs\"" }))
    #expect(string.foregroundColor == Theme.accent)
    #expect(message.body == "I will split this into two tasks.\n\n\(source)\nKeep the existing source.")
}

@Test @MainActor func literalCodePreservesMalformedJsonAndUnicode() {
    let source = "  {\"a\": \"unterminated 界 👩🏽‍💻\\\n"
    #expect(String(codeText(source, tokens: []).characters) == source)
}

@Test func messageExpansionIsLocalToItsCodemodAndSurvivesSnapshotRefreshes() throws {
    let snapshot = try decodeSnapshot()
    var workspace = WorkspaceSelection()
    workspace.reconcile(snapshot)
    workspace.messageToggles.insert("user-one:0")
    workspace.reconcile(snapshot)
    #expect(workspace.messageToggles == ["user-one:0"])
    workspace.show(.checks)
    workspace.show(.conversation)
    #expect(workspace.messageToggles == ["user-one:0"])
    let another = try decodeSnapshot { root in
        var mod = root["active_mod"] as! [String: Any]
        mod["id"] = 2
        root["active_mod"] = mod
    }
    workspace.reconcile(another)
    #expect(workspace.messageToggles.isEmpty)
    #expect(another.activeMod?.draft == "Saved draft")
}
