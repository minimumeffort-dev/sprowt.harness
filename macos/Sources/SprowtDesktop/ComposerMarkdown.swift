import AppKit

// Formatting is presentation metadata. The engine continues to receive Markdown.
enum ComposerFormat: String, CaseIterable, Identifiable {
    case heading, bold, italic, bullets, checklist, inlineCode, codeBlock
    var id: Self { self }
    var title: String {
        switch self {
        case .heading: "Heading"
        case .bold: "Bold"
        case .italic: "Italic"
        case .bullets: "Bulleted list"
        case .checklist: "Checklist"
        case .inlineCode: "Inline code"
        case .codeBlock: "Code block"
        }
    }
    var symbol: String {
        switch self {
        case .heading: "textformat.size"
        case .bold: "bold"
        case .italic: "italic"
        case .bullets: "list.bullet"
        case .checklist: "checklist"
        case .inlineCode: "chevron.left.forwardslash.chevron.right"
        case .codeBlock: "curlybraces"
        }
    }
    var shortcut: String? {
        switch self {
        case .bold: "⌘B"
        case .italic: "⌘I"
        case .inlineCode: "⌘E"
        default: nil
        }
    }
    var inlineStyle: Int? {
        switch self {
        case .bold: 1
        case .italic: 2
        case .inlineCode: 4
        default: nil
        }
    }
    var block: String {
        switch self {
        case .heading: "#"
        case .bullets: "-"
        case .checklist: "- [ ]"
        case .codeBlock: "code:"
        default: ""
        }
    }
}

extension NSAttributedString.Key {
    static let composerInline = Self("SprowtInline")
    static let composerBlock = Self("SprowtBlock")
    static let composerCodeGroup = Self("SprowtCodeBlock")
    static let composerEscape = Self("SprowtEscape")
    static let composerOrigin = Self("SprowtSource")
}

private final class MarkdownOrigin: NSObject {
    let source: String
    let canonical: String
    init(source: String, canonical: String) { self.source = source; self.canonical = canonical }
}

@MainActor
enum ComposerMarkdown {
    private static let escapable = Set("!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~")
    static var baseFont: NSFont { NSFont(name: "Manrope", size: 14) ?? .systemFont(ofSize: 14) }
    static var attributes: [NSAttributedString.Key: Any] {
        [.font: baseFont, .foregroundColor: NSColor(white: 0.09, alpha: 1), .composerInline: 0, .composerBlock: ""]
    }

    static func decode(_ source: String) -> NSAttributedString {
        let result = NSMutableAttributedString(string: "")
        let lines = source.components(separatedBy: "\n")
        var index = 0
        while index < lines.count {
            let line = lines[index]
            let newline = index < lines.count - 1 ? "\n" : ""
            if let fence = fenceStart(line), let end = (index + 1..<lines.count).first(where: {
                let trimmed = lines[$0].trimmingCharacters(in: .whitespaces)
                return trimmed.count >= fence.count && trimmed.allSatisfy { $0 == "`" }
            }) {
                let raw = lines[index...end].joined(separator: "\n") + (end < lines.count - 1 ? "\n" : "")
                let body = lines[(index + 1)..<end].joined(separator: "\n") + "\n"
                let block = NSMutableAttributedString(string: body, attributes: attributes)
                block.addAttributes([.composerBlock: "code:" + String(line.dropFirst(fence.count)),
                                     .composerCodeGroup: UUID().uuidString], range: block.fullRange)
                preserve(raw, in: block)
                result.append(block)
                index = end + 1
                continue
            }
            let (kind, body) = blockPrefix(line)
            // Empty source blocks have no attributed range in NSTextView. Keep their syntax visible.
            let block = inline(body.isEmpty && !kind.isEmpty ? line : body)
            if !body.isEmpty {
                if let marker = marker(kind) { block.insert(NSAttributedString(string: marker, attributes: attributes), at: 0) }
                block.addAttribute(.composerBlock, value: kind, range: block.fullRange)
            }
            block.append(NSAttributedString(string: newline, attributes: attributes.merging([.composerBlock: body.isEmpty ? "" : kind]) { _, new in new }))
            preserve(line + newline, in: block)
            result.append(block)
            index += 1
        }
        style(result)
        return result
    }

