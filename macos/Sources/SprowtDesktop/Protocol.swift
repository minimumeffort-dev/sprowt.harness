import Foundation

struct Envelope: Decodable, Sendable {
    let protocolVersion: Int
    let requestId: UInt64?
    let error: String?
    let snapshot: Snapshot

    static func decode(_ data: Data) throws -> Envelope {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let result = try decoder.decode(Self.self, from: data)
        guard result.protocolVersion == 1 else {
            throw ProtocolError.unsupportedVersion(result.protocolVersion)
        }
        return result
    }
}

enum ProtocolError: LocalizedError {
    case unsupportedVersion(Int)
    case incompleteLine

    var errorDescription: String? {
        switch self {
        case .unsupportedVersion(let version): "Unsupported desktop protocol version: \(version). Rebuild the app and its Rust engine together."
        case .incompleteLine: "The Rust engine closed its output before finishing a response."
        }
    }
}

enum Appearance: String, Codable, CaseIterable, Sendable {
    case system, light, dark
    var label: String { rawValue.capitalized }
}

struct ProjectCatalog: Decodable, Sendable {
    let protocolVersion: Int
    let projects: [ProjectSummary]
    let appearance: Appearance

    static func decode(_ data: Data) throws -> ProjectCatalog {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let catalog = try decoder.decode(Self.self, from: data)
        guard catalog.protocolVersion == 1 else { throw ProtocolError.unsupportedVersion(catalog.protocolVersion) }
        return catalog
    }
}

struct Snapshot: Decodable, Sendable {
    let workerSettings: WorkerSettings
    let modelCatalog: ModelCatalog
    let appearance: Appearance
    let project: String
    let projectId: Int64
    let projects: [ProjectSummary]
    let expandedProject: Int64?
    let creationDraft: String
    let navigation: NavigationState
    let workspaceViews: [WorkspaceTab]
    let phase: WorkflowPhase
    let mods: [CodemodSummary]
    let activeMod: CodemodDetail?
    let status: String
    let detail: String
    let evidence: String
    let error: String?
    let actions: [EngineAction]
    let dialog: EngineDialog?
}

enum NavigationDestination: Decodable, Equatable, Sendable {
    case codemod(Int64), newCodemod

    private enum CodingKeys: String, CodingKey { case kind, modId }
    private enum Kind: String, Decodable { case codemod, newCodemod = "new_codemod" }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        switch try values.decode(Kind.self, forKey: .kind) {
        case .codemod: self = .codemod(try values.decode(Int64.self, forKey: .modId))
        case .newCodemod: self = .newCodemod
        }
    }
}

struct NavigationState: Decodable, Sendable {
    let destination: NavigationDestination
    let canGoBack: Bool
    let canGoForward: Bool
}

enum ProjectActivity: String, Decodable, Sendable {
    case idle, working, attention
    var label: String {
        switch self {
        case .idle: ""
        case .working: "Working in background"
        case .attention: "Needs attention"
        }
    }
}

struct ProjectSummary: Decodable, Identifiable, Sendable {
    let id: Int64
    let path: String
    let name: String
    let mods: [ProjectCodemod]
    let activity: ProjectActivity
}

struct ProjectCodemod: Decodable, Identifiable, Sendable {
    let id: Int64
    let name: String
    let closed: Bool
}

struct CodemodSummary: Decodable, Identifiable, Sendable {
    let id: Int64
    let name: String
    let closed: Bool
    let status: String
    let workers: [WorkerSummary]
}

struct CodemodDetail: Decodable, Identifiable, Sendable {
    let id: Int64
    let name: String
    let description: String
    let closed: Bool
    let userPaused: Bool
    let draft: String
    let messages: [ConversationMessage]
    let history: [ConversationMessage]
    let queue: [QueuedMessage]
    let steering: [String]
    let plan: Plan?
    let checks: [Check]
    let findings: [Finding]
    let prFeedback: [Finding]
    let prFeedbackError: String?
    let workers: [String]
    let workerDetails: [WorkerSummary]
    let timings: String
}

enum WorkflowPhase: String, Decodable, Sendable {
    case new, planning, building, checking, recovering, reviewing, git
    case needsYou = "needs_you"
    case paused, ready, published, closed
}

struct WorkerSummary: Decodable, Identifiable, Sendable {
    let id: Int64
    let provider: String
    let identity: String
    let role: String
    let taskId: String?
    let state: String
    let activity: String
}

