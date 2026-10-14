import SwiftUI

struct MessagePresentation: View {
    let content: MessageContent
    @Binding var toggles: Set<String>

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            ForEach(content.segments) { segment in
                let key = "\(content.id):\(segment.id)"
                let collapsed = segment.initiallyCollapsed != toggles.contains(key)
                switch segment.content {
                case .prose(let text, let preview):
                    VStack(alignment: .leading, spacing: 10) {
                        if collapsed, let preview {
                            Text(prosePreview(preview)).font(Theme.font(14)).lineSpacing(5).lineLimit(4)
                                .frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled)
                        } else {
                            MessageText(source: text)
                        }
                        if preview != nil {
                            Button(collapsed ? "Show full message" : "Show less") { toggle(key) }
                                .buttonStyle(.plain).font(Theme.font(12, weight: .medium)).foregroundStyle(Theme.accent)
                                .accessibilityValue(collapsed ? "Collapsed" : "Expanded")
                        }
                    }
                case .code(let source, let language, let summary, _, let tokens):
                    VStack(alignment: .leading, spacing: 12) {
                        WorkspaceDivider()
                        Button { toggle(key) } label: {
                            HStack(spacing: 8) {
                                Image(systemName: collapsed ? "chevron.right" : "chevron.down").font(.system(size: 10, weight: .medium)).frame(width: 12)
                                Text(language.isEmpty ? "Code" : language).font(.system(size: Theme.scaledSize(12), weight: .medium, design: .monospaced)).foregroundStyle(Theme.ink)
                                Text(summary.components(separatedBy: " · ").dropFirst().joined(separator: " · ")).font(Theme.font(12))
                                Spacer(minLength: 0)
                                Text(collapsed ? "Show source" : "Hide source").font(Theme.font(12, weight: .medium)).foregroundStyle(Theme.accent)
                            }.foregroundStyle(Theme.muted).contentShape(Rectangle())
                        }.buttonStyle(.plain).accessibilityLabel("\(collapsed ? "Show source" : "Hide source"): \(summary)").accessibilityValue(collapsed ? "Collapsed" : "Expanded")
                        if !collapsed {
                            Text(codeText(source, tokens: tokens))
                                .font(.system(size: Theme.scaledSize(12), design: .monospaced))
                                .lineSpacing(7).textSelection(.enabled)
                                .fixedSize(horizontal: false, vertical: true)
                                .frame(maxWidth: .infinity, alignment: .leading).padding(14)
                                .background(Theme.codeBackground, in: RoundedRectangle(cornerRadius: 5))
                        }
                    }
                }
            }
        }.frame(maxWidth: .infinity, alignment: .leading)
    }

    private func toggle(_ key: String) {
        if !toggles.insert(key).inserted { toggles.remove(key) }
    }
}

@MainActor
private func prosePreview(_ source: String) -> AttributedString {
    var result = AttributedString()
    for (index, block) in MessageBlock.parse(source).enumerated() {
        if index > 0 { result.append(AttributedString("\n")) }
        if let marker = block.marker { result.append(AttributedString(marker)) }
        result.append(block.styled)
    }
    result.append(AttributedString("…"))
    return result
}

func codeText(_ source: String, tokens: [CodeToken]) -> AttributedString {
    if tokens.isEmpty {
        var text = AttributedString(source)
        text.foregroundColor = Theme.ink
        return text
    }
    var text = AttributedString()
    for token in tokens {
        var run = AttributedString(token.text)
        run.foregroundColor = switch token.kind {
        case .plain: Theme.ink
        case .key: Theme.blue
        case .string: Theme.accent
        case .number, .literal: Theme.amber
        case .punctuation: Theme.muted
        }
        text.append(run)
    }
    return text
}