    static func encode(_ text: NSAttributedString) -> String {
        var source = "", position = 0
        let string = text.string as NSString
        while position < text.length {
            var end = NSMaxRange(string.paragraphRange(for: NSRange(location: position, length: 0)))
            let kind = text.attribute(.composerBlock, at: position, effectiveRange: nil) as? String ?? ""
            if kind.hasPrefix("code:") {
                let group = text.attribute(.composerCodeGroup, at: position, effectiveRange: nil) as? String
                while end < text.length, (text.attribute(.composerBlock, at: end, effectiveRange: nil) as? String) == kind,
                      (text.attribute(.composerCodeGroup, at: end, effectiveRange: nil) as? String) == group {
                    end = NSMaxRange(string.paragraphRange(for: NSRange(location: end, length: 0)))
                }
            }
            let range = NSRange(location: position, length: end - position)
            let current = canonical(text.attributedSubstring(from: range))
            var originRange = NSRange()
            let origin = text.attribute(.composerOrigin, at: position, longestEffectiveRange: &originRange, in: text.fullRange) as? MarkdownOrigin
            if let origin, NSIntersectionRange(originRange, range) == range, current == origin.canonical {
                source += origin.source
            } else {
                source += current
            }
            position = end
        }
        return source
    }

    static func style(_ text: NSMutableAttributedString) {
        text.enumerateAttributes(in: text.fullRange) { attrs, range, _ in
            let block = attrs[.composerBlock] as? String ?? ""
            let bits = block.hasPrefix("code:") ? 0 : attrs[.composerInline] as? Int ?? 0
            var font = block.hasPrefix("code:") || bits & 4 != 0 ? NSFont.monospacedSystemFont(ofSize: 13, weight: .regular) : baseFont
            if block.hasPrefix("#") { font = NSFontManager.shared.convert(font, toSize: 20) }
            if bits & 1 != 0 || block.hasPrefix("#") { font = NSFontManager.shared.convert(font, toHaveTrait: .boldFontMask) }
            if bits & 2 != 0 { font = NSFontManager.shared.convert(font, toHaveTrait: .italicFontMask) }
            // Manrope has no italic face. Avoid adding a second slant to fonts that do.
            if bits & 2 != 0, !font.fontDescriptor.symbolicTraits.contains(.italic) {
                text.addAttribute(.obliqueness, value: 0.15, range: range)
            } else {
                text.removeAttribute(.obliqueness, range: range)
            }
            let paragraph = NSMutableParagraphStyle()
            paragraph.lineSpacing = 4
            if marker(block) != nil { paragraph.headIndent = 18 }
            text.addAttributes([.font: font, .paragraphStyle: paragraph, .foregroundColor: NSColor(white: 0.09, alpha: 1)], range: range)
            if bits & 4 != 0 || block.hasPrefix("code:") {
                text.addAttribute(.backgroundColor, value: NSColor(white: 0.93, alpha: 1), range: range)
            } else {
                text.removeAttribute(.backgroundColor, range: range)
            }
        }
    }

    static func escapeLiteralCharacters(in text: NSMutableAttributedString, range: NSRange) {
        let string = text.string as NSString
        let lineStart = string.paragraphRange(for: NSRange(location: range.location, length: 0)).location
        let prefix = string.substring(with: NSRange(location: lineStart, length: range.location - lineStart))
        var onlyWhitespace = prefix.allSatisfy { $0.isWhitespace }
        var position = range.location
        var pending: NSRange?
        func flush() {
            if let pending { text.addAttribute(.composerEscape, value: true, range: pending) }
            pending = nil
        }
        for character in string.substring(with: range) {
            let length = String(character).utf16.count
            if "\\*_`[]<>~&#".contains(character) || character == "-" && onlyWhitespace {
                if var run = pending { run.length += length; pending = run }
                else { pending = NSRange(location: position, length: length) }
            } else { flush() }
            if character.isNewline { onlyWhitespace = true }
            else if !character.isWhitespace { onlyWhitespace = false }
            position += length
        }
        flush()
    }

