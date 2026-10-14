import Foundation

struct MessageContent: Decodable, Sendable, Identifiable {
    let id: String
    let segments: [MessageSegment]
}

struct MessageSegment: Decodable, Sendable, Identifiable {
    enum Content: Sendable {
        case prose(text: String, preview: String?)
        case code(source: String, language: String, summary: String, collapsed: Bool, tokens: [CodeToken])
    }
    let id: Int
    let content: Content

    private enum CodingKeys: String, CodingKey { case id, kind, text, preview, source, language, summary, collapsed, tokens }
    private enum Kind: String, Decodable { case prose, code }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        id = try values.decode(Int.self, forKey: .id)
        switch try values.decode(Kind.self, forKey: .kind) {
        case .prose:
            content = .prose(text: try values.decode(String.self, forKey: .text), preview: try values.decodeIfPresent(String.self, forKey: .preview))
        case .code:
            content = .code(source: try values.decode(String.self, forKey: .source), language: try values.decode(String.self, forKey: .language),
                            summary: try values.decode(String.self, forKey: .summary), collapsed: try values.decode(Bool.self, forKey: .collapsed),
                            tokens: try values.decode([CodeToken].self, forKey: .tokens))
        }
    }

    var initiallyCollapsed: Bool {
        switch content {
        case .prose(_, let preview): preview != nil
        case .code(_, _, _, let collapsed, _): collapsed
        }
    }
}

struct CodeToken: Decodable, Sendable {
    enum Kind: String, Decodable, Sendable { case plain, key, string, number, literal, punctuation }
    let text: String
    let kind: Kind
}
