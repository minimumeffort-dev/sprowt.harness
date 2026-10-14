import Foundation
import Testing
@testable import SprowtDesktop

@Test @MainActor func projectCommandsDrainDraftsAndRejectSwitchesWithoutReplacingLocalText() async throws {
    let directory = try temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let executable = directory.appendingPathComponent("engine")
    try Data(snapshotJSON.utf8).write(to: URL(fileURLWithPath: executable.path + ".json"))
    try installExecutable(#"""
    #!/usr/bin/python3
    import copy, json, os, sys, time
    snapshot = json.load(open(sys.argv[0] + '.json'))
    a = snapshot
    b = copy.deepcopy(a)
    b.update(project_id=2, project='/tmp/second', creation_draft='B creation')
    b['active_mod'].update(id=2, draft='B instruction')
    snapshots = {1:a, 2:b}
    def respond(request_id=None, error=None):
        print(json.dumps(dict(protocol_version=1, request_id=request_id, error=error, snapshot=snapshot)), flush=True)
    respond()
    for line in sys.stdin:
        request = json.loads(line)
        command = request['command']
        kind = command['type']
        with open(sys.argv[0] + '.commands', 'a') as f: f.write(json.dumps(command) + '\n')
        error = None
        if kind == 'set_creation_draft':
            respond()
            open(sys.argv[0] + '.draft_entered', 'w').close()
            while not os.path.exists(sys.argv[0] + '.draft_release'): time.sleep(0.01)
            snapshot['creation_draft'] = command['text']
        elif kind == 'select_project':
            if os.path.exists(sys.argv[0] + '.reject'): error = 'Pause running work in this project before switching projects.'
            else: snapshot = snapshots[command['project_id']]
        elif kind == 'toggle_project':
            snapshot['expanded_project'] = None if snapshot['expanded_project'] == command['project_id'] else command['project_id']
        respond(request['id'], error)
    """#, at: executable)
    let session = Session()
    defer { session.close {} }
    session.start(project: directory, executable: executable)
    try await waitUntil { session.editable }
    session.newDescription = "A first draft"
    try await waitUntil { FileManager.default.fileExists(atPath: executable.path + ".draft_entered") }
    session.newDescription = "A latest draft"
    session.selectProject(2)
    #expect(session.busy && !session.canCompose)
    #expect(session.newDescription == "A latest draft")
    try Data().write(to: URL(fileURLWithPath: executable.path + ".draft_release"))
    try await waitUntil { session.snapshot?.projectId == 2 && !session.busy }
    #expect(session.newDescription == "B creation")
    #expect(session.draft == "B instruction")
    session.selectProject(1)
    try await waitUntil { session.snapshot?.projectId == 1 && !session.busy }
    #expect(session.newDescription == "A latest draft")
    session.toggleProject(1)
    try await waitUntil { session.snapshot?.expandedProject == nil && !session.busy }
    #expect(session.snapshot?.projectId == 1 && session.newDescription == "A latest draft")
    try Data().write(to: URL(fileURLWithPath: executable.path + ".reject"))
    session.selectProject(2)
    try await waitUntil { session.error != nil && !session.busy }
    #expect(session.snapshot?.projectId == 1 && session.newDescription == "A latest draft")
    let commands = try String(contentsOfFile: executable.path + ".commands", encoding: .utf8)
        .split(separator: "\n").map { try JSONSerialization.jsonObject(with: Data($0.utf8)) as! [String: Any] }
    #expect(commands.prefix(3).compactMap { $0["type"] as? String } == ["set_creation_draft", "set_creation_draft", "select_project"])
    #expect(commands[1]["text"] as? String == "A latest draft")
}