    static func marker(_ kind: String) -> String? {
        switch kind {
        case "-": "• "
        case "- [ ]": "☐ "
        case "- [x]", "- [X]": "☑ "
        default: nil
        }
    }

    static func safeTicks(_ text: String, minimum: Int) -> String {
        var longest = 0, run = 0
        for character in text { run = character == "`" ? run + 1 : 0; longest = max(longest, run) }
        return String(repeating: "`", count: max(minimum, longest + 1))
    }

    private static func preserve(_ source: String, in block: NSMutableAttributedString) {
        guard block.length > 0 else { return }
        block.addAttribute(.composerOrigin, value: MarkdownOrigin(source: source, canonical: canonical(block)), range: block.fullRange)
    }

    private static func blockPrefix(_ line: String) -> (String, String) {
        for prefix in ["- [ ] ", "- [x] ", "- [X] ", "### ", "## ", "# ", "- "] where line.hasPrefix(prefix) {
            return (String(prefix.dropLast()), String(line.dropFirst(prefix.count)))
        }
        return ("", line)
    }

    private static func fenceStart(_ line: String) -> String? {
        let ticks = String(line.prefix { $0 == "`" })
        guard ticks.count >= 3, !line.dropFirst(ticks.count).contains("`") else { return nil }
        return ticks
    }

    private static func inline(_ source: String) -> NSMutableAttributedString {
        let literal = NSMutableAttributedString(string: source, attributes: attributes)
        guard let parsed = try? AttributedString(markdown: source, options: .init(
            interpretedSyntax: .inlineOnlyPreservingWhitespace, appliesSourcePositionAttributes: true)) else { return literal }
        let result = NSMutableAttributedString(string: "")
        for run in parsed.runs {
            let intent = run.inlinePresentationIntent ?? []
            let supported: InlinePresentationIntent = [.stronglyEmphasized, .emphasized, .code]
            // Unsupported Markdown stays literal so links, HTML and other source are never erased.
            guard run.link == nil, run.imageURL == nil, intent.subtracting(supported).isEmpty else { return literal }
            let value = String(parsed[run.range].characters)
            var attrs = attributes
            attrs[.composerInline] = (intent.contains(.stronglyEmphasized) ? 1 : 0)
                | (intent.contains(.emphasized) ? 2 : 0) | (intent.contains(.code) ? 4 : 0)
            if intent.contains(.code) {
                result.append(NSAttributedString(string: value, attributes: attrs))
                continue
            }
            guard let position = run.markdownSourcePosition, let range = Range<String.Index>(position, in: source) else { return literal }
            let raw = source[range]
            var decodedString = "", escaped: [NSRange] = []
            decodedString.reserveCapacity(raw.utf8.count)
            var rest = raw, offset = 0
            while let character = rest.first {
                if character == "\\", let next = rest.dropFirst().first, escapable.contains(next) {
                    let length = String(next).utf16.count
                    decodedString.append(next)
                    if var last = escaped.last, NSMaxRange(last) == offset {
                        last.length += length
                        escaped[escaped.count - 1] = last
                    } else { escaped.append(NSRange(location: offset, length: length)) }
                    offset += length
                    rest = rest.dropFirst(2)
                } else {
                    decodedString.append(character)
                    offset += String(character).utf16.count
                    rest = rest.dropFirst()
                }
            }
            guard decodedString == value else { return literal }
            let decoded = NSMutableAttributedString(string: decodedString, attributes: attrs)
            for range in escaped { decoded.addAttribute(.composerEscape, value: true, range: range) }
            result.append(decoded)
        }
        return result
    }