enum EvidenceKind: String, Decodable, Sendable {
    case check
    case finalCheck = "final_check"
    case previousCheck = "previous_check"
    case observedCheck = "observed_check"
    case preparationSetup = "preparation_setup"
    case preparationCapabilities = "preparation_capabilities"
    case preparationBaseline = "preparation_baseline"
}

struct PreparationEvidence: Decodable, Sendable {
    let source: String
    let current: Bool
    let stopped: Bool
    let error: String?
    let results: [Check]
}

struct ConversationMessage: Decodable, Sendable {
    let content: MessageContent
    let role: String
    let author: String
    let body: String
    let model: String?
    let effort: String?
}

struct QueuedMessage: Decodable, Identifiable, Sendable {
    let id: Int64
    let body: String
}

struct Plan: Decodable, Sendable {
    let status: String
    let summary: String
    let tasks: [PlanTask]
}

struct PlanTask: Decodable, Identifiable, Sendable {
    let id: String
    let title: String
    let outcome: String
    let dependsOn: [String]
    let status: String
    let detail: String
    let files: [String]
    let checks: [String]
    let runId: Int64?
    let identity: String
    let source: String?
    let error: String?
    let latest: ConversationMessage?
    let results: [Check]
    let checksLabel: String
    let completed: Bool
    let preparation: PreparationEvidence?
    let actions: [EngineAction]

    var hasCurrentFailure: Bool {
        error != nil || results.contains { $0.kind != .previousCheck && $0.status == .failed } ||
        (preparation.map { $0.current && !$0.stopped && $0.error != nil } ?? false)
    }
}

enum CheckStatus: String, Decodable, Sendable { case passed, failed, skipped }

struct Check: Decodable, Identifiable, Sendable {
    let id: String
    let taskId: String?
    let kind: EvidenceKind
    let label: String
    let status: CheckStatus
    let output: String
}

struct Finding: Decodable, Identifiable, Sendable {
    let id: String
    let title: String
    let detail: String
    let status: String
    let imported: Bool
}

struct EngineAction: Decodable, Identifiable, Sendable {
    let id: String
    let label: String
    let taskId: Int64?
    var identity: String { "\(id):\(taskId.map(String.init) ?? "")" }
}

struct EngineDialog: Decodable, Identifiable, Sendable {
    let kind: String
    let title: String
    let body: String
    let confirmLabel: String?
    let requestId: Int64?
    let domains: [String]
    let repositorySlug: String?
    let repositoryCreate: Bool
    var id: String { "\(kind):\(requestId.map(String.init) ?? ""):\(body)" }
}

struct Command: Encodable, Sendable {
    let type: String
    var modId: Int64?
    var description: String?
    var text: String?
    var action: String?
    var taskId: Int64?
    var messageId: Int64?
    var messageIds: [Int64]?
    var feedbackIds: [String]?
    var direction: String?
    var kind: String?
    var requestId: Int64?
    var allow: Bool?
    var slug: String?
    var create: Bool?
    var appearance: Appearance?
    var projectId: Int64?
    var path: String?
    var settings: WorkerSettings?
}

struct Request: Encodable, Sendable {
    let id: UInt64
    let command: Command

    func encodedLine() throws -> Data {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        var data = try encoder.encode(self)
        data.append(0x0a)
        return data
    }
}

struct Drafts {
    struct Entry {
        var text: String
        var revision: UInt64 = 0
        var savedRevision: UInt64 = 0
    }
    private(set) var entries: [Int64: Entry] = [:]

    mutating func edit(_ text: String, for id: Int64) -> UInt64 {
        var entry = entries[id] ?? Entry(text: "")
        entry.text = text
        entry.revision += 1
        entries[id] = entry
        return entry.revision
    }

    mutating func acknowledge(id: Int64, revision: UInt64) {
        guard var entry = entries[id] else { return }
        entry.savedRevision = max(entry.savedRevision, revision)
        entries[id] = entry
    }

    mutating func receive(id: Int64, text: String) {
        var entry = entries[id] ?? Entry(text: text)
        guard entry.revision == entry.savedRevision else { return }
        entry.text = text
        entries[id] = entry
    }

    mutating func restoreSubmission(id: Int64, revision: UInt64, text: String) -> Bool {
        guard var entry = entries[id], entry.revision == revision else { return false }
        entry.text = text
        entries[id] = entry
        return true
    }

    func text(for id: Int64, fallback: String) -> String {
        entries[id]?.text ?? fallback
    }
}


enum WorkspaceTab: String, Decodable, CaseIterable, Sendable {
    case conversation, changes, checks, findings, agents
    var label: String { rawValue.capitalized }
}
