import Foundation
import Testing
@testable import SprowtDesktop

private let snapshotJSON = #"""
{"project":"/tmp/project","mods":[{"id":1,"name":"Example","closed":false}],"active_mod":{"id":1,"name":"Example","description":"Change the CLI","closed":false,"user_paused":false,"draft":"Saved draft","messages":[{"role":"user","body":"Change the CLI","model":null,"effort":null},{"role":"planner","body":"Ready","model":"codex","effort":"high"}],"history":[],"queue":[{"id":7,"body":"Keep tests"}],"steering":["Preserve the interface"],"plan":{"status":"ready","summary":"One task","tasks":[{"id":"implement_cli","title":"Change command","outcome":"CLI works","depends_on":["design"],"status":"pending","detail":"Ready","files":["src/main.rs"],"checks":["cargo test"]}]},"checks":[{"label":"cargo test","status":"passed","output":"3 passed"}],"findings":[{"id":"F1","title":"Handle EOF","detail":"Keep draft","status":"open"}],"workers":["codex executor running"]},"status":"Ready","detail":"Run plan","evidence":"3 checks","error":null,"actions":[{"id":"retry_task","label":"Retry task","task_id":12}],"dialog":null}
"""#

@Test func decodesGoldenFixtureProducedByRust() throws {
    var root = URL(fileURLWithPath: #filePath)
    for _ in 0..<4 { root.deleteLastPathComponent() }
    let response = try Envelope.decode(Data(contentsOf: root.appendingPathComponent("tests/fixtures/desktop.json")))
    #expect(response.snapshot.activeMod?.name == "Add native controls")
    #expect(response.snapshot.activeMod?.plan?.tasks.last?.dependsOn == ["core"])
    #expect(response.snapshot.activeMod?.messages.count == 1)
    #expect(response.snapshot.activeMod?.history.last?.role == "planner")
    #expect(response.snapshot.activeMod?.draft == "Do not lose this draft")
    #expect(response.snapshot.dialog?.kind == "delete")
    #expect(response.snapshot.dialog?.confirmLabel == "Delete codemod")
}

@Test func decodesRustProtocolAndEncodesCommands() throws {
    let data = Data("{\"protocol_version\":1,\"request_id\":42,\"error\":null,\"snapshot\":\(snapshotJSON)}".utf8)
    let envelope = try Envelope.decode(data)
    #expect(envelope.requestId == 42)
    #expect(envelope.snapshot.activeMod?.plan?.tasks.first?.id == "implement_cli")
    #expect(envelope.snapshot.activeMod?.plan?.tasks.first?.dependsOn == ["design"])
    #expect(envelope.snapshot.activeMod?.messages.last?.model == "codex")
    #expect(envelope.snapshot.activeMod?.queue.first?.body == "Keep tests")
    #expect(envelope.snapshot.actions.first?.taskId == 12)
    let command = Request(id: 43, command: Command(type: "action", modId: 1, action: "retry_task", taskId: 12))
    let encoded = try #require(JSONSerialization.jsonObject(with: command.encodedLine()) as? [String: Any])
    let payload = try #require(encoded["command"] as? [String: Any])
    #expect(payload["mod_id"] as? Int == 1)
    #expect(payload["task_id"] as? Int == 12)
    #expect(payload["action"] as? String == "retry_task")
    #expect(payload.count == 4)
}

@Test func rejectsIncompatibleProtocol() {
    let data = Data("{\"protocol_version\":2,\"request_id\":null,\"error\":null,\"snapshot\":\(snapshotJSON)}".utf8)
    #expect(throws: ProtocolError.self) { try Envelope.decode(data) }
}

@Test func repositoryEntryAndConfirmationKeepTheRustContract() throws {
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    let entry = try decoder.decode(EngineDialog.self, from: Data(#"{"kind":"repository","title":"Connect repository","body":"Enter owner/repository.","confirm_label":"Continue","request_id":null,"domains":[],"repository_slug":"owner/repo","repository_create":false}"#.utf8))
    #expect(entry.confirmLabel == "Continue")
    #expect(entry.repositorySlug == "owner/repo")
    let request = Request(id: 1, command: Command(type: "repository", modId: 7, slug: "owner/repo", create: false))
    let json = try #require(JSONSerialization.jsonObject(with: request.encodedLine()) as? [String: Any])
    let command = try #require(json["command"] as? [String: Any])
    #expect(command["slug"] as? String == "owner/repo")
    #expect(command["create"] as? Bool == false)
    let confirmation = try decoder.decode(EngineDialog.self, from: Data(#"{"kind":"repository_confirmation","title":"Connect repository","body":"Connect owner/repo","confirm_label":"Connect repository","request_id":null,"domains":[],"repository_slug":"owner/repo","repository_create":false}"#.utf8))
    #expect(confirmation.kind == "repository_confirmation")
    #expect(confirmation.confirmLabel == "Connect repository")
}

@Test func newerDraftSurvivesBackgroundSnapshotsAndOlderAcknowledgements() {
    var drafts = Drafts()
    drafts.receive(id: 1, text: "Saved")
    let first = drafts.edit("First edit", for: 1)
    _ = drafts.edit("Newer edit", for: 1)
    drafts.acknowledge(id: 1, revision: first)
    drafts.receive(id: 1, text: "First edit")
    #expect(drafts.text(for: 1, fallback: "") == "Newer edit")
    let restored = drafts.restoreSubmission(id: 1, revision: first, text: "Rejected submission")
    #expect(!restored)
    #expect(drafts.text(for: 1, fallback: "") == "Newer edit")
    drafts.receive(id: 2, text: "Other codemod")
    #expect(drafts.text(for: 2, fallback: "") == "Other codemod")
}

@Test @MainActor func sidecarDrainsStderrAndStopsCleanlyOnInputEOF() async throws {
    let directory = try temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let executable = directory.appendingPathComponent("engine")
    try installExecutable("""
    #!/bin/sh
    printf 'ready\\n'
    while IFS= read -r request; do printf '%s\\n' "$request"; done
    printf 'checkpoint complete' >&2
    printf saved > "$0.saved"
    """, at: executable)
    let sidecar = Sidecar()
    let events = try sidecar.start(project: directory, dataDirectory: nil, executable: executable)
    try sidecar.send(Request(id: 9, command: Command(type: "snapshot")))
    sidecar.closeInput()
    var lines: [String] = [], stderr = "", exitStatus: Int32?
    for await event in events {
        switch event {
        case .line(let data): lines.append(String(decoding: data, as: UTF8.self))
        case .stderr(let text): stderr += text
        case .failure(let text): Issue.record(Comment(rawValue: text))
        case .ended(let status):
            #expect(stderr == "checkpoint complete")
            exitStatus = status
        }
    }
    #expect(lines.first == "ready")
    let last = try #require(lines.last)
    let request = try #require(JSONSerialization.jsonObject(with: Data(last.utf8)) as? [String: Any])
    #expect(request["id"] as? Int == 9)
    #expect(exitStatus == 0)
    #expect(try String(contentsOfFile: executable.path + ".saved", encoding: .utf8) == "saved")
}

@Test @MainActor func brokenInputPipeReportsAnErrorWithoutTerminatingTheApp() async throws {
    let directory = try temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let executable = directory.appendingPathComponent("engine")
    try installExecutable("""
    #!/bin/sh
    exec 0<&-
    printf 'input closed\\n'
    /bin/sleep 0.2
    """, at: executable)
    let sidecar = Sidecar()
    let events = try sidecar.start(project: directory, dataDirectory: nil, executable: executable)
    var failure = ""
    for await event in events {
        switch event {
        case .line:
            try sidecar.send(Request(id: 1, command: Command(type: "snapshot")))
        case .failure(let message): failure = message
        default: break
        }
    }
    sidecar.closeInput()
    #expect(failure.hasPrefix("Could not send to the Rust engine:"))
}

@Test @MainActor func engineExitDismissesConfirmationAndKeepsRecoveryState() async throws {
    let directory = try temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let executable = directory.appendingPathComponent("engine")
    let dialog = #"{"kind":"delete","title":"Delete codemod","body":"Confirm deletion","confirm_label":"Delete","request_id":null,"domains":[],"repository_slug":null,"repository_create":false}"#
    let snapshot = snapshotJSON.replacingOccurrences(of: #""dialog":null"#, with: #""dialog":"# + dialog)
    let response = "{\"protocol_version\":1,\"request_id\":null,\"error\":null,\"snapshot\":\(snapshot)}"
    try Data(response.utf8).write(to: URL(fileURLWithPath: executable.path + ".json"))
    try installExecutable("""
    #!/bin/sh
    /bin/cat "$0.json"
    printf '\\n'
    IFS= read -r request
    printf 'Engine stopped during confirmation' >&2
    exit 12
    """, at: executable)
    let session = Session()
    defer { session.close {} }
    session.start(project: directory, executable: executable)
    try await waitUntil { session.dialog?.kind == "delete" }
    #expect(session.draft == "Saved draft")
    session.enqueue(Command(type: "snapshot"))
    try await waitUntil { session.connection == .disconnected }
    #expect(session.dialog == nil)
    #expect(session.snapshot?.dialog?.kind == "delete")
    #expect(session.draft == "Saved draft")
    #expect(session.error == "Engine stopped during confirmation")
}

@Test @MainActor func sessionPreservesRejectedAndNewerDraftsAndCancelledCreation() async throws {
    let directory = try temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let executable = directory.appendingPathComponent("engine")
    try Data(snapshotJSON.utf8).write(to: URL(fileURLWithPath: executable.path + ".json"))
    try installExecutable(#"""
    #!/usr/bin/python3
    import json, os, sys, time
    with open(sys.argv[0] + '.json') as f: snapshot = json.load(f)
    def respond(request_id=None, error=None):
        print(json.dumps(dict(protocol_version=1, request_id=request_id, error=error, snapshot=snapshot)), flush=True)
    respond()
    for line in sys.stdin:
        request = json.loads(line)
        command = request['command']
        kind = command['type']
        error = None
        if kind == 'set_draft': snapshot['active_mod']['draft'] = command['text']
        elif kind == 'submit':
            open(sys.argv[0] + '.submitted', 'w').close()
            while not os.path.exists(sys.argv[0] + '.release'): time.sleep(0.01)
            error = 'Submission rejected'
        elif kind == 'new_mod':
            snapshot['dialog'] = dict(kind='project_setup', title='Set up Git', body='Files', confirm_label='Continue', request_id=None, domains=[], repository_slug=None, repository_create=False)
        elif kind == 'cancel': snapshot['dialog'] = None
        elif kind == 'confirm':
            snapshot['dialog'] = None
            snapshot['active_mod']['id'] = 2
            snapshot['active_mod']['draft'] = ''
        respond(request['id'], error)
    print('checkpoint complete', file=sys.stderr)
    """#, at: executable)
    let session = Session()
    defer { session.close {} }
    session.start(project: directory, executable: executable)
    try await waitUntil { session.editable }
    session.editDraft("Keep this draft")
    try await waitUntil { session.active?.draft == "Keep this draft" }
    session.submit()
    try await waitUntil { FileManager.default.fileExists(atPath: executable.path + ".submitted") }
    try Data().write(to: URL(fileURLWithPath: executable.path + ".release"))
    try await waitUntil { session.error == "Submission rejected" && session.draft == "Keep this draft" }
    try await waitUntil { !session.busy }
    try FileManager.default.removeItem(atPath: executable.path + ".release")
    try FileManager.default.removeItem(atPath: executable.path + ".submitted")
    session.submit()
    try await waitUntil { FileManager.default.fileExists(atPath: executable.path + ".submitted") }
    session.editDraft("Typed while waiting")
    try Data().write(to: URL(fileURLWithPath: executable.path + ".release"))
    try await waitUntil { session.active?.draft == "Typed while waiting" }
    #expect(session.draft == "Typed while waiting")
    session.newDescription = "Create this codemod"
    session.showingNew = true
    session.create()
    try await waitUntil { session.snapshot?.dialog?.kind == "project_setup" }
    #expect(session.newDescription == "Create this codemod")
    #expect(session.showingNew)
    session.enqueue(Command(type: "cancel"))
    try await waitUntil { session.snapshot?.dialog == nil }
    #expect(session.newDescription == "Create this codemod")
    #expect(session.draft == "Typed while waiting")
    session.create()
    try await waitUntil { session.snapshot?.dialog?.kind == "project_setup" }
    session.enqueue(Command(type: "confirm", modId: 1, kind: "project_setup"))
    try await waitUntil { session.active?.id == 2 }
    #expect(session.newDescription == "")
    #expect(!session.showingNew)
    await withCheckedContinuation { continuation in session.close { continuation.resume() } }
    #expect(session.connection == .disconnected)
}

private func temporaryDirectory() throws -> URL {
    let url = FileManager.default.temporaryDirectory.appendingPathComponent("sprowt-desktop-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
    return url
}

private func installExecutable(_ source: String, at url: URL) throws {
    try source.write(to: url, atomically: true, encoding: .utf8)
    try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: url.path)
}

@MainActor private func waitUntil(line: UInt = #line, _ condition: () -> Bool) async throws {
    let deadline = ContinuousClock.now + .seconds(8)
    while !condition() {
        if ContinuousClock.now > deadline {
            throw NSError(domain: "DesktopTests.timeout", code: Int(line), userInfo: [NSLocalizedDescriptionKey: "Timed out at line \(line)"])
        }
        try await Task.sleep(for: .milliseconds(10))
    }
}