    private static func emphasisWhitespace(_ source: NSAttributedString) -> NSAttributedString {
        let text = NSMutableAttributedString(attributedString: source)
        let string = source.string as NSString
        var position = 0
        while position < text.length {
            let paragraph = string.paragraphRange(for: NSRange(location: position, length: 0))
            for bit in [1, 2] {
                var groups: [NSRange] = []
                text.enumerateAttribute(.composerInline, in: paragraph) { value, range, _ in
                    guard (value as? Int ?? 0) & bit != 0 else { return }
                    if let last = groups.last, NSMaxRange(last) == range.location {
                        groups[groups.count - 1].length += range.length
                    } else { groups.append(range) }
                }
                for group in groups {
                    let value = string.substring(with: group)
                    let leading = (String(value.prefix { $0.isWhitespace }) as NSString).length
                    let trailing = (String(value.reversed().prefix { $0.isWhitespace }) as NSString).length
                    let edges = [NSRange(location: group.location, length: leading),
                                 NSRange(location: NSMaxRange(group) - trailing, length: trailing)]
                    for edge in edges where edge.length > 0 {
                        text.enumerateAttribute(.composerInline, in: edge) { value, range, _ in
                            let bits = value as? Int ?? 0
                            if bits & 4 == 0 { text.addAttribute(.composerInline, value: bits & ~bit, range: range) }
                        }
                    }
                }
            }
            position = NSMaxRange(paragraph)
        }
        return text
    }

    private static func canonical(_ source: NSAttributedString) -> String {
        let text = emphasisWhitespace(source)
        let string = text.string as NSString
        var position = 0, output = ""
        while position < text.length {
            let paragraph = string.paragraphRange(for: NSRange(location: position, length: 0))
            let kind = text.attribute(.composerBlock, at: position, effectiveRange: nil) as? String ?? ""
            if kind.hasPrefix("code:") {
                var end = NSMaxRange(paragraph)
                let group = text.attribute(.composerCodeGroup, at: position, effectiveRange: nil) as? String
                while end < text.length, (text.attribute(.composerBlock, at: end, effectiveRange: nil) as? String) == kind,
                      (text.attribute(.composerCodeGroup, at: end, effectiveRange: nil) as? String) == group {
                    end = NSMaxRange(string.paragraphRange(for: NSRange(location: end, length: 0)))
                }
                let body = string.substring(with: NSRange(location: position, length: end - position))
                let ticks = safeTicks(body, minimum: 3)
                output += ticks + String(kind.dropFirst(5)) + "\n" + body + (body.hasSuffix("\n") ? "" : "\n") + ticks
                if end < text.length || body.hasSuffix("\n") { output += "\n" }
                position = end
                continue
            }
            var start = position
            if let marker = marker(kind), string.substring(with: paragraph).hasPrefix(marker) { start += (marker as NSString).length }
            let endsLine = string.character(at: NSMaxRange(paragraph) - 1) == 10
            let end = NSMaxRange(paragraph) - (endsLine ? 1 : 0)
            if !kind.isEmpty { output += kind + " " }
            let range = NSRange(location: min(start, end), length: max(0, end - start))
            var emphasis: [String] = []
            text.enumerateAttribute(.composerInline, in: range) { style, run, _ in
                var value = string.substring(with: run)
                let bits = style as? Int ?? 0
                if bits & 4 != 0 {
                    let ticks = safeTicks(value, minimum: 1)
                    let pad = value.hasPrefix("`") || value.hasSuffix("`") || value.hasPrefix(" ") && value.hasSuffix(" ") && value.contains(where: { $0 != " " })
                    value = ticks + (pad ? " " : "") + value + (pad ? " " : "") + ticks
                } else {
                    var escaped = ""
                    text.enumerateAttribute(.composerEscape, in: run) { escape, part, _ in
                        for character in string.substring(with: part) {
                            if escape as? Bool == true || bits != 0 && "\\*_`".contains(character) { escaped += "\\" }
                            escaped.append(character)
                        }
                    }
                    value = escaped
                }
                let target = (bits & 1 != 0 ? ["**"] : []) + (bits & 2 != 0 ? ["*"] : [])
                let common = zip(emphasis, target).prefix { $0 == $1 }.count
                output += emphasis.dropFirst(common).reversed().joined()
                output += target.dropFirst(common).joined()
                emphasis = target
                output += value
            }
            output += emphasis.reversed().joined()
            if endsLine { output += "\n" }
            position = NSMaxRange(paragraph)
        }
        return output
    }
}

extension NSAttributedString {
    var fullRange: NSRange { NSRange(location: 0, length: length) }
}
