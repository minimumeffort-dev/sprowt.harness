import AppKit
import SwiftUI

// Read-only messages use the composer's existing Markdown interpretation.
@MainActor
struct MessageBlock {
    let text: NSAttributedString
    let kind: String
    let marker: String?

    static func parse(_ source: String) -> [Self] {
        let decoded = ComposerMarkdown.decode(source)
        let string = decoded.string as NSString
        var blocks: [Self] = [], position = 0
        while position < decoded.length {
            var range = string.paragraphRange(for: NSRange(location: position, length: 0))
            let kind = decoded.attribute(.composerBlock, at: position, effectiveRange: nil) as? String ?? ""
            if kind.hasPrefix("code:") {
                var groupRange = NSRange()
                _ = decoded.attribute(.composerCodeGroup, at: position, effectiveRange: &groupRange)
                range = groupRange
            }
            position = NSMaxRange(range)
            if range.length > 0, string.character(at: NSMaxRange(range) - 1) == 10 { range.length -= 1 }
            let marker = ComposerMarkdown.marker(kind)
            if let marker, string.substring(with: range).hasPrefix(marker) {
                range.location += marker.utf16.count
                range.length -= marker.utf16.count
            }
            blocks.append(Self(text: decoded.attributedSubstring(from: range), kind: kind, marker: marker))
        }
        return blocks
    }

    var styled: AttributedString {
        var result = AttributedString()
        text.enumerateAttributes(in: text.fullRange) { attributes, range, _ in
            var run = AttributedString((text.string as NSString).substring(with: range))
            if let font = attributes[.font] as? NSFont { run.font = Font(font) }
            run.foregroundColor = Theme.ink
            if (attributes[.composerInline] as? Int ?? 0) & 4 != 0 {
                run.foregroundColor = Theme.accent
                run.backgroundColor = Theme.codeBackground
            }
            result.append(run)
        }
        return result
    }
}

struct MessageText: View {
    let source: String
    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            ForEach(Array(MessageBlock.parse(source).enumerated()), id: \.offset) { _, block in
                if block.kind.hasPrefix("code:") {
                    CodeText(text: block.text.string, label: String(block.kind.dropFirst(5)))
                        .padding(.vertical, 7)
                } else if block.text.length == 0 {
                    Color.clear.frame(height: 7).accessibilityHidden(true)
                } else {
                    HStack(alignment: .firstTextBaseline, spacing: 7) {
                        if let marker = block.marker { Text(marker).foregroundStyle(Theme.muted) }
                        Text(block.styled).lineSpacing(5)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }.padding(.top, block.kind.hasPrefix("#") ? 5 : 0)
                }
            }
        }.font(Theme.font(14)).foregroundStyle(Theme.ink)
            .frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled)
    }
}

struct CodeText: View {
    let text: String
    var label = ""
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if !label.isEmpty {
                Text(label).font(Theme.font(11)).foregroundStyle(Theme.muted)
                    .padding(.horizontal, 12).padding(.vertical, 8)
                WorkspaceDivider()
            }
            Text(verbatim: text).font(.system(size: Theme.scaledSize(12), design: .monospaced))
                .lineSpacing(4).foregroundStyle(Theme.ink).textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading).padding(12)
        }.frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.codeBackground, in: RoundedRectangle(cornerRadius: Theme.radius))
    }
}

struct SourceReference: View {
    let source: String
    var body: some View {
        DisclosureGroup {
            CodeText(text: source).padding(.top, 8)
        } label: {
            Text("Source reference").font(Theme.font(12)).foregroundStyle(Theme.muted)
        }.frame(maxWidth: .infinity, alignment: .leading)
    }
}

struct EvidenceNotice: View {
    let text: String
    var color: Color = Theme.red
    var body: some View {
        let parts = text.split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false)
        VStack(alignment: .leading, spacing: 10) {
            Text(verbatim: String(parts.first ?? "")).font(Theme.font(14)).foregroundStyle(color)
                .lineSpacing(4).textSelection(.enabled)
            if parts.count > 1 { CodeText(text: String(parts[1])) }
        }.frame(maxWidth: .infinity, alignment: .leading).padding(12)
            .background(color.opacity(0.06), in: RoundedRectangle(cornerRadius: Theme.radius))
    }
}

extension PlanTask {
    var preparationOwnsError: Bool {
        guard let preparation, preparation.current, let reason = preparation.error, let error else { return false }
        return error == reason || error == "Environment preflight failed: \(reason)"
    }
}

extension CodemodDetail {
    func inspectedTask(_ target: WorkspaceSelection.Target?) -> PlanTask? {
        let id: String?
        switch target {
        case .task(let task): id = task
        case .worker(let worker): id = workerDetails.first(where: { $0.id == worker })?.taskId
        case nil: id = nil
        }
        return plan?.tasks.first { $0.id == id }
    }
}
