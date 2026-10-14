import AppKit
import Foundation
import SwiftUI
import Testing
@testable import SprowtDesktop

private let snapshotJSON = #"""
{"project":"/tmp/project","navigation":{"destination":{"kind":"codemod","mod_id":1},"can_go_back":false,"can_go_forward":false},"phase":"building","mods":[{"id":1,"name":"Example","closed":false,"status":"Building","workers":[]}],"active_mod":{"id":1,"name":"Example","description":"Change the CLI","closed":false,"user_paused":false,"draft":"Saved draft","messages":[{"role":"user","body":"Change the CLI","model":null,"effort":null,"author":"user"},{"role":"planner","body":"Ready","model":"codex","effort":"high","author":"Codex · planner"}],"history":[],"queue":[{"id":7,"body":"Keep tests"}],"steering":["Preserve the interface"],"plan":{"status":"ready","summary":"One task","tasks":[{"id":"implement_cli","title":"Change command","outcome":"CLI works","depends_on":["design"],"status":"pending","detail":"Ready","files":["src/main.rs"],"checks":["cargo test"],"run_id":12,"identity":"codex executor","source":"task:12:attempt:1","error":null,"latest":null,"results":[],"checks_label":"Checks","completed":false,"preparation":null,"actions":[]}]},"checks":[{"id":"task:12:check:0","task_id":"implement_cli","kind":"check","label":"cargo test","status":"passed","output":"3 passed"}],"findings":[{"id":"F1","title":"Handle EOF","detail":"Keep draft","status":"open","imported":false}],"pr_feedback":[{"id":"thread:30","title":"src/main.rs:42 · rev","detail":"This drops the error.","status":"unresolved thread","imported":false}],"pr_feedback_error":null,"workers":["codex executor running"],"worker_details":[{"id":4,"provider":"codex","identity":"codex executor","role":"executor","task_id":"implement_cli","state":"running","activity":"Running cargo test"}],"timings":""},"status":"Ready","detail":"Run plan","evidence":"3 checks","error":null,"actions":[{"id":"retry_task","label":"Retry task","task_id":12}],"dialog":null,"workspace_views":["conversation","changes","checks","findings","agents"]}
"""#

@Test func decodesGoldenFixtureProducedByRust() throws {
    var root = URL(fileURLWithPath: #filePath)
    for _ in 0..<4 { root.deleteLastPathComponent() }
    let response = try Envelope.decode(Data(contentsOf: root.appendingPathComponent("tests/fixtures/desktop.json")))
    #expect(response.snapshot.activeMod?.name == "Add native controls")
    #expect(response.snapshot.activeMod?.plan?.tasks.last?.dependsOn == ["core"])
    #expect(response.snapshot.workspaceViews == WorkspaceTab.allCases)
    #expect(response.snapshot.activeMod?.messages.count == 1)
    #expect(response.snapshot.activeMod?.history.count == 2)
    #expect(response.snapshot.activeMod?.history.contains(where: { $0.role == "user" }) == false)
    #expect(response.snapshot.activeMod?.history.first?.author == "Codex · planner")
    #expect(response.snapshot.activeMod?.history.contains(where: { $0.role == "planner" }) == true)
    #expect(response.snapshot.activeMod?.plan?.tasks.first?.completed == true)
    #expect(response.snapshot.activeMod?.plan?.tasks.last?.latest?.body == "Native controls are ready; draft persistence needs repair.")
    #expect(response.snapshot.activeMod?.plan?.tasks.last?.results.first?.kind == .finalCheck)
    #expect(response.snapshot.activeMod?.draft == "Do not lose this draft")
    #expect(response.snapshot.actions.first?.id == "retry_task")
    #expect(response.snapshot.dialog?.kind == "delete")
    #expect(response.snapshot.dialog?.confirmLabel == "Delete codemod")
    #expect(response.snapshot.navigation.destination == .codemod(1))
    #expect(!response.snapshot.navigation.canGoBack)
}

@Test @MainActor func navigationAcknowledgementsKeepDraftsFiltersAndBusyGuardsTogether() async throws {
    let directory = try temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let executable = directory.appendingPathComponent("engine")
    try Data(snapshotJSON.utf8).write(to: URL(fileURLWithPath: executable.path + ".json"))
    try installExecutable(#"""
    #!/usr/bin/python3
    import copy, json, os, sys, time
    with open(sys.argv[0] + '.json') as f: snapshot = json.load(f)
    first = copy.deepcopy(snapshot['active_mod'])
    second = copy.deepcopy(first)
    second.update(id=2, closed=True, draft='Saved closed draft')
    details = {1:first, 2:second}
    history = [dict(kind='codemod', mod_id=1)]
    cursor = 0
    def respond(request_id=None, error=None):
        snapshot['navigation'] = dict(destination=history[cursor], can_go_back=cursor > 0, can_go_forward=cursor + 1 < len(history))
        if history[cursor]['kind'] == 'codemod': snapshot['active_mod'] = details[history[cursor]['mod_id']]
        print(json.dumps(dict(protocol_version=1, request_id=request_id, error=error, snapshot=snapshot)), flush=True)
    respond()
    for line in sys.stdin:
        request = json.loads(line)
        command = request['command']
        kind = command['type']
        with open(sys.argv[0] + '.commands', 'a') as f: f.write(json.dumps(command) + '\n')
        error = None
        if kind == 'set_draft':
            open(sys.argv[0] + '.draft_entered', 'w').close()
            while not os.path.exists(sys.argv[0] + '.draft_release'): time.sleep(0.01)
            details[command['mod_id']]['draft'] = command['text']
        elif kind in ['begin_creation', 'select_mod']:
            destination = dict(kind='new_codemod') if kind == 'begin_creation' else dict(kind='codemod', mod_id=command['mod_id'])
            if destination != history[cursor]:
                history = history[:cursor+1] + [destination]
                cursor += 1
        elif kind in ['go_back', 'go_forward']:
            if os.path.exists(sys.argv[0] + '.reject'): error = 'Navigation rejected'
            else:
                open(sys.argv[0] + '.nav_entered', 'w').close()
                while not os.path.exists(sys.argv[0] + '.nav_release'): time.sleep(0.01)
                cursor += -1 if kind == 'go_back' else 1
        respond(request['id'], error)
    """#, at: executable)
    let session = Session()
    defer { session.close {} }
    session.start(project: directory, executable: executable)
    try await waitUntil { session.canCompose }
    session.editDraft("Keep first draft")
    try await waitUntil { FileManager.default.fileExists(atPath: executable.path + ".draft_entered") }
    session.newDescription = "Unsent **creation**"
    session.beginCreation()
    #expect(session.busy)
    #expect(!session.canCompose && !session.canBeginCreation)
    session.editDraft("Must not reach the old codemod")
    session.select(2)
    try Data().write(to: URL(fileURLWithPath: executable.path + ".draft_release"))
    try await waitUntil { session.showingNew && !session.busy }
    #expect(session.draft == "Keep first draft")
    session.navigate(back: true)
    try await waitUntil { FileManager.default.fileExists(atPath: executable.path + ".nav_entered") }
    #expect(!session.canGoBack && !session.canGoForward && !session.canCompose)
    session.editDraft("Must not reach the destination")
    try Data().write(to: URL(fileURLWithPath: executable.path + ".nav_release"))
    try await waitUntil { !session.showingNew && !session.busy }
    #expect(session.draft == "Keep first draft")
    #expect(session.canGoForward)
    session.navigate(back: false)
    try await waitUntil { session.showingNew && !session.busy }
    #expect(session.newDescription == "Unsent **creation**")
    session.select(2)
    try await waitUntil { session.active?.id == 2 && !session.busy }
    #expect(session.showClosed && !session.canCompose)
    #expect(session.draft == "Saved closed draft")
    session.navigate(back: true)
    try await waitUntil { session.showingNew && !session.busy }
    #expect(!session.showClosed)
    session.navigate(back: true)
    try await waitUntil { !session.showingNew && session.active?.id == 1 && !session.busy }
    #expect(!session.showClosed)
    #expect(session.draft == "Keep first draft")
    session.navigate(back: false)
    try await waitUntil { session.showingNew && !session.busy }
    session.navigate(back: false)
    try await waitUntil { session.active?.id == 2 && !session.busy }
    #expect(session.showClosed)
    try Data().write(to: URL(fileURLWithPath: executable.path + ".reject"))
    session.navigate(back: true)
    try await waitUntil { session.error == "Navigation rejected" && !session.busy }
    #expect(session.active?.id == 2 && session.showClosed)
    #expect(!session.showingNew)
    #expect(session.newDescription == "Unsent **creation**")
    let commands = try String(contentsOfFile: executable.path + ".commands", encoding: .utf8)
    #expect(!commands.contains("Must not reach"))
    #expect(commands.components(separatedBy: "\n").filter { $0.contains("select_mod") }.count == 1)
}

@Test func decodesRustProtocolAndEncodesCommands() throws {
    let data = Data("{\"protocol_version\":1,\"request_id\":42,\"error\":null,\"snapshot\":\(snapshotJSON)}".utf8)
    let envelope = try Envelope.decode(data)
    #expect(envelope.requestId == 42)
    #expect(envelope.snapshot.activeMod?.plan?.tasks.first?.id == "implement_cli")
    #expect(envelope.snapshot.activeMod?.plan?.tasks.first?.dependsOn == ["design"])
    #expect(envelope.snapshot.activeMod?.messages.last?.model == "codex")
    #expect(envelope.snapshot.activeMod?.queue.first?.body == "Keep tests")
    #expect(envelope.snapshot.activeMod?.prFeedback.first?.id == "thread:30")
    #expect(envelope.snapshot.activeMod?.prFeedback.first?.imported == false)
    let fix = Request(id: 44, command: Command(type: "fix_pr_feedback", modId: 1, feedbackIds: ["thread:30"]))
    let fixJSON = try #require(JSONSerialization.jsonObject(with: fix.encodedLine()) as? [String: Any])
    let fixPayload = try #require(fixJSON["command"] as? [String: Any])
    #expect(fixPayload["feedback_ids"] as? [String] == ["thread:30"])
    #expect(envelope.snapshot.activeMod?.prFeedback.first?.status == "unresolved thread")
    #expect(envelope.snapshot.activeMod?.prFeedbackError == nil)
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
        if snapshot.get('dialog') and snapshot['dialog']['kind'] in ['diff', 'failure', 'findings', 'sync_details']: snapshot['navigation']['destination'] = dict(kind='codemod', mod_id=snapshot['active_mod']['id'])
        print(json.dumps(dict(protocol_version=1, request_id=request_id, error=error, snapshot=snapshot)), flush=True)
    respond()
    for line in sys.stdin:
        request = json.loads(line)
        if request['command']['type'] == 'begin_creation': snapshot['navigation']['destination'] = dict(kind='new_codemod')
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
            snapshot['navigation']['destination'] = dict(kind='codemod', mod_id=2)
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
    session.beginCreation()
    try await waitUntil { session.showingNew }
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

@Test @MainActor func projectMenuViewsSeparateClosedWorkAndNewCreationKeepsBothDrafts() async throws {
    let directory = try temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let executable = directory.appendingPathComponent("engine")
    var snapshot = try #require(JSONSerialization.jsonObject(with: Data(snapshotJSON.utf8)) as? [String: Any])
    snapshot["mods"] = [
        ["id": 1, "name": "Active change", "closed": false, "status": "Planning", "workers": []],
        ["id": 2, "name": "Saved change", "closed": true, "status": "Closed", "workers": []]
    ]
    try JSONSerialization.data(withJSONObject: snapshot).write(to: URL(fileURLWithPath: executable.path + ".json"))
    try installExecutable("""
    #!/usr/bin/python3
    import json, sys
    with open(sys.argv[0] + '.json') as f: snapshot = json.load(f)
    def respond(request_id=None):
        print(json.dumps(dict(protocol_version=1, request_id=request_id, error=None, snapshot=snapshot)), flush=True)
    respond()
    for line in sys.stdin:
        request = json.loads(line)
        if request['command']['type'] == 'begin_creation': snapshot['navigation']['destination'] = dict(kind='new_codemod')
        respond(request['id'])
    """, at: executable)
    let session = Session()
    defer { session.close {} }
    session.start(project: directory, executable: executable)
    try await waitUntil { session.editable }
    session.newDescription = "Keep my unsent request"
    #expect(session.visibleMods.map(\.id) == [1])
    session.showClosed = true
    #expect(session.visibleMods.map(\.id) == [2])
    #expect(session.draft == "Saved draft")
    session.beginCreation()
    try await waitUntil { session.showingNew }
    #expect(session.showingNew)
    #expect(!session.showClosed)
    #expect(session.visibleMods.map(\.id) == [1])
    #expect(session.newDescription == "Keep my unsent request")
    #expect(session.draft == "Saved draft")
    await withCheckedContinuation { continuation in session.close { continuation.resume() } }
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

@Test @MainActor func bundledManropeRegistersAndResolvesTheActualFamily() throws {
    _ = Theme.registerFonts()
    let font = try #require(NSFont(name: "Manrope", size: 13))
    #expect(font.familyName == "Manrope")
    #expect(font.fontName.hasPrefix("Manrope"))
}

@Test func workspaceSelectionKeepsLocalChoicesAndClearsStaleContexts() throws {
    let original = try decodeSnapshot()
    var selection = WorkspaceSelection()
    selection.reconcile(original)
    selection.show(.checks, target: .task("implement_cli"))
    selection.queue = .open
    selection.reconcile(original)
    #expect(selection.tab == .checks)
    #expect(selection.target == .task("implement_cli"))
    #expect(selection.queue == .open)
    let removed = try decodeSnapshot { root in
        var mod = root["active_mod"] as! [String: Any]
        var plan = mod["plan"] as! [String: Any]
        plan["tasks"] = []
        mod["plan"] = plan
        root["active_mod"] = mod
    }
    selection.reconcile(removed)
    #expect(selection.target == nil)
    #expect(selection.tab == .checks)
    selection.show(.agents, target: .worker(4))
    selection.reconcile(original)
    #expect(selection.target == .worker(4))
    let workerRemoved = try decodeSnapshot { root in
        var mod = root["active_mod"] as! [String: Any]
        mod["worker_details"] = []
        root["active_mod"] = mod
    }
    selection.reconcile(workerRemoved)
    #expect(selection.target == nil)
    let switched = try decodeSnapshot { $0["project"] = "/tmp/other-project" }
    selection.reconcile(switched)
    #expect(selection.tab == .conversation)
    #expect(selection.queue == .closed)
    #expect(selection.context?.project == "/tmp/other-project")
}

@Test func repeatedInspectionSnapshotsPreserveLocalTabAndTarget() throws {
    let snapshot = try decodeSnapshot { root in
        root["dialog"] = ["kind": "diff", "title": "Review changes", "body": "Actual patch", "confirm_label": NSNull(), "request_id": NSNull(), "domains": [], "repository_slug": NSNull(), "repository_create": false] as [String: Any]
    }
    var selection = WorkspaceSelection()
    selection.reconcile(snapshot)
    #expect(selection.tab == .changes)
    selection.show(.findings, target: .task("implement_cli"))
    selection.reconcile(snapshot)
    selection.reconcile(snapshot)
    #expect(selection.tab == .findings)
    #expect(selection.target == .task("implement_cli"))
    #expect(snapshot.dialog?.isInspection == true)
}

@Test func queueEditorRetainsUnsavedTextAcrossReorderingAndSelectsRemainingItem() {
    let first = QueuedMessage(id: 7, body: "First instruction")
    let second = QueuedMessage(id: 8, body: "Second instruction")
    var editor = QueueEditor()
    editor.reconcile([first, second])
    #expect(editor.selected == 7)
    #expect(editor.text == "First instruction")
    editor.text = "Unsaved revision"
    editor.reconcile([second, first])
    #expect(editor.selected == 7)
    #expect(editor.text == "Unsaved revision")
    editor.reconcile([second])
    #expect(editor.selected == 8)
    #expect(editor.text == "Second instruction")
}

@Test @MainActor func informationalInspectionKeepsInputUsableUntilDone() async throws {
    let directory = try temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let executable = directory.appendingPathComponent("engine")
    var root = try #require(JSONSerialization.jsonObject(with: Data(snapshotJSON.utf8)) as? [String: Any])
    root["dialog"] = ["kind": "diff", "title": "Review changes", "body": "Actual patch", "confirm_label": NSNull(), "request_id": NSNull(), "domains": [], "repository_slug": NSNull(), "repository_create": false] as [String: Any]
    try JSONSerialization.data(withJSONObject: root).write(to: URL(fileURLWithPath: executable.path + ".json"))
    try installExecutable(#"""
    #!/usr/bin/python3
    import json, sys
    with open(sys.argv[0] + '.json') as f: snapshot = json.load(f)
    def respond(request_id=None):
        if snapshot.get('dialog') and snapshot['dialog']['kind'] in ['diff', 'failure', 'findings', 'sync_details']: snapshot['navigation']['destination'] = dict(kind='codemod', mod_id=snapshot['active_mod']['id'])
        print(json.dumps(dict(protocol_version=1, request_id=request_id, error=None, snapshot=snapshot)), flush=True)
    respond()
    for line in sys.stdin:
        request = json.loads(line)
        if request['command']['type'] == 'begin_creation': snapshot['navigation']['destination'] = dict(kind='new_codemod')
        if request['command']['type'] == 'cancel': snapshot['dialog'] = None
        respond(request['id'])
    """#, at: executable)
    let session = Session()
    defer { session.close {} }
    session.start(project: directory, executable: executable)
    try await waitUntil { session.dialog?.kind == "diff" }
    #expect(session.interactive)
    #expect(session.canCompose)
    #expect(!session.canBeginCreation)
    #expect(session.draft == "Saved draft")
    #expect(session.workspace.tab == .changes)
    session.editDraft("Edited while inspecting")
    #expect(session.draft == "Edited while inspecting")
    session.workspace.show(.checks, target: .task("implement_cli"))
    session.enqueue(Command(type: "snapshot"))
    try await waitUntil { !session.busy }
    #expect(session.workspace.tab == .checks)
    #expect(session.workspace.target == .task("implement_cli"))
    session.enqueue(Command(type: "cancel"))
    try await waitUntil { session.dialog == nil && !session.busy }
    #expect(session.canBeginCreation)
    #expect(session.draft == "Saved draft")
    session.beginCreation()
    try await waitUntil { session.showingNew }
    #expect(session.showingNew)
}

private func decodeSnapshot(_ edit: (inout [String: Any]) -> Void = { _ in }) throws -> Snapshot {
    var root = try #require(JSONSerialization.jsonObject(with: Data(snapshotJSON.utf8)) as? [String: Any])
    edit(&root)
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    return try decoder.decode(Snapshot.self, from: JSONSerialization.data(withJSONObject: root))
}

@Test func currentFailureOutranksCompletionWithoutTreatingPreviousEvidenceAsCurrent() throws {
    let failed = try decodeSnapshot { root in
        var mod = root["active_mod"] as! [String: Any]
        var plan = mod["plan"] as! [String: Any]
        var tasks = plan["tasks"] as! [[String: Any]]
        tasks[0]["completed"] = true
        tasks[0]["error"] = NSNull()
        tasks[0]["results"] = [["id": "final:source:0", "task_id": "implement_cli", "kind": "final_check", "label": "Combined version", "status": "failed", "output": "Failed after integration"]]
        plan["tasks"] = tasks
        mod["plan"] = plan
        root["active_mod"] = mod
    }
    let task = try #require(failed.activeMod?.plan?.tasks.first)
    #expect(task.completed)
    #expect(task.hasCurrentFailure)
    let previous = try decodeSnapshot { root in
        var mod = root["active_mod"] as! [String: Any]
        var plan = mod["plan"] as! [String: Any]
        var tasks = plan["tasks"] as! [[String: Any]]
        tasks[0]["completed"] = true
        tasks[0]["results"] = [["id": "previous:0", "task_id": "implement_cli", "kind": "previous_check", "label": "Earlier attempt", "status": "failed", "output": "Previous failure"]]
        tasks[0]["preparation"] = ["source": "earlier-fingerprint", "current": false, "stopped": false, "error": "Previous setup failed", "results": []] as [String: Any]
        plan["tasks"] = tasks
        mod["plan"] = plan
        root["active_mod"] = mod
    }
    let earlier = try #require(previous.activeMod?.plan?.tasks.first)
    #expect(earlier.completed)
    #expect(!earlier.hasCurrentFailure)
    #expect(earlier.results.first?.kind.caption == "Previous failed checks")
}

@Test func observedTaskCheckEvidenceKeepsBlockedTaskIncomplete() throws {
    let snapshot = try decodeSnapshot { root in
        var mod = root["active_mod"] as! [String: Any]
        var plan = mod["plan"] as! [String: Any]
        var tasks = plan["tasks"] as! [[String: Any]]
        tasks[0]["completed"] = false
        tasks[0]["error"] = "Dependency is still blocked"
        tasks[0]["checks_label"] = "Observed task checks"
        tasks[0]["results"] = [["id": "observed:0", "task_id": "implement_cli", "kind": "observed_check", "label": "Observed failure", "status": "failed", "output": "Observed command: exit 1"]]
        plan["tasks"] = tasks
        mod["plan"] = plan
        root["active_mod"] = mod
    }
    let task = try #require(snapshot.activeMod?.plan?.tasks.first)
    #expect(!task.completed)
    #expect(task.hasCurrentFailure)
    #expect(task.results.first?.kind.caption == "Observed task checks")
    #expect(task.results.first?.output == "Observed command: exit 1")
}

@Test func themePaletteResolvesDifferentlyInDarkAppearance() {
    func resolved(_ color: Color, dark: Bool) -> CGColor? {
        var result: CGColor?
        (dark ? NSAppearance(named: .darkAqua) : NSAppearance(named: .aqua))?.performAsCurrentDrawingAppearance {
            result = NSColor(color).usingColorSpace(.sRGB)?.cgColor
        }
        return result
    }
    let light = resolved(Theme.paper, dark: false)
    let dark = resolved(Theme.paper, dark: true)
    #expect(light != nil && dark != nil && light != dark)
    #expect(WorkflowPhase.needsYou.color != WorkflowPhase.building.color)
    #expect(WorkspaceTab.allCases.map(\.label) == ["Conversation", "Changes", "Checks", "Findings", "Agents"])
}

@Test func packagedFontResourcesResolveInsideTheAppWithoutBuildFallback() throws {
    let directory = try temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let app = directory.appendingPathComponent("Relocated.app")
    let contents = app.appendingPathComponent("Contents")
    let resources = contents.appendingPathComponent("Resources")
    try FileManager.default.createDirectory(at: resources, withIntermediateDirectories: true)
    let info: [String: Any] = ["CFBundlePackageType": "APPL", "CFBundleIdentifier": "dev.sprowt.resource-test", "CFBundleExecutable": "Sprowt"]
    try PropertyListSerialization.data(fromPropertyList: info, format: .xml, options: 0).write(to: contents.appendingPathComponent("Info.plist"))
    let source = try #require(Theme.resourceBundle())
    let copied = resources.appendingPathComponent("SprowtDesktop_SprowtDesktop.bundle")
    try FileManager.default.copyItem(at: source.bundleURL, to: copied)
    let main = try #require(Bundle(url: app))
    let font = try #require(Theme.resourceBundle(in: main)?.url(forResource: "Manrope", withExtension: "ttf", subdirectory: "Manrope"))
    #expect(font.path == copied.appendingPathComponent("Manrope/Manrope.ttf").path)
    #expect(try Data(contentsOf: font).prefix(4) == Data([0x00, 0x01, 0x00, 0x00]))
    let missing = directory.appendingPathComponent("Missing.app/Contents")
    try FileManager.default.createDirectory(at: missing.appendingPathComponent("Resources"), withIntermediateDirectories: true)
    try PropertyListSerialization.data(fromPropertyList: info, format: .xml, options: 0).write(to: missing.appendingPathComponent("Info.plist"))
    let incomplete = try #require(Bundle(url: missing.deletingLastPathComponent()))
    #expect(Theme.resourceBundle(in: incomplete) == nil)
}

@Test @MainActor func pendingInspectionKeepsDoneAccessibleAndPreservesCreationAndComposerDrafts() async throws {
    let directory = try temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let executable = directory.appendingPathComponent("engine")
    try Data(snapshotJSON.utf8).write(to: URL(fileURLWithPath: executable.path + ".json"))
    try installExecutable(#"""
    #!/usr/bin/python3
    import json, os, sys, time
    with open(sys.argv[0] + '.json') as f: snapshot = json.load(f)
    def respond(request_id=None):
        if snapshot.get('dialog') and snapshot['dialog']['kind'] in ['diff', 'failure', 'findings', 'sync_details']: snapshot['navigation']['destination'] = dict(kind='codemod', mod_id=snapshot['active_mod']['id'])
        print(json.dumps(dict(protocol_version=1, request_id=request_id, error=None, snapshot=snapshot)), flush=True)
    respond()
    for line in sys.stdin:
        request = json.loads(line)
        if request['command']['type'] == 'begin_creation': snapshot['navigation']['destination'] = dict(kind='new_codemod')
        if request['command']['type'] == 'action':
            open(sys.argv[0] + '.entered', 'w').close()
            while not os.path.exists(sys.argv[0] + '.release'): time.sleep(0.01)
            snapshot['dialog'] = dict(kind='diff', title='Review changes', body='Actual patch', confirm_label=None, request_id=None, domains=[], repository_slug=None, repository_create=False)
        elif request['command']['type'] == 'cancel': snapshot['dialog'] = None
        respond(request['id'])
    """#, at: executable)
    let session = Session()
    defer { session.close {} }
    session.start(project: directory, executable: executable)
    try await waitUntil { session.interactive }
    session.newDescription = "Keep this unfinished description"
    session.perform(EngineAction(id: "diff", label: "Inspect changes", taskId: nil))
    try await waitUntil { FileManager.default.fileExists(atPath: executable.path + ".entered") }
    session.beginCreation()
    #expect(session.busy)
    #expect(!session.showingNew)
    try Data().write(to: URL(fileURLWithPath: executable.path + ".release"))
    try await waitUntil { session.dialog?.kind == "diff" }
    #expect(!session.showingNew)
    #expect(session.interactive)
    #expect(session.workspace.tab == .changes)
    #expect(session.draft == "Saved draft")
    #expect(session.newDescription == "Keep this unfinished description")
    session.enqueue(Command(type: "cancel"))
    try await waitUntil { session.dialog == nil && !session.busy }
    session.beginCreation()
    try await waitUntil { session.showingNew }
    #expect(session.showingNew)
    #expect(session.draft == "Saved draft")
    #expect(session.newDescription == "Keep this unfinished description")
}

@Test @MainActor func inspectionQueuedBehindDraftPersistenceKeepsDoneAndBothDraftsAccessible() async throws {
    let directory = try temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let executable = directory.appendingPathComponent("engine")
    try Data(snapshotJSON.utf8).write(to: URL(fileURLWithPath: executable.path + ".json"))
    try installExecutable(#"""
    #!/usr/bin/python3
    import json, os, sys, time
    with open(sys.argv[0] + '.json') as f: snapshot = json.load(f)
    def respond(request_id=None):
        if snapshot.get('dialog') and snapshot['dialog']['kind'] in ['diff', 'failure', 'findings', 'sync_details']: snapshot['navigation']['destination'] = dict(kind='codemod', mod_id=snapshot['active_mod']['id'])
        print(json.dumps(dict(protocol_version=1, request_id=request_id, error=None, snapshot=snapshot)), flush=True)
    respond()
    for line in sys.stdin:
        request = json.loads(line)
        if request['command']['type'] == 'begin_creation': snapshot['navigation']['destination'] = dict(kind='new_codemod')
        if request['command']['type'] == 'set_draft':
            open(sys.argv[0] + '.entered', 'w').close()
            while not os.path.exists(sys.argv[0] + '.release'): time.sleep(0.01)
            snapshot['active_mod']['draft'] = request['command']['text']
        elif request['command']['type'] == 'action':
            snapshot['dialog'] = dict(kind='diff', title='Review changes', body='Actual patch', confirm_label=None, request_id=None, domains=[], repository_slug=None, repository_create=False)
        elif request['command']['type'] == 'cancel': snapshot['dialog'] = None
        respond(request['id'])
    """#, at: executable)
    let session = Session()
    defer { session.close {} }
    session.start(project: directory, executable: executable)
    try await waitUntil { session.interactive }
    session.newDescription = "Keep this unfinished description"
    session.editDraft("Draft being persisted")
    try await waitUntil { FileManager.default.fileExists(atPath: executable.path + ".entered") }
    #expect(!session.busy)
    #expect(session.canBeginCreation)
    session.perform(EngineAction(id: "diff", label: "Inspect changes", taskId: nil))
    #expect(session.busy)
    #expect(!session.canBeginCreation)
    #expect(session.canCompose)
    session.beginCreation()
    session.editDraft("A newer draft while inspection waits")
    #expect(!session.showingNew)
    try Data().write(to: URL(fileURLWithPath: executable.path + ".release"))
    try await waitUntil { session.dialog?.kind == "diff" && session.active?.draft == "A newer draft while inspection waits" }
    #expect(!session.showingNew)
    #expect(session.workspace.tab == .changes)
    #expect(session.draft == "A newer draft while inspection waits")
    #expect(session.newDescription == "Keep this unfinished description")
    session.enqueue(Command(type: "cancel"))
    try await waitUntil { session.canBeginCreation }
    session.beginCreation()
    try await waitUntil { session.showingNew }
    #expect(session.showingNew)
    #expect(session.draft == "A newer draft while inspection waits")
    #expect(session.newDescription == "Keep this unfinished description")
}

@Test @MainActor func unsolicitedInspectionReturnsFromCreationWithoutDiscardingEitherDraft() async throws {
    let directory = try temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let executable = directory.appendingPathComponent("engine")
    try Data(snapshotJSON.utf8).write(to: URL(fileURLWithPath: executable.path + ".json"))
    try installExecutable(#"""
    #!/usr/bin/python3
    import json, os, sys, time, threading
    with open(sys.argv[0] + '.json') as f: snapshot = json.load(f)
    def respond(request_id=None):
        if snapshot.get('dialog') and snapshot['dialog']['kind'] in ['diff', 'failure', 'findings', 'sync_details']: snapshot['navigation']['destination'] = dict(kind='codemod', mod_id=snapshot['active_mod']['id'])
        print(json.dumps(dict(protocol_version=1, request_id=request_id, error=None, snapshot=snapshot)), flush=True)
    def finish_inspection():
        while not os.path.exists(sys.argv[0] + '.release'): time.sleep(0.01)
        snapshot['dialog'] = dict(kind='diff', title='Review changes', body='Actual patch', confirm_label=None, request_id=None, domains=[], repository_slug=None, repository_create=False)
        snapshot['navigation']['destination'] = dict(kind='codemod', mod_id=snapshot['active_mod']['id'])
        respond()
    respond()
    for line in sys.stdin:
        request = json.loads(line)
        if request['command']['type'] == 'begin_creation': snapshot['navigation']['destination'] = dict(kind='new_codemod')
        kind = request['command']['type']
        if kind == 'set_draft': snapshot['active_mod']['draft'] = request['command']['text']
        elif kind == 'action':
            snapshot['phase'] = 'git'
            snapshot['status'] = 'loading changes'
            respond(request['id'])
            threading.Thread(target=finish_inspection).start()
            continue
        elif kind == 'cancel': snapshot['dialog'] = None
        respond(request['id'])
    """#, at: executable)
    let session = Session()
    defer { session.close {} }
    session.start(project: directory, executable: executable)
    try await waitUntil { session.interactive }
    session.editDraft("Keep the composer draft")
    try await waitUntil { session.active?.draft == "Keep the composer draft" }
    session.perform(EngineAction(id: "diff", label: "Inspect changes", taskId: nil))
    try await waitUntil { session.snapshot?.status == "loading changes" && !session.busy }
    #expect(session.canBeginCreation)
    session.beginCreation()
    try await waitUntil { session.showingNew }
    session.newDescription = "New description typed during loading"
    #expect(session.showingNew)
    try Data().write(to: URL(fileURLWithPath: executable.path + ".release"))
    try await waitUntil { session.dialog?.kind == "diff" }
    #expect(!session.showingNew)
    #expect(session.workspace.tab == .changes)
    #expect(session.draft == "Keep the composer draft")
    #expect(session.newDescription == "New description typed during loading")
    session.enqueue(Command(type: "cancel"))
    try await waitUntil { session.canBeginCreation }
    #expect(!session.showingNew)
    session.beginCreation()
    try await waitUntil { session.showingNew }
    #expect(session.showingNew)
    #expect(session.draft == "Keep the composer draft")
    #expect(session.newDescription == "New description typed during loading")
}


@Test func workspaceTabsClearExplicitScopeAndKeepQueueState() throws {
    let snapshot = try decodeSnapshot()
    var selection = WorkspaceSelection()
    selection.reconcile(snapshot)
    selection.queue = .open
    for tab in snapshot.workspaceViews {
        selection.show(.checks, target: .task("implement_cli"))
        selection.show(tab)
        selection.reconcile(snapshot)
        #expect(selection.tab == tab)
        #expect(selection.target == nil)
        #expect(selection.queue == .open)
    }
}
