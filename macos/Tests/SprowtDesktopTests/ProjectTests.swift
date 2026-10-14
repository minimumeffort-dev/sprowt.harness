import Foundation
import Testing
@testable import SprowtDesktop

@Test func savedProjectCatalogMatchesTheRustFixture() throws {
    var root = URL(fileURLWithPath: #filePath)
    for _ in 0..<4 { root.deleteLastPathComponent() }
    let catalog = try ProjectCatalog.decode(Data(contentsOf: root.appendingPathComponent("tests/fixtures/projects.json")))
    #expect(catalog.projects.map(\.name) == ["todo", "notes"])
    #expect(catalog.projects[0].mods.map(\.id) == [1])
    #expect(catalog.appearance == .system)
    #expect(throws: ProtocolError.self) {
        try ProjectCatalog.decode(Data(#"{"protocol_version":2,"projects":[],"appearance":"system"}"#.utf8))
    }
}

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
            if os.path.exists(sys.argv[0] + '.reject'): error = 'This project is already open in another Sprowt window.'
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

@Test @MainActor func backgroundProjectUpdatesPreserveTheVisibleDraftAndSelection() async throws {
    let directory = try temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let executable = directory.appendingPathComponent("engine")
    try Data(snapshotJSON.utf8).write(to: URL(fileURLWithPath: executable.path + ".json"))
    try installExecutable(#"""
    #!/usr/bin/python3
    import copy, json, sys
    snapshot = json.load(open(sys.argv[0] + '.json'))
    other = copy.deepcopy(snapshot['projects'][0])
    other.update(id=2, name='Background', path='/tmp/background', activity='working')
    snapshot['projects'].append(other)
    def respond(request_id=None):
        print(json.dumps(dict(protocol_version=1, request_id=request_id, error=None, snapshot=snapshot)), flush=True)
    respond()
    for line in sys.stdin:
        request = json.loads(line)
        command = request['command']
        if command['type'] == 'set_draft':
            snapshot['active_mod']['draft'] = command['text']
            respond(request['id'])
            snapshot['projects'][1]['activity'] = 'attention'
            respond()
        else: respond(request['id'])
    """#, at: executable)
    let session = Session()
    defer { session.close {} }
    session.start(project: directory, executable: executable)
    try await waitUntil { session.editable }
    #expect(session.snapshot?.projects.last?.activity == .working)
    session.editDraft("Keep typing in this project")
    try await waitUntil { session.snapshot?.projects.last?.activity == .attention && !session.busy }
    #expect(session.snapshot?.projectId == 1)
    #expect(session.active?.id == 1)
    #expect(session.draft == "Keep typing in this project")
    #expect(session.canCompose)
    #expect(session.snapshot?.dialog == nil)
    #expect(session.error == nil)
}

@Test @MainActor func savedProjectsLoadBeforeOpeningAndSelectTheRequestedCodemod() async throws {
    let directory = try temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let executable = directory.appendingPathComponent("engine")
    try Data(snapshotJSON.utf8).write(to: URL(fileURLWithPath: executable.path + ".json"))
    try installExecutable(#"""
    #!/usr/bin/python3
    import json, sys
    snapshot = json.load(open(sys.argv[0] + '.json'))
    with open(sys.argv[0] + '.launches', 'a') as f: f.write(json.dumps(sys.argv[1:]) + '\n')
    if sys.argv[1] == 'projects':
        snapshot['projects'][0]['path'] = sys.argv[sys.argv.index('--data-dir')+1]
        snapshot['projects'][0]['mods'].append(dict(id=2, name='Second task', closed=False))
        print(json.dumps(dict(protocol_version=1, projects=snapshot['projects'], appearance='light')), flush=True)
        sys.exit(0)
    def respond(request_id=None):
        print(json.dumps(dict(protocol_version=1, request_id=request_id, error=None, snapshot=snapshot)), flush=True)
    respond()
    for line in sys.stdin:
        request = json.loads(line)
        command = request['command']
        if command['type'] == 'select_mod':
            snapshot['active_mod']['id'] = command['mod_id']
            snapshot['active_mod']['name'] = 'Second task'
        respond(request['id'])
    """#, at: executable)
    let session = Session()
    defer { session.close {} }
    session.loadCatalog(dataDirectory: directory.path, executable: executable)
    try await waitUntil { session.canBrowseProjects }
    #expect(session.snapshot == nil)
    #expect(session.projects.map(\.name) == ["project"])
    #expect(session.appearance == .light)
    session.toggleProject(1)
    #expect(session.expandedProject == 1)
    session.toggleProject(1)
    #expect(session.expandedProject == nil)
    let before = try String(contentsOfFile: executable.path + ".launches", encoding: .utf8)
    #expect(before.split(separator: "\n").count == 1)
    #expect(before.contains("projects"))
    session.select(2)
    try await waitUntil { session.active?.id == 2 && !session.busy }
    #expect(session.active?.name == "Second task")
    #expect(session.draft == "Saved draft")
    let launches = try String(contentsOfFile: executable.path + ".launches", encoding: .utf8)
        .split(separator: "\n").map { try JSONSerialization.jsonObject(with: Data($0.utf8)) as! [String] }
    #expect(launches == [["projects", "--data-dir", directory.path], ["desktop", "--project", directory.path, "--data-dir", directory.path]])
}

@Test @MainActor func removeSavedProjectBeforeOpeningRefreshesTheCatalogAndKeepsFailuresVisible() async throws {
    let directory = try temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let executable = directory.appendingPathComponent("engine")
    try installExecutable(#"""
    #!/usr/bin/python3
    import json, sys
    projects = [dict(id=n, path='/tmp/project'+str(n), name='Project '+str(n), activity='idle', mods=[]) for n in [1,2]]
    if '--remove' in sys.argv:
        removed = int(sys.argv[sys.argv.index('--remove')+1])
        if removed == 2:
            print('Catalog write failed', file=sys.stderr)
            sys.exit(1)
        projects = [p for p in projects if p['id'] != removed]
    print(json.dumps(dict(protocol_version=1, projects=projects, appearance='dark')), flush=True)
    """#, at: executable)
    let session = Session()
    session.loadCatalog(dataDirectory: directory.path, executable: executable)
    try await waitUntil { session.canBrowseProjects }
    session.toggleProject(1)
    session.removeProject(1)
    try await waitUntil { session.canBrowseProjects && session.projects.count == 1 }
    #expect(session.projects.map(\.id) == [2])
    #expect(session.expandedProject == nil)
    #expect(session.snapshot == nil)
    #expect(session.appearance == .dark)
    session.removeProject(2)
    try await waitUntil { session.canBrowseProjects && session.error != nil }
    #expect(session.error == "Catalog write failed")
    #expect(session.projects.map(\.id) == [2])
}
